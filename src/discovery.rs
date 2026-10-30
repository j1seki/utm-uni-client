//! Bonjour/mDNS discovery of UTM Remote servers.
//!
//! UTM advertises its Remote listener as `_utm_server._tcp` (the same service
//! pyutmremote browses).  Results are kept in memory and pushed to the browser
//! over a WebSocket, so the UI can show servers without any manual config.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};

use mdns_sd::{ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent};
use serde::Serialize;
use tokio::sync::watch;
use tracing::{debug, info, warn};

pub const SERVICE_TYPE: &str = "_utm_server._tcp.local.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredServer {
    /// Stable identity (the mDNS full service name).
    pub id: String,
    pub name: String,
    /// Address to connect to, IPv4 preferred.
    pub host: String,
    pub port: u16,
    /// Every address advertised by the service, for duplicate matching.
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoverySnapshot {
    pub available: bool,
    pub error: Option<String>,
    pub servers: Vec<DiscoveredServer>,
}

struct DiscoveryInner {
    servers: RwLock<Vec<DiscoveredServer>>,
    daemon: ServiceDaemon,
}

/// Handle to the background mDNS browser.  Cloning is cheap and shares state.
#[derive(Clone)]
pub struct Discovery {
    inner: Option<Arc<DiscoveryInner>>,
    error: Option<String>,
    changed: watch::Sender<u64>,
}

impl Discovery {
    pub fn start() -> Self {
        let daemon = match ServiceDaemon::new() {
            Ok(daemon) => daemon,
            Err(error) => {
                return Self::unavailable(format!("cannot start mDNS daemon: {error}"));
            }
        };
        let receiver = match daemon.browse(SERVICE_TYPE) {
            Ok(receiver) => receiver,
            Err(error) => {
                let _ = daemon.shutdown();
                return Self::unavailable(format!("cannot browse {SERVICE_TYPE}: {error}"));
            }
        };

        let (changed, _) = watch::channel(0u64);
        let inner = Arc::new(DiscoveryInner {
            servers: RwLock::new(Vec::new()),
            daemon,
        });

        let thread_inner = Arc::clone(&inner);
        let thread_changed = changed.clone();
        let spawned = std::thread::Builder::new()
            .name("utm-web-mdns".to_owned())
            .spawn(move || {
                while let Ok(event) = receiver.recv() {
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            apply_resolved(&thread_inner, &thread_changed, *info);
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            remove(&thread_inner, &thread_changed, &fullname);
                        }
                        ServiceEvent::SearchStarted(_) => {
                            debug!("mDNS search started for {SERVICE_TYPE}");
                        }
                        ServiceEvent::SearchStopped(_) => {
                            debug!("mDNS search stopped for {SERVICE_TYPE}");
                        }
                        _ => {}
                    }
                }
            });

        if let Err(error) = spawned {
            let _ = inner.daemon.shutdown();
            return Self::unavailable(format!("cannot start mDNS thread: {error}"));
        }

        info!("mDNS discovery enabled for {SERVICE_TYPE}");
        Self {
            inner: Some(inner),
            error: None,
            changed,
        }
    }

    fn unavailable(error: String) -> Self {
        warn!("{error}");
        let (changed, _) = watch::channel(0u64);
        Self {
            inner: None,
            error: Some(error),
            changed,
        }
    }

    pub fn snapshot(&self) -> DiscoverySnapshot {
        let servers = match &self.inner {
            Some(inner) => inner
                .servers
                .read()
                .expect("discovery lock poisoned")
                .clone(),
            None => Vec::new(),
        };
        DiscoverySnapshot {
            available: self.inner.is_some(),
            error: self.error.clone(),
            servers,
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub fn shutdown(&self) {
        if let Some(inner) = &self.inner {
            let _ = inner.daemon.shutdown();
        }
    }
}

fn apply_resolved(inner: &DiscoveryInner, changed: &watch::Sender<u64>, info: ResolvedService) {
    let Some(host) = choose_host(info.get_addresses()) else {
        debug!("ignoring {}: no usable address", info.fullname);
        return;
    };

    let mut addresses: Vec<String> = info
        .get_addresses()
        .iter()
        .map(ToString::to_string)
        .collect();
    addresses.sort();
    addresses.dedup();

    let server = DiscoveredServer {
        id: info.fullname.clone(),
        name: display_name(&info.fullname),
        host,
        port: info.port,
        addresses,
    };

    let mut servers = inner.servers.write().expect("discovery lock poisoned");
    if let Some(existing) = servers.iter_mut().find(|item| item.id == server.id) {
        if *existing == server {
            return;
        }
        *existing = server;
    } else {
        servers.push(server);
    }
    servers.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.host.cmp(&b.host))
            .then_with(|| a.port.cmp(&b.port))
    });
    drop(servers);
    bump(changed);
}

fn remove(inner: &DiscoveryInner, changed: &watch::Sender<u64>, fullname: &str) {
    let mut servers = inner.servers.write().expect("discovery lock poisoned");
    let before = servers.len();
    servers.retain(|item| item.id != fullname);
    if servers.len() != before {
        drop(servers);
        bump(changed);
    }
}

fn bump(changed: &watch::Sender<u64>) {
    changed.send_modify(|value| *value = value.wrapping_add(1));
}

/// Pick an address to connect to.  IPv4 is preferred because it is the least
/// surprising on mixed networks; global IPv6 comes next, link-local last.
fn choose_host(addresses: &HashSet<ScopedIp>) -> Option<String> {
    let mut candidates: Vec<&ScopedIp> = addresses.iter().collect();
    candidates.sort_by_key(|address| address.to_ip_addr().to_string());

    candidates
        .iter()
        .find(|address| address.is_ipv4())
        .or_else(|| candidates.iter().find(|address| !is_link_local(address)))
        .or_else(|| candidates.first())
        .map(|address| address.to_string())
}

fn is_link_local(address: &ScopedIp) -> bool {
    match address.to_ip_addr() {
        IpAddr::V6(ip) => (ip.segments()[0] & 0xffc0) == 0xfe80,
        IpAddr::V4(_) => false,
    }
}

/// Convert an mDNS full name (`Studio\032Mac._utm_server._tcp.local.`) into a
/// human-readable instance name.
fn display_name(fullname: &str) -> String {
    let instance = fullname
        .strip_suffix(SERVICE_TYPE)
        .unwrap_or(fullname)
        .trim_end_matches('.');
    unescape_dns(instance)
}

fn unescape_dns(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let bytes = name.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            if index + 3 < bytes.len() && bytes[index + 1..index + 4].iter().all(u8::is_ascii_digit)
            {
                let value = (bytes[index + 1] - b'0') as u32 * 100
                    + (bytes[index + 2] - b'0') as u32 * 10
                    + (bytes[index + 3] - b'0') as u32;
                if let Some(character) = char::from_u32(value) {
                    out.push(character);
                    index += 4;
                    continue;
                }
            }
            if index + 1 < bytes.len() {
                out.push(bytes[index + 1] as char);
                index += 2;
                continue;
            }
        }
        let character = name[index..].chars().next().unwrap_or('\u{fffd}');
        out.push(character);
        index += character.len_utf8();
    }
    out
}

/// A discovery handle that never finds anything, used by tests and as a
/// fallback when mDNS is unavailable.
impl Default for Discovery {
    fn default() -> Self {
        Self::unavailable("mDNS discovery is disabled".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_instance_names() {
        assert_eq!(
            display_name("Studio Mac._utm_server._tcp.local."),
            "Studio Mac"
        );
        assert_eq!(
            display_name("Studio\\032Mac._utm_server._tcp.local."),
            "Studio Mac"
        );
        assert_eq!(display_name("Lab\\.Mac._utm_server._tcp.local."), "Lab.Mac");
        assert_eq!(
            display_name("Back\\\\slash._utm_server._tcp.local."),
            "Back\\slash"
        );
    }

    #[test]
    fn prefers_ipv4() {
        let mut addresses = HashSet::new();
        addresses.insert(ScopedIp::from(IpAddr::V6(
            "2001:db8::1".parse().expect("ipv6"),
        )));
        addresses.insert(ScopedIp::from(IpAddr::V4(
            "192.168.1.20".parse().expect("ipv4"),
        )));
        assert_eq!(choose_host(&addresses).as_deref(), Some("192.168.1.20"));
    }

    #[test]
    fn falls_back_to_ipv6() {
        let mut addresses = HashSet::new();
        addresses.insert(ScopedIp::from(IpAddr::V6(
            "2001:db8::5".parse().expect("ipv6"),
        )));
        assert_eq!(choose_host(&addresses).as_deref(), Some("2001:db8::5"));
    }

    #[test]
    fn empty_addresses_have_no_host() {
        assert!(choose_host(&HashSet::new()).is_none());
    }
}
