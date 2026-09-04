//! SSH access to the guest via libssh2 (`ssh2`).
//!
//! One mechanism handles everything: a readiness probe used by `up`, streaming
//! command execution for `ssh -- CMD` and provisioning, and an interactive PTY
//! shell. Authentication is by password (Tart's `admin`/`admin` by default).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
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
        session
            .userauth_password(user, password)
            .context("ssh password authentication")?;
        if !session.authenticated() {
            bail!("ssh authentication failed for user `{user}`");
        }
        Ok(Self { session })
    }

    /// Retry [`Ssh::connect`] until it succeeds or `timeout` elapses. Used to
    /// wait for a freshly booted VM to accept logins.
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

    /// Run `command`, streaming stdout/stderr to the terminal. Returns the
    /// remote exit status.
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
        channel.stderr().read_to_end(&mut err).context("reading stderr")?;
        std::io::stderr().write_all(&err)?;

        channel.wait_close().context("closing channel")?;
        Ok(channel.exit_status()?)
    }

    /// Run `command`, capturing combined stdout+stderr. Returns
    /// `(exit_status, output)`.
    pub fn exec_capture(&self, command: &str) -> Result<(i32, String)> {
        let mut channel = self.session.channel_session().context("opening channel")?;
        channel.exec(command).context("exec")?;
        let mut out = String::new();
        channel.read_to_string(&mut out).context("reading stdout")?;
        let mut err = String::new();
        channel.stderr().read_to_string(&mut err).context("reading stderr")?;
        channel.wait_close().context("closing channel")?;
        out.push_str(&err);
        Ok((channel.exit_status()?, out))
    }

    /// Write `contents` to `remote` on the guest via SCP.
    pub fn upload(&self, contents: &[u8], remote: &str, mode: i32) -> Result<()> {
        let mut ch = self
            .session
            .scp_send(std::path::Path::new(remote), mode, contents.len() as u64, None)
            .with_context(|| format!("scp to {remote}"))?;
        ch.write_all(contents)?;
        ch.send_eof()?;
        ch.wait_eof()?;
        ch.close()?;
        ch.wait_close()?;
        Ok(())
    }

    /// Open an interactive PTY shell wired to the local terminal. Returns the
    /// shell's exit status.
    pub fn shell(&self) -> Result<i32> {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let mut channel = self.session.channel_session().context("opening channel")?;
        channel
            .request_pty(
                "xterm-256color",
                None,
                Some((cols as u32, rows as u32, 0, 0)),
            )
            .context("requesting pty")?;
        channel.shell().context("starting shell")?;

        let _raw = RawMode::enable()?;
        let _nb = NonBlockingStdin::enable();
        self.session.set_blocking(false);

        let mut chan_buf = [0u8; 8192];
        let mut in_buf = [0u8; 8192];
        let mut stdout = std::io::stdout();
        let mut stdin = std::io::stdin();
        let (mut last_cols, mut last_rows) = (cols, rows);

        let status = loop {
            // Guest -> local terminal.
            match channel.read(&mut chan_buf) {
                Ok(0) => {
                    if channel.eof() {
                        break channel.exit_status().unwrap_or(0);
                    }
                }
                Ok(n) => {
                    stdout.write_all(&chan_buf[..n])?;
                    stdout.flush()?;
                    continue; // drain the guest before sleeping
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e).context("reading from guest"),
            }

            // Local keyboard -> guest.
            match stdin.read(&mut in_buf) {
                Ok(0) => {}
                Ok(n) => {
                    self.session.set_blocking(true);
                    channel.write_all(&in_buf[..n])?;
                    channel.flush()?;
                    self.session.set_blocking(false);
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e).context("reading stdin"),
            }

            // Propagate terminal resizes.
            if let Ok((c, r)) = crossterm::terminal::size() {
                if (c, r) != (last_cols, last_rows) {
                    self.session.set_blocking(true);
                    let _ = channel.request_pty_size(c as u32, r as u32, None, None);
                    self.session.set_blocking(false);
                    last_cols = c;
                    last_rows = r;
                }
            }

            std::thread::sleep(Duration::from_millis(10));
        };

        self.session.set_blocking(true);
        Ok(status)
    }
}

/// RAII guard that puts the terminal in raw mode and restores it on drop.
struct RawMode;

impl RawMode {
    fn enable() -> Result<Self> {
        crossterm::terminal::enable_raw_mode().context("enabling raw terminal mode")?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// RAII guard that makes stdin non-blocking and restores its flags on drop.
struct NonBlockingStdin {
    fd: i32,
    prev: Option<nix::fcntl::OFlag>,
}

impl NonBlockingStdin {
    fn enable() -> Self {
        use nix::fcntl::{fcntl, FcntlArg, OFlag};
        let fd = std::io::stdin().as_raw_fd();
        let prev = fcntl(fd, FcntlArg::F_GETFL)
            .ok()
            .map(OFlag::from_bits_truncate);
        if let Some(flags) = prev {
            let _ = fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK));
        }
        Self { fd, prev }
    }
}

impl Drop for NonBlockingStdin {
    fn drop(&mut self) {
        if let Some(flags) = self.prev {
            let _ = nix::fcntl::fcntl(self.fd, nix::fcntl::FcntlArg::F_SETFL(flags));
        }
    }
}
