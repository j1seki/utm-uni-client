//! Small networking helpers shared by the UTM and SPICE clients.

use std::io;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};

use rustls::pki_types::ServerName;
use tokio::net::TcpStream;

/// Format a host and port for `TcpStream::connect`, bracketing IPv6 literals.
pub fn tcp_address(host: &str, port: u16) -> String {
    let host = host.trim();
    if host.starts_with('[') {
        format!("{host}:{port}")
    } else if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Connect to a host and port, resolving IPv6 scope identifiers.
///
/// `TcpStream::connect` does not accept zone identifiers such as
/// `fe80::1%enp0s1`, but mDNS returns them for link-local addresses.  Numeric
/// zones (`%2`) and interface names are converted to a scope id here.
pub async fn connect(host: &str, port: u16) -> io::Result<TcpStream> {
    if let Some(address) = scoped_ipv6_socket_addr(host, port)? {
        return TcpStream::connect(address).await;
    }
    TcpStream::connect(tcp_address(host, port)).await
}

fn scoped_ipv6_socket_addr(host: &str, port: u16) -> io::Result<Option<SocketAddr>> {
    let bare = host.trim().trim_start_matches('[').trim_end_matches(']');
    let Some((address, scope)) = bare.split_once('%') else {
        return Ok(None);
    };
    let Ok(address) = address.parse::<Ipv6Addr>() else {
        return Ok(None);
    };
    let scope_id = resolve_scope_id(scope)?;
    Ok(Some(SocketAddr::V6(SocketAddrV6::new(
        address, port, 0, scope_id,
    ))))
}

fn resolve_scope_id(scope: &str) -> io::Result<u32> {
    if let Ok(index) = scope.parse::<u32>() {
        return Ok(index);
    }
    if_addrs::get_if_addrs()?
        .into_iter()
        .find(|interface| interface.name == scope)
        .and_then(|interface| interface.index)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("unknown network interface {scope:?}"),
            )
        })
}

/// Build a rustls server name from a profile host.  IPv6 scope identifiers
/// (`fe80::1%en0`) are only meaningful to the socket layer, not to TLS.
pub fn tls_server_name(host: &str) -> Option<ServerName<'static>> {
    let bare = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split('%')
        .next()
        .unwrap_or(host);
    ServerName::try_from(bare.to_owned()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_hosts_and_ports() {
        assert_eq!(tcp_address("mac.local", 21589), "mac.local:21589");
        assert_eq!(tcp_address("192.168.1.2", 21589), "192.168.1.2:21589");
        assert_eq!(tcp_address("2001:db8::1", 21589), "[2001:db8::1]:21589");
        assert_eq!(tcp_address("fe80::1%en0", 21589), "[fe80::1%en0]:21589");
        assert_eq!(tcp_address("[2001:db8::1]", 21589), "[2001:db8::1]:21589");
    }

    #[test]
    fn parses_scoped_ipv6_addresses() {
        let address = scoped_ipv6_socket_addr("fe80::1%2", 21589)
            .expect("scope lookup")
            .expect("scoped address");
        match address {
            SocketAddr::V6(address) => {
                assert_eq!(address.ip(), &"fe80::1".parse::<Ipv6Addr>().expect("ip"));
                assert_eq!(address.port(), 21589);
                assert_eq!(address.scope_id(), 2);
            }
            SocketAddr::V4(_) => panic!("expected IPv6"),
        }
        assert!(scoped_ipv6_socket_addr("2001:db8::1", 21589)
            .expect("no scope")
            .is_none());
        assert!(scoped_ipv6_socket_addr("mac.local", 21589)
            .expect("not ipv6")
            .is_none());
    }

    #[test]
    fn strips_ipv6_scope_for_tls() {
        let name = tls_server_name("fe80::1%en0").expect("server name");
        assert_eq!(name.to_str(), "fe80::1");
        assert!(tls_server_name("mac.local").is_some());
    }
}
