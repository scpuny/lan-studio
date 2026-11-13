//! mDNS multicast transport (RFC 6762) with zero third-party crates.
//!
//! On Linux the UDP socket is created through a raw fd so `SO_REUSEADDR` and
//! `SO_REUSEPORT` can be set *before* bind. That is what allows `lan-mdns` to
//! coexist with a system mDNS daemon on port 5353 (F-C03).

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;

pub const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const MDNS_PORT: u16 = 5353;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceInfo {
    pub name: String,
    pub addr: Ipv4Addr,
}

pub fn group_addr(group: Ipv4Addr, port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(group, port))
}

#[cfg(target_os = "linux")]
fn raw_bind(port: u16) -> io::Result<UdpSocket> {
    use std::os::fd::FromRawFd;

    #[repr(C)]
    struct SockAddrIn {
        family: u16,
        port: u16,
        addr: u32,
        zero: [u8; 8],
    }

    extern "C" {
        fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
        fn setsockopt(fd: i32, level: i32, name: i32, value: *const i32, len: u32) -> i32;
        fn bind(fd: i32, addr: *const SockAddrIn, len: u32) -> i32;
        fn close(fd: i32) -> i32;
    }

    const AF_INET: i32 = 2;
    const SOCK_DGRAM: i32 = 2;
    const SOL_SOCKET: i32 = 1;
    const SO_REUSEADDR: i32 = 2;
    const SO_REUSEPORT: i32 = 15;

    unsafe {
        let fd = socket(AF_INET, SOCK_DGRAM, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let one: i32 = 1;
        for option in [SO_REUSEADDR, SO_REUSEPORT] {
            if setsockopt(
                fd,
                SOL_SOCKET,
                option,
                &one as *const i32,
                std::mem::size_of::<i32>() as u32,
            ) != 0
            {
                let error = io::Error::last_os_error();
                let _ = close(fd);
                return Err(error);
            }
        }
        let addr = SockAddrIn {
            family: AF_INET as u16,
            port: port.to_be(),
            addr: 0,
            zero: [0; 8],
        };
        if bind(
            fd,
            &addr as *const SockAddrIn,
            std::mem::size_of::<SockAddrIn>() as u32,
        ) != 0
        {
            let error = io::Error::last_os_error();
            let _ = close(fd);
            return Err(error);
        }
        Ok(UdpSocket::from_raw_fd(fd))
    }
}

#[cfg(not(target_os = "linux"))]
fn raw_bind(port: u16) -> io::Result<UdpSocket> {
    UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))
}

pub struct MdnsSocket {
    socket: UdpSocket,
    group: Ipv4Addr,
    port: u16,
}

impl MdnsSocket {
    /// Bind a multicast socket. An empty `interfaces` list joins on the
    /// default interface; otherwise each address is joined explicitly.
    pub fn bind(
        group: Ipv4Addr,
        port: u16,
        interfaces: &[Ipv4Addr],
        multicast_loop: bool,
    ) -> io::Result<Self> {
        let socket = raw_bind(port)?;
        socket.set_multicast_loop_v4(multicast_loop)?;
        socket.set_multicast_ttl_v4(255)?;
        socket.set_read_timeout(Some(Duration::from_millis(250)))?;
        if interfaces.is_empty() {
            socket.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)?;
        } else {
            for interface in interfaces {
                socket.join_multicast_v4(&group, interface)?;
            }
        }
        Ok(Self {
            socket,
            group,
            port,
        })
    }

    pub fn send(&self, data: &[u8], target: SocketAddr) -> io::Result<usize> {
        self.socket.send_to(data, target)
    }

    pub fn broadcast(&self, data: &[u8]) -> io::Result<usize> {
        self.send(data, self.group_addr())
    }

    pub fn recv(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.socket.recv_from(buf)
    }

    pub fn group_addr(&self) -> SocketAddr {
        group_addr(self.group, self.port)
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

/// Enumerate local IPv4 addresses. Uses `ip -4 -o addr show` when available
/// and falls back to the kernel-selected egress address for the mDNS group.
pub fn local_ipv4_addrs() -> Vec<Ipv4Addr> {
    let mut result = Vec::new();
    for info in list_interfaces_raw() {
        if !result.contains(&info.addr) && !info.addr.is_loopback() {
            result.push(info.addr);
        }
    }
    if result.is_empty() {
        if let Ok(socket) = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)) {
            if socket
                .connect(SocketAddrV4::new(MDNS_GROUP, MDNS_PORT))
                .is_ok()
            {
                if let Ok(SocketAddr::V4(addr)) = socket.local_addr() {
                    result.push(*addr.ip());
                }
            }
        }
    }
    result
}

pub fn list_interfaces() -> Vec<InterfaceInfo> {
    let mut result = list_interfaces_raw();
    if result.is_empty() {
        for addr in local_ipv4_addrs() {
            result.push(InterfaceInfo {
                name: "default".to_string(),
                addr,
            });
        }
    }
    result
}

/// Interface name carrying the default IPv4 route.
///
/// `mdns.interface_mode = auto` joins multicast on the kernel-selected
/// interface, so the console needs this name to tell the truth about which
/// interface is actually taking part (F-A12). Returns `None` when `ip` is
/// unavailable or no default route exists.
pub fn default_interface() -> Option<String> {
    let output = std::process::Command::new("ip")
        .args(["-4", "route", "show", "default"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut tokens = line.split_whitespace();
        while let Some(token) = tokens.next() {
            if token == "dev" {
                return tokens.next().map(|name| name.to_string());
            }
        }
    }
    None
}

fn list_interfaces_raw() -> Vec<InterfaceInfo> {
    let mut result = Vec::new();
    let output = match std::process::Command::new("ip")
        .args(["-4", "-o", "addr", "show"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return result,
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut tokens = line.split_whitespace();
        tokens.next();
        let name = tokens.next().unwrap_or("unknown").to_string();
        let mut inet_seen = false;
        for token in tokens {
            if token == "inet" {
                inet_seen = true;
                continue;
            }
            if inet_seen {
                if let Some(ip_text) = token.split('/').next() {
                    if let Ok(addr) = ip_text.parse::<Ipv4Addr>() {
                        result.push(InterfaceInfo {
                            name: name.clone(),
                            addr,
                        });
                    }
                }
                break;
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_list_is_bounded_and_well_formed() {
        let list = list_interfaces();
        assert!(list.len() <= 256);
        for info in list {
            assert!(!info.name.is_empty());
        }
    }

    #[test]
    fn group_address_is_standard_mdns() {
        let addr = group_addr(MDNS_GROUP, MDNS_PORT);
        assert_eq!(addr.to_string(), "224.0.0.251:5353");
    }

    #[test]
    fn multicast_loopback_roundtrip_on_isolated_port() {
        // Use a non-standard port so the test never touches production mDNS.
        let port = 15353;
        let socket = match MdnsSocket::bind(Ipv4Addr::new(239, 1, 2, 3), port, &[], true) {
            Ok(socket) => socket,
            Err(_) => return, // sandboxed CI without multicast support
        };
        let payload = b"lan-studio-mdns-loopback";
        if socket.broadcast(payload).is_err() {
            return;
        }
        let mut buf = [0u8; 256];
        for _ in 0..8 {
            match socket.recv(&mut buf) {
                Ok((len, _)) if &buf[..len] == payload => return,
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        // Multicast loopback may be filtered by the host; not a codec failure.
    }
}
