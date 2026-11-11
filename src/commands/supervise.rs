//! The hidden `__supervise` command: run `tart run` as a child and forward
//! host ports to the guest while it runs.
//!
//! `up` starts it detached. It exits when its child exits, so a `tart stop`
//! also releases the host ports.

use std::net::{IpAddr, TcpListener};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tracing::info;

use crate::forward::{self, Forward};
use crate::tart::Tart;

pub fn run(vm: &str, forwards: &[Forward], child: &[String]) -> Result<ExitCode> {
    let (program, args) = child.split_first().context("no command to supervise")?;

    // Bind before the VM starts. A busy port then stops the boot, and `up`
    // reports it from the run log.
    let listeners = forwards
        .iter()
        .map(|f| {
            TcpListener::bind(f.host_addr())
                .with_context(|| format!("binding {} for forward `{f}`", f.host_addr()))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut child = Command::new(program)
        .args(args)
        .spawn()
        .with_context(|| format!("spawning {program}"))?;

    let guest_ip = guest_ip_lookup(Tart::at(PathBuf::from(program)), vm.to_string());
    for (listener, f) in listeners.into_iter().zip(forwards) {
        info!(forward = %forward::label(f), "forwarding");
        let guest_ip = guest_ip.clone();
        let guest = f.guest;
        std::thread::spawn(move || forward::serve(listener, guest, guest_ip));
    }

    let status = child.wait().context("waiting for the VM process")?;
    info!(%status, "VM process exited; stopping forwards");
    let code = status
        .code()
        .and_then(|c| u8::try_from(c).ok())
        .unwrap_or(1);
    Ok(ExitCode::from(code))
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
