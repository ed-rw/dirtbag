//! Start and track the detached VM process.

use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use nix::sys::signal::kill;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

/// Spawn `program` detached from the current session. Write stdout and stderr
/// to `log_path`. Return the child PID.
pub fn spawn_detached(program: &Path, args: &[String], log_path: &Path) -> Result<u32> {
    let log =
        File::create(log_path).with_context(|| format!("creating log {}", log_path.display()))?;
    let log_err = log.try_clone().context("duplicating log handle")?;

    let mut cmd = Command::new(program);
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
        .with_context(|| format!("spawning {}", program.display()))?;
    Ok(child.id())
}

/// Return true if a process with `pid` exists. Reap it if it is an exited
/// child of this process. Otherwise it stays as a zombie, which `kill` still
/// finds.
pub fn is_alive(pid: u32) -> bool {
    let pid = Pid::from_raw(pid as i32);
    match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::StillAlive) => true,
        Ok(_) => false,
        Err(_) => kill(pid, None).is_ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::zombie_processes, reason = "is_alive reaps the child")]
    fn an_exited_child_is_not_alive() {
        let child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        let start = std::time::Instant::now();
        while is_alive(pid) {
            assert!(start.elapsed().as_secs() < 5, "child {pid} still alive");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(is_alive(std::process::id()));
    }
}
