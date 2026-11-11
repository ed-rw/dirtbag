//! The hidden `__supervise` command: run `tart run` as a child and forward
//! host ports to the guest while it runs.
//!
//! `up` starts it detached. It exits when its child exits, so a `tart stop`
//! also releases the host ports.

use std::net::{IpAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use nix::sys::resource::{Resource, getrlimit, setrlimit};
use tracing::{error, info, warn};

use crate::forward::{self, Forward};
use crate::tart::Tart;

pub fn run(vm: &str, forwards: &[Forward], child: &[String]) -> Result<ExitCode> {
    let (program, args) = child.split_first().context("no command to supervise")?;
    raise_open_file_limit();

    // Bind before the VM starts. A busy port then stops the boot, and `up`
    // reports it from the run log. Bind an address that the VM network
    // creates after the VM starts.
    let mut listeners = Vec::new();
    let mut deferred = Vec::new();
    for f in forwards {
        match TcpListener::bind(f.host_addr()) {
            Ok(listener) => listeners.push((listener, *f)),
            Err(e) if forward::is_not_available_yet(&e) => deferred.push(*f),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("binding {} for forward `{f}`", f.host_addr()));
            }
        }
    }

    let mut child = Command::new(program)
        .args(args)
        .spawn()
        .with_context(|| format!("spawning {program}"))?;

    let tart = || Tart::at(PathBuf::from(program));
    let guest_ip = guest_ip_lookup(tart(), vm.to_string());
    for (listener, f) in listeners {
        start_forward(listener, f, guest_ip.clone());
    }
    for f in deferred {
        let (tart, vm, guest_ip) = (tart(), vm.to_string(), guest_ip.clone());
        std::thread::spawn(
            move || match forward::bind_when_available(&f, DEFERRED_BIND_TIMEOUT) {
                Ok(listener) => start_forward(listener, f, guest_ip),
                // Fail as a busy port before the boot does. `up` sees that the
                // supervisor exited and points to the run log.
                Err(e) => {
                    error!("{e:#}; stopping the VM");
                    if let Err(e) = tart.stop(&vm) {
                        error!("cannot stop the VM: {e}");
                    }
                }
            },
        );
    }

    let status = child.wait().context("waiting for the VM process")?;
    info!(%status, "VM process exited; stopping forwards");
    let code = status
        .code()
        .and_then(|c| u8::try_from(c).ok())
        .unwrap_or(1);
    Ok(ExitCode::from(code))
}

/// The maximum time from `tart run` until the VM network interface exists.
const DEFERRED_BIND_TIMEOUT: Duration = Duration::from_secs(60);

fn start_forward<F>(listener: TcpListener, f: Forward, guest_ip: F)
where
    F: Fn(bool) -> Option<IpAddr> + Clone + Send + 'static,
{
    info!(forward = %forward::label(&f), "forwarding");
    std::thread::spawn(move || forward::serve(listener, f.guest, guest_ip));
}

/// macOS refuses a soft limit above `OPEN_MAX` when the hard limit is
/// unlimited.
const OPEN_MAX: u64 = 10240;

/// Each relayed connection holds four file descriptors. Raise the soft limit
/// from the macOS default of 256 to the hard limit, so that many connections
/// can be open at the same time.
fn raise_open_file_limit() {
    let result = getrlimit(Resource::RLIMIT_NOFILE).and_then(|(soft, hard)| {
        if soft >= hard {
            return Ok(());
        }
        setrlimit(Resource::RLIMIT_NOFILE, hard, hard)
            .or_else(|_| setrlimit(Resource::RLIMIT_NOFILE, OPEN_MAX.min(hard).max(soft), hard))
    });
    match result {
        Ok(()) => info!(limit = ?getrlimit(Resource::RLIMIT_NOFILE).ok(), "open file limit"),
        Err(e) => warn!("cannot raise the open file limit: {e}"),
    }
}

/// A guest IP lookup for [`forward::serve`]. Keep the last IP, and ask tart
/// again when no IP is known or the caller asks for a fresh one.
fn guest_ip_lookup(
    tart: Tart,
    vm: String,
) -> impl Fn(bool) -> Option<IpAddr> + Clone + Send + 'static {
    let cache: Arc<Mutex<Option<IpAddr>>> = Arc::default();
    move |fresh| {
        let mut cached = cache.lock().unwrap();
        if fresh || cached.is_none() {
            *cached = tart.ip(&vm).ok().flatten().and_then(|ip| ip.parse().ok());
        }
        *cached
    }
}
