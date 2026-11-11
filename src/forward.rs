//! TCP port forwarding from the host to the guest.
//!
//! The supervisor process (see `commands::supervise`) owns the listeners and
//! relays each connection to the guest. Its command line holds the active
//! forwards, so dirtbag reads them back from `ps` and keeps no state file.

use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::process::Command;
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// The hidden subcommand name of the supervisor process.
pub const SUPERVISE: &str = "__supervise";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const GUEST_IP_TIMEOUT: Duration = Duration::from_secs(30);
const GUEST_IP_POLL: Duration = Duration::from_secs(1);
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);
const BIND_RETRY_DELAY: Duration = Duration::from_millis(200);
const KEEPALIVE_IDLE_SECS: u32 = 60;
const KEEPALIVE_INTERVAL_SECS: u32 = 10;
const KEEPALIVE_PROBES: u32 = 6;

/// One forward: `[bind:]host:guest`. A `host` port of 0 means "pick a free
/// port" until [`reserve`] replaces it with a real one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Forward {
    pub bind: IpAddr,
    pub host: u16,
    pub guest: u16,
}

impl Forward {
    pub fn host_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind, self.host)
    }

    /// True if `running` can serve this config entry. An auto port (`host` 0)
    /// matches any host port.
    pub fn matches(&self, running: &Forward) -> bool {
        self.bind == running.bind
            && self.guest == running.guest
            && (self.host == 0 || self.host == running.host)
    }
}

impl FromStr for Forward {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = |why: &str| format!("forward `{s}` {why}; expected `[bind:]host:guest`");
        let mut parts = s.rsplitn(3, ':');
        let guest = parts.next().ok_or_else(|| bad("is empty"))?;
        let host = parts.next().ok_or_else(|| bad("has no host port"))?;
        let bind = match parts.next() {
            Some(b) => b
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse()
                .map_err(|_| bad("has an invalid bind address"))?,
            None => IpAddr::V4(Ipv4Addr::LOCALHOST),
        };
        let host: u16 = host.parse().map_err(|_| bad("has an invalid host port"))?;
        let guest: u16 = guest
            .parse()
            .map_err(|_| bad("has an invalid guest port"))?;
        if guest == 0 {
            return Err(bad("must not use guest port 0"));
        }
        Ok(Self { bind, host, guest })
    }
}

impl TryFrom<String> for Forward {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Forward> for String {
    fn from(f: Forward) -> Self {
        f.to_string()
    }
}

/// The full `bind:host:guest` form. [`FromStr`] reads it back.
impl fmt::Display for Forward {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.bind {
            IpAddr::V4(ip) => write!(f, "{ip}:{}:{}", self.host, self.guest),
            IpAddr::V6(ip) => write!(f, "[{ip}]:{}:{}", self.host, self.guest),
        }
    }
}

/// Reject two forwards that use the same fixed host address.
pub fn validate(forwards: &[Forward]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for f in forwards.iter().filter(|f| f.host != 0) {
        if !seen.insert(f.host_addr()) {
            bail!("duplicate forward host address {}", f.host_addr());
        }
    }
    Ok(())
}

/// Check that each fixed host port is free, and give each auto port a free
/// port. Return the forwards with real host ports. The ports are released
/// again, so the supervisor can bind them.
///
/// A bind address that does not exist yet is not an error. The supervisor
/// binds it after the boot (see [`bind_when_available`]). Its fixed port is
/// not checked, and its auto port is a port that is free on all addresses.
pub fn reserve(forwards: &[Forward]) -> Result<Vec<Forward>> {
    forwards
        .iter()
        .map(|f| {
            let probe = match TcpListener::bind(f.host_addr()) {
                Err(e) if is_not_available_yet(&e) && f.host == 0 => {
                    debug!(forward = %f, "bind address does not exist yet");
                    TcpListener::bind(SocketAddr::new(unspecified(f.bind), 0))
                }
                Err(e) if is_not_available_yet(&e) => {
                    debug!(forward = %f, "bind address does not exist yet");
                    return Ok(*f);
                }
                result => result,
            };
            let host = probe
                .with_context(|| {
                    format!(
                        "host port {} for forward `{f}` is not available",
                        f.host_addr()
                    )
                })?
                .local_addr()?
                .port();
            Ok(Forward { host, ..*f })
        })
        .collect()
}

/// True if a bind failed because the address does not exist on the host yet.
/// macOS creates the VM network interface, such as bridge100 at 192.168.64.1,
/// when the first VM starts, and removes it after the last VM stops.
pub fn is_not_available_yet(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::AddrNotAvailable
}

fn unspecified(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
        IpAddr::V6(_) => std::net::Ipv6Addr::UNSPECIFIED.into(),
    }
}

/// Bind `f`. While its address does not exist on the host, try again until
/// `timeout`. Any other error stops at once.
pub fn bind_when_available(f: &Forward, timeout: Duration) -> Result<TcpListener> {
    let start = Instant::now();
    loop {
        match TcpListener::bind(f.host_addr()) {
            Err(e) if is_not_available_yet(&e) && start.elapsed() < timeout => {
                std::thread::sleep(BIND_RETRY_DELAY);
            }
            result => {
                return result
                    .with_context(|| format!("binding {} for forward `{f}`", f.host_addr()));
            }
        }
    }
}

/// True if a listener accepts connections on the host address of `f`.
pub fn is_listening(f: &Forward) -> bool {
    let addr = match f.bind {
        ip if ip.is_unspecified() => SocketAddr::new(loopback(ip), f.host),
        _ => f.host_addr(),
    };
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

fn loopback(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
        IpAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
    }
}

/// Accept connections on `listener` and relay each one to `guest` on the IP
/// that `guest_ip` returns. `guest_ip(true)` must look the IP up again.
/// `guest_ip(false)` can return a cached IP. Run until the listener fails.
pub fn serve<F>(listener: TcpListener, guest: u16, guest_ip: F)
where
    F: Fn(bool) -> Option<IpAddr> + Clone + Send + 'static,
{
    for conn in listener.incoming() {
        let client = match conn {
            Ok(c) => c,
            Err(e) => {
                // An error such as EMFILE stays until a connection closes.
                // Wait, so that the loop does not spin.
                warn!("accept failed: {e}");
                std::thread::sleep(ACCEPT_RETRY_DELAY);
                continue;
            }
        };
        let guest_ip = guest_ip.clone();
        std::thread::spawn(move || {
            if let Err(e) = connect_and_relay(client, guest, guest_ip) {
                debug!("forward to guest port {guest} ended: {e:#}");
            }
        });
    }
}

fn connect_and_relay<F>(client: TcpStream, guest: u16, guest_ip: F) -> Result<()>
where
    F: Fn(bool) -> Option<IpAddr>,
{
    let upstream = connect_guest(guest, &guest_ip)?;
    for stream in [&client, &upstream] {
        enable_keepalive(stream).context("enabling TCP keepalive")?;
    }
    relay(client, upstream)?;
    Ok(())
}

/// Send keepalive probes on an idle connection. A guest that stops or
/// changes IP then closes the relay, and its threads and sockets go away.
fn enable_keepalive(stream: &TcpStream) -> nix::Result<()> {
    use nix::sys::socket::{setsockopt, sockopt};
    setsockopt(stream, sockopt::KeepAlive, &true)?;
    setsockopt(stream, sockopt::TcpKeepAlive, &KEEPALIVE_IDLE_SECS)?;
    setsockopt(stream, sockopt::TcpKeepInterval, &KEEPALIVE_INTERVAL_SECS)?;
    setsockopt(stream, sockopt::TcpKeepCount, &KEEPALIVE_PROBES)
}

/// Connect to the guest port. Wait for the guest to get an IP, and look the IP
/// up again once if the first connect fails, because a DHCP renewal can change
/// it.
fn connect_guest<F>(guest: u16, guest_ip: &F) -> Result<TcpStream>
where
    F: Fn(bool) -> Option<IpAddr>,
{
    let mut last_err = None;
    for fresh in [false, true] {
        let ip = wait_for(guest_ip, fresh).context("guest has no IP")?;
        match TcpStream::connect_timeout(&SocketAddr::new(ip, guest), CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("the loop runs at least once"))
        .with_context(|| format!("connecting to guest port {guest}"))
}

fn wait_for<F>(guest_ip: &F, fresh: bool) -> Option<IpAddr>
where
    F: Fn(bool) -> Option<IpAddr>,
{
    let start = Instant::now();
    let mut fresh = fresh;
    loop {
        if let Some(ip) = guest_ip(fresh) {
            return Some(ip);
        }
        fresh = true;
        if start.elapsed() > GUEST_IP_TIMEOUT {
            return None;
        }
        std::thread::sleep(GUEST_IP_POLL);
    }
}

/// Copy bytes both ways until both sides close. When one direction reaches
/// EOF, shut down the write half of the other socket, so half-closed
/// protocols work.
fn relay(a: TcpStream, b: TcpStream) -> io::Result<()> {
    let (mut a_read, mut b_write) = (a.try_clone()?, b.try_clone()?);
    let upstream = std::thread::spawn(move || {
        let _ = io::copy(&mut a_read, &mut b_write);
        let _ = b_write.shutdown(Shutdown::Write);
    });
    let (mut b_read, mut a_write) = (b, a);
    let _ = io::copy(&mut b_read, &mut a_write);
    let _ = a_write.shutdown(Shutdown::Write);
    let _ = upstream.join();
    Ok(())
}

/// The arguments that start a supervisor for `vm`: forward `forwards`, and run
/// `program` with `args` as its child.
pub fn supervise_args(
    vm: &str,
    forwards: &[Forward],
    program: &str,
    args: &[String],
) -> Vec<String> {
    let mut a = vec![SUPERVISE.to_string(), "--vm".into(), vm.into()];
    for f in forwards {
        a.push("--forward".into());
        a.push(f.to_string());
    }
    a.push("--".into());
    a.push(program.into());
    a.extend(args.iter().cloned());
    a
}

/// A running supervisor, read back from its command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supervisor {
    pub vm: String,
    pub forwards: Vec<Forward>,
}

/// Find the running supervisors with `ps`.
pub fn supervisors() -> Result<Vec<Supervisor>> {
    let out = Command::new("ps")
        .args(["-axww", "-o", "args="])
        .output()
        .context("running ps")?;
    if !out.status.success() {
        bail!("ps failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_supervisor)
        .collect())
}

/// The supervisor for `vm`, if one runs.
pub fn supervisor_for(vm: &str) -> Result<Option<Supervisor>> {
    Ok(supervisors()?.into_iter().find(|s| s.vm == vm))
}

/// Parse one `ps` command line. Read the options up to `--` only, because the
/// child's arguments after it can hold any text.
fn parse_supervisor(line: &str) -> Option<Supervisor> {
    let mut words = line.split_whitespace().skip_while(|w| *w != SUPERVISE);
    words.next()?;
    let (mut vm, mut forwards) = (None, Vec::new());
    while let Some(flag) = words.next() {
        match flag {
            "--vm" => vm = words.next().map(str::to_string),
            "--forward" => forwards.push(words.next()?.parse().ok()?),
            "--" => break,
            _ => return None,
        }
    }
    Some(Supervisor { vm: vm?, forwards })
}

/// A short `host -> guest` label, such as `127.0.0.1:8080 -> 80`.
pub fn label(f: &Forward) -> String {
    format!("{} -> {}", f.host_addr(), f.guest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn parses_host_guest_with_localhost_default() {
        let f: Forward = "8080:80".parse().unwrap();
        assert_eq!(f.bind, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!((f.host, f.guest), (8080, 80));
    }

    #[test]
    fn parses_bind_addresses() {
        let f: Forward = "0.0.0.0:2222:22".parse().unwrap();
        assert_eq!(f.bind, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let f: Forward = "[::1]:2222:22".parse().unwrap();
        assert_eq!(f.bind, "::1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn display_round_trips() {
        for s in ["127.0.0.1:8080:80", "0.0.0.0:0:5432", "[::1]:2222:22"] {
            let f: Forward = s.parse().unwrap();
            assert_eq!(f.to_string(), s);
            assert_eq!(f.to_string().parse::<Forward>().unwrap(), f);
        }
    }

    #[test]
    fn rejects_bad_specs() {
        for bad in [
            "",
            "80",
            "x:80",
            "8080:x",
            "8080:0",
            "70000:80",
            "nope:8080:80",
        ] {
            assert!(bad.parse::<Forward>().is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn rejects_duplicate_fixed_host_ports_but_allows_auto_ports() {
        let f = |s: &str| s.parse::<Forward>().unwrap();
        assert!(validate(&[f("8080:80"), f("8080:81")]).is_err());
        assert!(validate(&[f("8080:80"), f("0.0.0.0:8080:81")]).is_ok());
        assert!(validate(&[f("0:80"), f("0:81")]).is_ok());
    }

    #[test]
    fn auto_ports_match_any_host_port() {
        let config: Forward = "0:80".parse().unwrap();
        assert!(config.matches(&"127.0.0.1:53211:80".parse().unwrap()));
        assert!(!config.matches(&"127.0.0.1:53211:81".parse().unwrap()));
        let fixed: Forward = "8080:80".parse().unwrap();
        assert!(!fixed.matches(&"127.0.0.1:8081:80".parse().unwrap()));
    }

    #[test]
    fn reserve_assigns_auto_ports_and_rejects_busy_ones() {
        let auto: Forward = "0:80".parse().unwrap();
        let reserved = reserve(&[auto]).unwrap();
        assert_ne!(reserved[0].host, 0);

        let busy = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = busy.local_addr().unwrap().port();
        let err = reserve(&[format!("{port}:80").parse().unwrap()]).unwrap_err();
        assert!(err.to_string().contains("not available"), "{err:#}");
    }

    #[test]
    fn supervisor_args_parse_back_from_ps() {
        let forwards = vec![
            "8080:80".parse().unwrap(),
            "0.0.0.0:2222:22".parse().unwrap(),
        ];
        let args = supervise_args(
            "vm-a",
            &forwards,
            "/opt/homebrew/bin/tart",
            &["run".into(), "--dir=/my dir:tag=p".into(), "vm-a".into()],
        );
        let line = format!("/usr/local/bin/dirtbag {}", args.join(" "));
        let parsed = parse_supervisor(&line).unwrap();
        assert_eq!(parsed.vm, "vm-a");
        assert_eq!(parsed.forwards, forwards);
        assert_eq!(parse_supervisor("/usr/bin/tart run vm-a"), None);
    }

    /// Relay a round trip through [`serve`] to a local echo server that
    /// stands in for the guest.
    // 192.0.2.0/24 is a documentation range, so no host interface has it.
    const ABSENT: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));

    #[test]
    fn reserve_accepts_bind_addresses_that_do_not_exist_yet() {
        let fixed = Forward {
            bind: ABSENT,
            host: 3000,
            guest: 3000,
        };
        let auto = Forward {
            bind: ABSENT,
            host: 0,
            guest: 80,
        };
        let reserved = reserve(&[fixed, auto]).unwrap();
        assert_eq!(reserved[0], fixed);
        assert_ne!(reserved[1].host, 0);
    }

    #[test]
    fn bind_when_available_gives_up_after_the_timeout() {
        let f = Forward {
            bind: ABSENT,
            host: 3000,
            guest: 3000,
        };
        let err = bind_when_available(&f, Duration::ZERO).unwrap_err();
        assert!(format!("{err:#}").contains("192.0.2.1:3000"), "{err:#}");
    }

    #[test]
    fn bind_when_available_fails_at_once_on_a_busy_port() {
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let f = Forward {
            bind: Ipv4Addr::LOCALHOST.into(),
            host: held.local_addr().unwrap().port(),
            guest: 80,
        };
        let start = Instant::now();
        assert!(bind_when_available(&f, Duration::from_secs(30)).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(is_listening(&f));
    }

    #[test]
    fn keepalive_is_on_with_short_probes() {
        use nix::sys::socket::{getsockopt, sockopt};
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        enable_keepalive(&stream).unwrap();
        assert!(getsockopt(&stream, sockopt::KeepAlive).unwrap());
        assert_eq!(
            getsockopt(&stream, sockopt::TcpKeepAlive).unwrap(),
            KEEPALIVE_IDLE_SECS
        );
        assert_eq!(
            getsockopt(&stream, sockopt::TcpKeepCount).unwrap(),
            KEEPALIVE_PROBES
        );
    }

    #[test]
    fn serve_relays_to_the_guest_and_back() {
        let echo = TcpListener::bind("127.0.0.1:0").unwrap();
        let guest = echo.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut s in echo.incoming().flatten() {
                let mut buf = Vec::new();
                s.read_to_end(&mut buf).unwrap();
                s.write_all(&buf).unwrap();
            }
        });

        let front = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = front.local_addr().unwrap();
        std::thread::spawn(move || serve(front, guest, |_| Some(Ipv4Addr::LOCALHOST.into())));

        let mut client = TcpStream::connect(addr).unwrap();
        client.write_all(b"hello guest").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut reply = String::new();
        client.read_to_string(&mut reply).unwrap();
        assert_eq!(reply, "hello guest");
    }
}
