//! SSH access to the guest via libssh2 (`ssh2`).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ssh2::Session;

pub const SSH_PORT: u16 = 22;

pub struct Ssh {
    session: Session,
}

impl Ssh {
    /// Open an authenticated session to `host:port`.
    pub fn connect(host: &str, port: u16, user: &str, password: &str) -> Result<Self> {
        let addr = (host, port)
            .to_socket_addrs()
            .with_context(|| format!("resolving {host}:{port}"))?
            .next()
            .with_context(|| format!("no address for {host}:{port}"))?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
            .with_context(|| format!("connecting to {host}:{port}"))?;

        let mut session = Session::new().context("creating ssh session")?;
        session.set_tcp_stream(tcp);
        session.handshake().context("ssh handshake")?;
        // Ask libssh2 for keepalives so a quiet channel does not stall behind a
        // NAT idle timeout during a long `exec` step.
        session.set_keepalive(true, 15);
        session
            .userauth_password(user, password)
            .context("ssh password authentication")?;
        if !session.authenticated() {
            bail!("ssh authentication failed for user `{user}`");
        }
        Ok(Self { session })
    }

    /// Retry [`Ssh::connect`] until it succeeds or `timeout` elapses. Use it to
    /// wait for a new VM to accept logins.
    pub fn connect_ready(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        timeout: Duration,
    ) -> Result<Self> {
        let start = Instant::now();
        loop {
            match Self::connect(host, port, user, password) {
                Ok(ssh) => return Ok(ssh),
                Err(e) => {
                    if start.elapsed() > timeout {
                        return Err(e).with_context(|| {
                            format!("ssh not ready after {}s", timeout.as_secs())
                        });
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    /// Run `command`. Send stdout and stderr to the terminal. Return the remote
    /// exit status.
    pub fn exec_streaming(&self, command: &str) -> Result<i32> {
        let mut channel = self.session.channel_session().context("opening channel")?;
        channel.exec(command).context("exec")?;

        let mut buf = [0u8; 8192];
        let mut stdout = std::io::stdout();
        loop {
            let n = channel.read(&mut buf).context("reading stdout")?;
            if n == 0 {
                break;
            }
            stdout.write_all(&buf[..n])?;
            stdout.flush()?;
        }

        let mut err = Vec::new();
        channel
            .stderr()
            .read_to_end(&mut err)
            .context("reading stderr")?;
        std::io::stderr().write_all(&err)?;

        channel.wait_close().context("closing channel")?;
        Ok(channel.exit_status()?)
    }

    /// Run `command`. Capture stdout and stderr together. Return
    /// `(exit_status, output)`.
    pub fn exec_capture(&self, command: &str) -> Result<(i32, String)> {
        let mut channel = self.session.channel_session().context("opening channel")?;
        channel.exec(command).context("exec")?;
        let mut out = String::new();
        channel.read_to_string(&mut out).context("reading stdout")?;
        let mut err = String::new();
        channel
            .stderr()
            .read_to_string(&mut err)
            .context("reading stderr")?;
        channel.wait_close().context("closing channel")?;
        out.push_str(&err);
        Ok((channel.exit_status()?, out))
    }

    /// Write `contents` to `remote` on the guest via SCP.
    pub fn upload(&self, contents: &[u8], remote: &str, mode: i32) -> Result<()> {
        let mut ch = self
            .session
            .scp_send(
                std::path::Path::new(remote),
                mode,
                contents.len() as u64,
                None,
            )
            .with_context(|| format!("scp to {remote}"))?;
        ch.write_all(contents)?;
        ch.send_eof()?;
        ch.wait_eof()?;
        ch.close()?;
        ch.wait_close()?;
        Ok(())
    }

}

/// Open an interactive shell on the guest through the system `ssh` client.
///
/// The client owns the terminal, so the session gets a real PTY with correct
/// window resize and flow control. Return the remote exit status.
pub fn interactive_shell(host: &str, port: u16, user: &str, password: &str) -> Result<i32> {
    let askpass = AskpassScript::create()?;
    let target = format!("{user}@{host}");

    // The VMs are disposable and reuse IP addresses, so their host keys churn.
    // Skip the host-key check and keep the churn out of the user's known_hosts.
    // `ServerAliveInterval` holds the link open through a quiet stretch, such as
    // a long build or an idle editor.
    let status = Command::new("ssh")
        .arg("-t")
        .args(["-p", &port.to_string()])
        .args(["-o", "StrictHostKeyChecking=no"])
        .args(["-o", "UserKnownHostsFile=/dev/null"])
        .args(["-o", "GlobalKnownHostsFile=/dev/null"])
        .args(["-o", "LogLevel=ERROR"])
        .args(["-o", "ServerAliveInterval=15"])
        .args(["-o", "ServerAliveCountMax=3"])
        .args(["-o", "NumberOfPasswordPrompts=1"])
        .arg(&target)
        // `SSH_ASKPASS_REQUIRE=force` makes ssh read the password from the helper
        // even with a terminal attached. The helper reads it from the
        // environment, so the password never lands in a file or the argument
        // list. Needs OpenSSH 8.4 or later.
        .env("SSH_ASKPASS", askpass.path())
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DIRTBAG_SSH_PASSWORD", password)
        .status()
        .context("running ssh")?;

    Ok(status.code().unwrap_or(1))
}

/// A temporary askpass helper script. The [`Drop`] removes it.
struct AskpassScript {
    path: PathBuf,
}

impl AskpassScript {
    fn create() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("dirtbag-askpass-{}.sh", std::process::id()));
        std::fs::write(&path, "#!/bin/sh\nprintf '%s\\n' \"$DIRTBAG_SSH_PASSWORD\"\n")
            .with_context(|| format!("writing askpass helper to {}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .context("setting askpass helper mode")?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AskpassScript {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
