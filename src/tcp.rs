//! TCP socket options.

use std::net::TcpStream;

const KEEPALIVE_IDLE_SECS: u32 = 60;
const KEEPALIVE_INTERVAL_SECS: u32 = 10;
const KEEPALIVE_PROBES: u32 = 6;

/// Send keepalive probes on an idle connection. If the peer does not answer
/// for about two minutes, reads and writes fail.
pub fn enable_keepalive(stream: &TcpStream) -> nix::Result<()> {
    use nix::sys::socket::{setsockopt, sockopt};
    setsockopt(stream, sockopt::KeepAlive, &true)?;
    setsockopt(stream, sockopt::TcpKeepAlive, &KEEPALIVE_IDLE_SECS)?;
    setsockopt(stream, sockopt::TcpKeepInterval, &KEEPALIVE_INTERVAL_SECS)?;
    setsockopt(stream, sockopt::TcpKeepCount, &KEEPALIVE_PROBES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::socket::{getsockopt, sockopt};
    use std::net::{Ipv4Addr, TcpListener};

    #[test]
    fn keepalive_is_on_with_short_probes() {
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
}
