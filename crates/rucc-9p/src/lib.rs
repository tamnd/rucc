//! The 9P2000 protocol, so that rucc can read a Plan 9 tree that a 9P server gives.
//!
//! Design: tamnd/rucc#4153, and 07 of the rucc-plan9 plan. Layer rank 0, see
//! `spec/18-package-layout.md` section 18.2.
//!
//! # Why this crate exists
//!
//! A Plan 9 system gives its files over 9P, and so do `u9fs`, `exportfs` and the file servers of
//! plan9port. When the headers and the libraries of a build are on such a server, `rucc
//! --sysroot=9p:tcp!host!port` reads them from it with no mount on the host. A mount needs FUSE
//! or a kernel module on the host, and the hosts in the support table do not all have one.
//!
//! # What is here
//!
//! [`wire`] gives the bytes of each message. [`Client`] speaks the protocol over one byte stream
//! and reads files, with a cache of what it read that the qid of each file keeps correct, and a
//! count of the time it waited on the server. [`serve`] is a small read-only server of a tree in
//! memory. The tests of this crate and of the driver use it, and it is not a file server for a
//! real tree.
//!
//! Only the reading half of the protocol is here. A compiler reads its sources and writes its
//! output to the host, so `create`, `write`, `remove` and `wstat` are not here, and neither is
//! authentication.

pub mod client;
pub mod serve;
pub mod wire;

pub use client::{Client, Stats};
pub use wire::{Qid, Stat};

use std::io;
use std::net::TcpStream;

/// The port of 9P on TCP, which `dial` uses when the address gives none.
pub const PORT: u16 = 564;

/// Connect to a 9P server at `addr`.
///
/// The address is a Plan 9 dial string, `tcp!host!port` or `tcp!host`, or `host:port`. The
/// network `net` is the same as `tcp`, and a missing port is 564.
///
/// # Errors
///
/// When the address is not one of these forms, or the connection fails.
pub fn dial(addr: &str) -> io::Result<TcpStream> {
    let stream = TcpStream::connect(host_port(addr)?)?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// The host and the port of a dial string.
fn host_port(addr: &str) -> io::Result<(String, u16)> {
    let bad =
        || io::Error::new(io::ErrorKind::InvalidInput, format!("`{addr}` is not a 9P address"));
    let port = |text: &str| text.parse::<u16>().map_err(|_| bad());
    let parts: Vec<&str> = addr.split('!').collect();
    let (host, port) = match parts.as_slice() {
        [net, host] if matches!(*net, "tcp" | "net") => (*host, PORT),
        [net, host, p] if matches!(*net, "tcp" | "net") => (*host, port(p)?),
        [plain] => match plain.rsplit_once(':') {
            Some((host, p)) => (host, port(p)?),
            None => (*plain, PORT),
        },
        _ => return Err(bad()),
    };
    if host.is_empty() {
        return Err(bad());
    }
    // A host in brackets is an IPv6 address, which `TcpStream::connect` takes with no brackets.
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    Ok((host.to_owned(), port))
}

#[cfg(test)]
mod tests {
    use super::host_port;

    #[test]
    fn a_dial_string_gives_the_host_and_the_port() {
        let ok = |addr: &str| host_port(addr).unwrap();
        assert_eq!(ok("tcp!fs.local!5640"), ("fs.local".to_owned(), 5640));
        assert_eq!(ok("net!10.0.0.2"), ("10.0.0.2".to_owned(), 564));
        assert_eq!(ok("127.0.0.1:9999"), ("127.0.0.1".to_owned(), 9999));
        assert_eq!(ok("tcp![::1]!564"), ("::1".to_owned(), 564));
        for bad in ["udp!h!1", "tcp!!1", "tcp!h!port", "a!b!c!d", ""] {
            assert!(host_port(bad).is_err(), "{bad}");
        }
    }
}
