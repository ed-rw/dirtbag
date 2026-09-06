//! Start and track the detached `tart run` process.

use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use nix::sys::signal::kill;
use nix::unistd::Pid;

/// Spawn `tart_bin` detached from the current session. Write stdout and stderr
/// to `log_path`. Return the child PID.
pub fn spawn_detached(tart_bin: &Path, args: &[String], log_path: &Path) -> Result<u32> {
    let log =
        File::create(log_path).with_context(|| format!("creating log {}", log_path.display()))?;
    let log_err = log.try_clone().context("duplicating log handle")?;

    let mut cmd = Command::new(tart_bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));

    // Put the VM in a new session (`setsid`) so it survives after dirtbag exits
    // and a terminal hangup or Ctrl-C does not reach it.
    //
    // SAFETY: `pre_exec` runs the closure in the child, after `fork` and before
    // `exec`. In that window the child must use only async-signal-safe code.
    // `fork` copies one thread, so a lock that another thread held is now stuck,
    // and any allocation or lock can deadlock the child. This closure is safe:
    // it makes one `setsid` syscall, and on error it calls `from_raw_os_error`,
    // which wraps the errno value and does not allocate.
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

/// Return true if a process with `pid` exists.
pub fn is_alive(pid: u32) -> bool {
    kill(Pid::from_raw(pid as i32), None).is_ok()
}
