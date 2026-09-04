//! Spawning and tracking the detached `tart run` process.
//!
//! `tart run` runs in the foreground for the life of the VM, so dirtbag starts
//! it in its own session (`setsid`) with stdio redirected to a log file, then
//! records the PID in state. The process outlives the `dirtbag up` invocation.

use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

/// Spawn `tart_bin args...` detached from the current session, with stdout and
/// stderr appended to `log_path`. Returns the child PID.
pub fn spawn_detached(tart_bin: &Path, args: &[String], log_path: &Path) -> Result<u32> {
    let log = File::create(log_path)
        .with_context(|| format!("creating log {}", log_path.display()))?;
    let log_err = log.try_clone().context("duplicating log handle")?;

    let mut cmd = Command::new(tart_bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));

    // Detach into a new session so the VM survives `dirtbag` exiting.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
        });
    }

    let child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", tart_bin.display()))?;
    Ok(child.id())
}

/// Whether a process with `pid` currently exists.
pub fn is_alive(pid: u32) -> bool {
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

/// Send SIGTERM to `pid` if it is still alive. No-op otherwise.
pub fn terminate(pid: u32) {
    let p = Pid::from_raw(pid as i32);
    if kill(p, None).is_ok() {
        let _ = kill(p, Signal::SIGTERM);
    }
}
