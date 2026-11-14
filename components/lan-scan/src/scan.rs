//! Read-only LAN asset / port discovery (PRD §4.5).
//!
//! Hard rules: bounded rate and concurrency, no payload is ever written to a
//! probed socket, and results only ever land in this component's own store.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub max_hosts: usize,
    pub max_ports: usize,
    pub rate_per_second: u64,
    pub concurrency: usize,
    pub connect_timeout_ms: u64,
    pub banner_bytes: usize,
    pub ports: Vec<u16>,
    pub results_path: String,
    pub max_results: usize,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            max_hosts: 4_096,
            max_ports: 64,
            rate_per_second: 200,
            concurrency: 16,
            connect_timeout_ms: 300,
            banner_bytes: 64,
            ports: vec![22, 80, 443, 5353, 8080],
            results_path: "lan-scan.results.json".to_string(),
            max_results: 200,
        }
    }
}

/// Fixed-window limiter. `now` is injectable so the ceiling is testable.
#[derive(Debug)]
pub struct RateLimiter {
    pub rate: u64,
    start: Instant,
    used: u64,
}

impl RateLimiter {
    pub fn new(rate: u64, start: Instant) -> Self {
        Self {
            rate: rate.max(1),
            start,
            used: 0,
        }
    }

    pub fn take(&mut self, now: Instant) -> bool {
        if now.duration_since(self.start) >= Duration::from_secs(1) {
            self.start = now;
            self.used = 0;
        }
        if self.used >= self.rate {
            false
        } else {
            self.used += 1;
            true
        }
    }
}

/// Expand an IPv4 CIDR into explicit hosts, with a hard upper bound.
pub fn expand_cidr(text: &str, max_hosts: usize) -> Result<Vec<Ipv4Addr>, String> {
    let (addr, bits) = text
        .trim()
        .split_once('/')
        .ok_or_else(|| "segment must be CIDR (a.b.c.d/n)".to_string())?;
    let ip: Ipv4Addr = addr
        .parse()
        .map_err(|_| "invalid IPv4 address".to_string())?;
    let bits: u32 = bits
        .parse()
        .map_err(|_| "invalid prefix length".to_string())?;
    if bits > 32 {
        return Err("prefix length must be 0..32".to_string());
    }
    let count = 1u64.checked_shl(32 - bits).unwrap_or(u64::MAX);
    if count > max_hosts as u64 {
        return Err(format!(
            "segment expands to {} hosts, limit is {}",
            count, max_hosts
        ));
    }
    let mask: u32 = if bits == 0 {
        0
    } else {
        u32::MAX << (32 - bits)
    };
    let base = u32::from(ip) & mask;
    Ok((0..count)
        .map(|offset| Ipv4Addr::from(base.wrapping_add(offset as u32)))
        .collect())
}

pub fn service_name(port: u16) -> &'static str {
    match port {
        20 | 21 => "ftp",
        22 => "ssh",
        23 => "telnet",
        25 | 587 => "smtp",
        53 => "dns",
        80 | 8080 | 8000 => "http",
        110 => "pop3",
        143 => "imap",
        443 | 8443 => "https",
        445 => "smb",
        1883 => "mqtt",
        3306 => "mysql",
        5353 => "mdns",
        5432 => "postgresql",
        6379 => "redis",
        9100 => "metrics",
        _ => "unknown",
    }
}

#[derive(Debug, Clone)]
pub struct ServiceHit {
    pub port: u16,
    pub service: String,
    pub banner: String,
}

#[derive(Debug, Clone)]
pub struct HostResult {
    pub ip: String,
    pub hits: Vec<ServiceHit>,
    pub probed_at: String,
}

impl HostResult {
    pub fn to_json(&self) -> String {
        let hits: Vec<String> = self
            .hits
            .iter()
            .map(|hit| {
                format!(
                    "{{\"port\":{},\"service\":{},\"banner\":{}}}",
                    hit.port,
                    q(&hit.service),
                    q(&hit.banner)
                )
            })
            .collect();
        format!(
            "{{\"ip\":{},\"probed_at\":{},\"hits\":[{}]}}",
            q(&self.ip),
            q(&self.probed_at),
            hits.join(",")
        )
    }
}

/// A probe is a passive TCP connect + optional banner read. Implementations
/// must never write bytes to the target socket.
pub trait Probe: Send + Sync {
    fn probe(&self, ip: Ipv4Addr, port: u16) -> Option<ServiceHit>;
}

pub struct TcpProbe {
    pub timeout: Duration,
    pub banner_bytes: usize,
}

impl Probe for TcpProbe {
    fn probe(&self, ip: Ipv4Addr, port: u16) -> Option<ServiceHit> {
        let addr = SocketAddr::new(IpAddr::V4(ip), port);
        let mut stream = TcpStream::connect_timeout(&addr, self.timeout).ok()?;
        // READ-ONLY: no `write` call anywhere in this implementation.
        let mut buf = vec![0u8; self.banner_bytes.min(256)];
        let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
        let read = stream.read(&mut buf).unwrap_or_default();
        let banner: String = String::from_utf8_lossy(&buf[..read])
            .chars()
            .filter(|ch| ch.is_ascii_graphic() || *ch == ' ')
            .take(64)
            .collect();
        Some(ServiceHit {
            port,
            service: service_name(port).to_string(),
            banner,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanOutcome {
    pub hosts: Vec<HostResult>,
    pub total_hosts: usize,
    pub probes: u64,
    pub throttled: u64,
    pub open_ports: u64,
}

/// Bounded parallel scan. Each worker pulls from a shared queue; the shared
/// rate limiter is the authoritative ceiling (PRD §4.5).
pub fn run_scan<P: Probe + Sync>(
    probe: &P,
    ips: Vec<Ipv4Addr>,
    ports: &[u16],
    cfg: &ScanConfig,
) -> ScanOutcome {
    let total_hosts = ips.len();
    let queue = Mutex::new(VecDeque::from(ips));
    let limiter = Mutex::new(RateLimiter::new(cfg.rate_per_second, Instant::now()));
    let results: Mutex<Vec<HostResult>> = Mutex::new(Vec::new());
    let probes = AtomicU64::new(0);
    let throttled = AtomicU64::new(0);

    let workers = cfg.concurrency.clamp(1, 64);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let queue = &queue;
            let limiter = &limiter;
            let results = &results;
            let probes = &probes;
            let throttled = &throttled;
            scope.spawn(move || loop {
                let ip = match queue.lock().unwrap().pop_front() {
                    Some(value) => value,
                    None => break,
                };
                let mut hits = Vec::new();
                for port in ports {
                    if !limiter.lock().unwrap().take(Instant::now()) {
                        throttled.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    probes.fetch_add(1, Ordering::Relaxed);
                    if let Some(hit) = probe.probe(ip, *port) {
                        hits.push(hit);
                    }
                }
                if !hits.is_empty() {
                    results.lock().unwrap().push(HostResult {
                        ip: ip.to_string(),
                        hits,
                        probed_at: common_alert::utc_rfc3339(),
                    });
                }
            });
        }
    });
    let hosts = results.into_inner().unwrap();
    let open_ports = hosts.iter().map(|host| host.hits.len() as u64).sum();
    ScanOutcome {
        hosts,
        total_hosts,
        probes: probes.load(Ordering::Relaxed),
        throttled: throttled.load(Ordering::Relaxed),
        open_ports,
    }
}

pub struct ScanStore {
    /// Canonical JSON lines are kept verbatim so hits survive a restart.
    pub results: VecDeque<String>,
    pub summaries: VecDeque<String>,
    pub path: String,
    pub max_results: usize,
}

impl ScanStore {
    pub fn open(path: &str, max_results: usize) -> Self {
        let mut results = VecDeque::new();
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                if line.trim_start().starts_with('{') {
                    results.push_back(line.to_string());
                }
            }
        }
        let max_results = max_results.max(1);
        while results.len() > max_results {
            results.pop_front();
        }
        Self {
            results,
            summaries: VecDeque::new(),
            path: path.to_string(),
            max_results,
        }
    }

    pub fn ingest(&mut self, outcome: &ScanOutcome, segment: &str) {
        for host in &outcome.hosts {
            self.results.push_back(host.to_json());
        }
        while self.results.len() > self.max_results {
            self.results.pop_front();
        }
        self.summaries.push_back(format!(
            "{{\"segment\":{},\"total_hosts\":{},\"probes\":{},\"throttled\":{},\"open_ports\":{},\"at\":{}}}",
            q(segment),
            outcome.total_hosts,
            outcome.probes,
            outcome.throttled,
            outcome.open_ports,
            q(&common_alert::utc_rfc3339())
        ));
        while self.summaries.len() > 30 {
            self.summaries.pop_front();
        }
        let _ = self.persist();
    }

    /// Only this component's own results file is written; no other component
    /// config is ever touched (PRD §4.5).
    pub fn persist(&self) -> io::Result<()> {
        let lines: Vec<String> = self.results.iter().cloned().collect();
        let tmp = format!("{}.tmp", self.path);
        fs::write(&tmp, lines.join("\n"))?;
        fs::rename(&tmp, &self.path)
    }

    pub fn snapshot_json(&self) -> String {
        let hosts: Vec<String> = self.results.iter().cloned().collect();
        let summaries: Vec<String> = self.summaries.iter().cloned().collect();
        format!(
            "{{\"schema\":1,\"component\":\"lan-scan\",\"hosts\":[{}],\"runs\":[{}],\"total\":{}}}",
            hosts.join(","),
            summaries.join(","),
            hosts.len()
        )
    }
}

fn q(value: &str) -> String {
    common_api::json_quote(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockProbe(AtomicU64);
    impl Probe for MockProbe {
        fn probe(&self, _ip: Ipv4Addr, port: u16) -> Option<ServiceHit> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Some(ServiceHit {
                port,
                service: service_name(port).to_string(),
                banner: String::new(),
            })
        }
    }

    #[test]
    fn cidr_expansion_is_bounded_and_correct() {
        let hosts = expand_cidr("192.168.1.0/30", 16).unwrap();
        assert_eq!(hosts.len(), 4);
        assert_eq!(hosts[0].to_string(), "192.168.1.0");
        assert!(expand_cidr("10.0.0.0/8", 4096).is_err());
        assert!(expand_cidr("192.168.1.0/33", 16).is_err());
        assert!(expand_cidr("bad", 16).is_err());
        assert_eq!(expand_cidr("127.0.0.1/32", 16).unwrap().len(), 1);
    }

    #[test]
    fn rate_limiter_enforces_a_hard_ceiling() {
        let t0 = Instant::now();
        let mut limiter = RateLimiter::new(5, t0);
        let mut allowed = 0;
        for _ in 0..100 {
            if limiter.take(t0) {
                allowed += 1;
            }
        }
        assert_eq!(allowed, 5);
        // A new window releases another 5 slots, never more.
        assert!(limiter.take(t0 + Duration::from_millis(1_001)));
        assert_eq!(limiter.rate, 5);
    }

    #[test]
    fn scan_results_and_probes_are_bounded() {
        let probe = MockProbe(AtomicU64::new(0));
        let cfg = ScanConfig {
            rate_per_second: 10,
            concurrency: 4,
            max_hosts: 100,
            ..ScanConfig::default()
        };
        let ips = expand_cidr("127.0.0.0/28", cfg.max_hosts).unwrap();
        let outcome = run_scan(&probe, ips, &[80], &cfg);
        assert!(
            outcome.probes <= 10,
            "rate ceiling exceeded: {}",
            outcome.probes
        );
        assert_eq!(outcome.probes + outcome.throttled, 16);
        assert_eq!(outcome.total_hosts, 16);
        assert!(outcome.hosts.len() <= 16);
        assert_eq!(probe.0.load(Ordering::Relaxed), outcome.probes);
    }

    #[test]
    fn tcp_probe_never_writes_payload() {
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(value) => value,
            Err(_) => return, // sandbox without loopback sockets
        };
        let port = listener.local_addr().unwrap().port();
        let probe = TcpProbe {
            timeout: Duration::from_millis(300),
            banner_bytes: 64,
        };
        let hit = probe.probe(Ipv4Addr::LOCALHOST, port);
        assert!(hit.is_some(), "connect should succeed");
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 16];
        let _ = stream.set_read_timeout(Some(Duration::from_millis(120)));
        // A read-only probe sends zero bytes: EOF (Ok(0)) or timeout (Err).
        let received = stream.read(&mut buf).unwrap_or(0);
        assert_eq!(received, 0, "probe must never send a payload");
    }

    #[test]
    fn service_names_are_stable() {
        assert_eq!(service_name(22), "ssh");
        assert_eq!(service_name(5353), "mdns");
        assert_eq!(service_name(65534), "unknown");
    }

    #[test]
    fn store_is_bounded_and_snapshot_has_no_config_of_other_components() {
        let path = std::env::temp_dir()
            .join(format!("lan-scan-test-{}.json", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = fs::remove_file(&path);
        let mut store = ScanStore::open(&path, 3);
        let host = HostResult {
            ip: "127.0.0.1".into(),
            hits: vec![],
            probed_at: "2026-11-13T00:00:00Z".into(),
        };
        let outcome = ScanOutcome {
            hosts: vec![host.clone(), host.clone(), host.clone(), host],
            total_hosts: 4,
            probes: 4,
            throttled: 0,
            open_ports: 0,
        };
        store.ingest(&outcome, "127.0.0.1/32");
        assert_eq!(store.results.len(), 3);
        let snapshot = store.snapshot_json();
        assert!(snapshot.contains("\"component\":\"lan-scan\""));
        assert!(!snapshot.contains("lan-mdns.yaml"));
        let disk = fs::read_to_string(&path).unwrap();
        assert_eq!(disk.lines().count(), 3);
        let _ = fs::remove_file(&path);
    }
}
