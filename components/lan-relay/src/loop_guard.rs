//! mDNS relay loop guard — PRD §4.3 hard acceptance (M3).
//!
//! Four independent guards, all pure logic so they are deterministically
//! testable without multicast sockets:
//!   1. packet fingerprint de-duplication in a bounded 5s window;
//!   2. ingress/egress interface isolation (never re-send on the ingress NIC);
//!   3. TTL / hop budget carried in an EDNS0 OPT trailer, decremented on every
//!      forward and dropped when it reaches zero (D8: max 4 hops);
//!   4. locally generated packets are registered and never forwarded.
//!
//! Plus a per-second forwarding ceiling so a misconfigured topology cannot
//! produce a broadcast storm.

use std::collections::{HashMap, VecDeque};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use common_crypto::sha256;

/// 24-bit magic in the OPT TTL field; the high 8 bits carry the remaining TTL.
const RELAY_MAGIC: u32 = 0x004C_5352; // "LSR"
const TRAILER_LEN: usize = 11;

#[derive(Debug, Clone, Copy, Default)]
pub struct Counters {
    pub received: u64,
    pub forwarded: u64,
    pub duplicate: u64,
    pub same_interface: u64,
    pub ttl_exceeded: u64,
    pub self_generated: u64,
    pub outside_whitelist: u64,
    pub rate_limited: u64,
    pub malformed: u64,
    pub bytes_forwarded: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Forward {
        ttl: u8,
        targets: Vec<String>,
        frame: Vec<u8>,
    },
    Duplicate,
    SameInterface,
    TtlExceeded,
    SelfGenerated,
    OutsideWhitelist,
    RateLimited,
    Malformed,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Forward { .. } => "forward",
            Verdict::Duplicate => "duplicate",
            Verdict::SameInterface => "same_interface",
            Verdict::TtlExceeded => "ttl_exceeded",
            Verdict::SelfGenerated => "self_generated",
            Verdict::OutsideWhitelist => "outside_whitelist",
            Verdict::RateLimited => "rate_limited",
            Verdict::Malformed => "malformed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GuardConfig {
    /// Logical interface identifiers. Each bound socket carries one of these.
    pub interfaces: Vec<String>,
    /// Allowed source networks (CIDR). Empty = allow all.
    pub allowed_networks: Vec<String>,
    pub fingerprint_window_ms: u64,
    /// Initial TTL / maximum hops (D8 = 4).
    pub max_hops: u8,
    pub dedup_capacity: usize,
    pub max_forward_per_second: u64,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            interfaces: vec!["default".to_string()],
            allowed_networks: Vec::new(),
            fingerprint_window_ms: 5_000,
            max_hops: 4,
            dedup_capacity: 4096,
            max_forward_per_second: 2_000,
        }
    }
}

/// A decoded relay frame: the original DNS message (without the OPT trailer)
/// and the remaining TTL. A plain mDNS packet has no trailer and starts with a
/// full budget, which keeps the relay interoperable with standard responders.
#[derive(Debug, Clone)]
pub struct Frame {
    pub base: Vec<u8>,
    pub ttl: Option<u8>,
}

pub fn fingerprint(bytes: &[u8]) -> u64 {
    let digest = sha256(bytes);
    u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ])
}

fn trailer(ttl: u8) -> [u8; TRAILER_LEN] {
    let ttl_field = ((ttl as u32) << 24) | RELAY_MAGIC;
    [
        0x00, // root name
        0x00,
        0x29, // TYPE = OPT (41)
        0x10,
        0x00, // CLASS = 4096
        (ttl_field >> 24) as u8,
        (ttl_field >> 16) as u8,
        (ttl_field >> 8) as u8,
        ttl_field as u8,
        0x00,
        0x00, // RDLENGTH = 0
    ]
}

fn is_trailer(bytes: &[u8]) -> bool {
    bytes.len() == TRAILER_LEN
        && bytes[0] == 0x00
        && bytes[1] == 0x00
        && bytes[2] == 0x29
        && bytes[3] == 0x10
        && bytes[4] == 0x00
        && bytes[9] == 0x00
        && bytes[10] == 0x00
        && (u32::from_be_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]) & 0x00FF_FFFF)
            == RELAY_MAGIC
}

/// Strip our OPT trailer and restore ARCOUNT; outbound frames add it back.
pub fn decode_frame(data: &[u8]) -> Result<Frame, ()> {
    if data.len() < 12 {
        return Err(());
    }
    if data.len() >= 12 + TRAILER_LEN && is_trailer(&data[data.len() - TRAILER_LEN..]) {
        let tail = &data[data.len() - TRAILER_LEN..];
        let ttl = tail[5];
        let mut base = data[..data.len() - TRAILER_LEN].to_vec();
        let arcount = u16::from_be_bytes([base[10], base[11]]).saturating_sub(1);
        base[10..12].copy_from_slice(&arcount.to_be_bytes());
        Ok(Frame {
            base,
            ttl: Some(ttl),
        })
    } else {
        Ok(Frame {
            base: data.to_vec(),
            ttl: None,
        })
    }
}

pub fn encode_frame(base: &[u8], ttl: u8) -> Result<Vec<u8>, ()> {
    if base.len() < 12 {
        return Err(());
    }
    let mut out = base.to_vec();
    let arcount = u16::from_be_bytes([out[10], out[11]]).saturating_add(1);
    out[10..12].copy_from_slice(&arcount.to_be_bytes());
    out.extend_from_slice(&trailer(ttl));
    Ok(out)
}

/// Bounded time window. Expired entries are purged on every insert; when the
/// capacity is hit the oldest entry is evicted. `now` is injectable so the
/// storm test can simulate 60 seconds without sleeping.
#[derive(Debug)]
pub struct FingerprintWindow {
    window: Duration,
    capacity: usize,
    entries: HashMap<u64, Instant>,
    order: VecDeque<(u64, Instant)>,
}

impl FingerprintWindow {
    pub fn new(window_ms: u64, capacity: usize) -> Self {
        Self {
            window: Duration::from_millis(window_ms.max(1)),
            capacity: capacity.max(1),
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn purge(&mut self, now: Instant) {
        while let Some((fp, at)) = self.order.front().copied() {
            if now.duration_since(at) < self.window {
                break;
            }
            self.order.pop_front();
            if self.entries.get(&fp) == Some(&at) {
                self.entries.remove(&fp);
            }
        }
    }

    /// Returns true when the fingerprint is already present (a duplicate).
    pub fn seen_or_insert(&mut self, fp: u64, now: Instant) -> bool {
        self.purge(now);
        if self.entries.contains_key(&fp) {
            return true;
        }
        self.entries.insert(fp, now);
        self.order.push_back((fp, now));
        while self.entries.len() > self.capacity {
            if let Some((old, at)) = self.order.pop_front() {
                if self.entries.get(&old) == Some(&at) {
                    self.entries.remove(&old);
                }
            } else {
                break;
            }
        }
        false
    }

    pub fn mark(&mut self, fp: u64, now: Instant) {
        let _ = self.seen_or_insert(fp, now);
    }

    pub fn contains(&self, fp: u64) -> bool {
        self.entries.contains_key(&fp)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Cidr {
    network: u32,
    mask: u32,
}

impl Cidr {
    pub fn parse(text: &str) -> Option<Self> {
        let (addr, bits) = text.trim().split_once('/')?;
        let ip: Ipv4Addr = addr.parse().ok()?;
        let bits: u32 = bits.parse().ok()?;
        if bits > 32 {
            return None;
        }
        let mask = if bits == 0 {
            0
        } else {
            u32::MAX << (32 - bits)
        };
        Some(Self {
            network: u32::from(ip) & mask,
            mask,
        })
    }

    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        u32::from(ip) & self.mask == self.network
    }
}

pub struct LoopGuard {
    pub cfg: GuardConfig,
    dedup: FingerprintWindow,
    local: FingerprintWindow,
    cidrs: Vec<Cidr>,
    pub counters: Counters,
    second_start: Instant,
    second_forwards: u64,
}

impl LoopGuard {
    pub fn new(cfg: GuardConfig) -> Self {
        let window = cfg.fingerprint_window_ms;
        let capacity = cfg.dedup_capacity;
        let cidrs = cfg
            .allowed_networks
            .iter()
            .filter_map(|text| Cidr::parse(text))
            .collect();
        Self {
            cfg,
            dedup: FingerprintWindow::new(window, capacity),
            local: FingerprintWindow::new(window, capacity),
            cidrs,
            counters: Counters::default(),
            second_start: Instant::now(),
            second_forwards: 0,
        }
    }

    pub fn dedup_len(&self) -> usize {
        self.dedup.len()
    }

    /// Hot reload keeps counters and the de-duplication window intact.
    pub fn reconfigure(&mut self, cfg: GuardConfig) {
        self.cidrs = cfg
            .allowed_networks
            .iter()
            .filter_map(|text| Cidr::parse(text))
            .collect();
        self.cfg = cfg;
    }

    /// Register a packet produced by this process. It must never be relayed,
    /// even if another interface receives a reflected copy (PRD §4.3 item 4).
    pub fn mark_self_generated(&mut self, data: &[u8], now: Instant) {
        if let Ok(frame) = decode_frame(data) {
            let fp = fingerprint(&frame.base);
            self.local.mark(fp, now);
            self.dedup.mark(fp, now);
        }
    }

    pub fn allow_rate(&mut self, now: Instant) -> bool {
        if now.duration_since(self.second_start) >= Duration::from_secs(1) {
            self.second_start = now;
            self.second_forwards = 0;
        }
        if self.second_forwards >= self.cfg.max_forward_per_second {
            false
        } else {
            self.second_forwards += 1;
            true
        }
    }

    pub fn decide(
        &mut self,
        data: &[u8],
        ingress: &str,
        source: Ipv4Addr,
        self_claimed: bool,
        now: Instant,
    ) -> Verdict {
        self.counters.received += 1;
        let frame = match decode_frame(data) {
            Ok(value) => value,
            Err(()) => {
                self.counters.malformed += 1;
                return Verdict::Malformed;
            }
        };
        let fp = fingerprint(&frame.base);

        // Guard 4: locally generated packets (and their reflections).
        if self_claimed || self.local.contains(fp) {
            self.counters.self_generated += 1;
            return Verdict::SelfGenerated;
        }

        // Whitelist: source network must be allowed when a list is configured.
        if !self.cidrs.is_empty() && !self.cidrs.iter().any(|cidr| cidr.contains(source)) {
            self.counters.outside_whitelist += 1;
            return Verdict::OutsideWhitelist;
        }

        // Guard 3: TTL decrements on every forward; zero means drop.
        let ttl = frame.ttl.unwrap_or(self.cfg.max_hops);
        if ttl == 0 {
            self.counters.ttl_exceeded += 1;
            return Verdict::TtlExceeded;
        }

        // Guard 1: short-window content fingerprint de-duplication.
        if self.dedup.seen_or_insert(fp, now) {
            self.counters.duplicate += 1;
            return Verdict::Duplicate;
        }

        // Guard 2: never egress on the interface the packet arrived on.
        let targets: Vec<String> = self
            .cfg
            .interfaces
            .iter()
            .filter(|name| name.as_str() != ingress)
            .cloned()
            .collect();
        if targets.is_empty() {
            self.counters.same_interface += 1;
            return Verdict::SameInterface;
        }

        if !self.allow_rate(now) {
            self.counters.rate_limited += 1;
            return Verdict::RateLimited;
        }

        let next_ttl = ttl - 1;
        match encode_frame(&frame.base, next_ttl) {
            Ok(out) => {
                self.counters.forwarded += 1;
                self.counters.bytes_forwarded += out.len() as u64;
                Verdict::Forward {
                    ttl: next_ttl,
                    targets,
                    frame: out,
                }
            }
            Err(()) => {
                self.counters.malformed += 1;
                Verdict::Malformed
            }
        }
    }

    pub fn status_json(&self) -> String {
        let c = self.counters;
        format!(
            "{{\"interfaces\":{},\"allowed_networks\":{},\"fingerprint_window_ms\":{},\"max_hops\":{},\"dedup_capacity\":{},\"dedup_entries\":{},\"max_forward_per_second\":{},\"received\":{},\"forwarded\":{},\"duplicate\":{},\"same_interface\":{},\"ttl_exceeded\":{},\"self_generated\":{},\"outside_whitelist\":{},\"rate_limited\":{},\"malformed\":{},\"bytes_forwarded\":{}}}",
            json_array(&self.cfg.interfaces),
            json_array(&self.cfg.allowed_networks),
            self.cfg.fingerprint_window_ms,
            self.cfg.max_hops,
            self.cfg.dedup_capacity,
            self.dedup.len(),
            self.cfg.max_forward_per_second,
            c.received,
            c.forwarded,
            c.duplicate,
            c.same_interface,
            c.ttl_exceeded,
            c.self_generated,
            c.outside_whitelist,
            c.rate_limited,
            c.malformed,
            c.bytes_forwarded,
        )
    }
}

fn json_array(items: &[String]) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|item| common_api::json_quote(item))
        .collect();
    format!("[{}]", parts.join(","))
}

/// Minimal DNS query used by tests: header + one A question for `nas.local`.
#[cfg(test)]
fn test_query() -> Vec<u8> {
    let mut bytes = vec![
        0x00, 0x00, // ID
        0x00, 0x00, // flags
        0x00, 0x01, // QDCOUNT
        0x00, 0x00, // ANCOUNT
        0x00, 0x00, // NSCOUNT
        0x00, 0x00, // ARCOUNT
    ];
    for label in ["nas", "local"] {
        bytes.push(label.len() as u8);
        bytes.extend_from_slice(label.as_bytes());
    }
    bytes.push(0x00);
    bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
    bytes
}

#[cfg(test)]
fn config() -> GuardConfig {
    GuardConfig {
        interfaces: vec!["eth0".into(), "eth1".into()],
        allowed_networks: vec!["192.168.0.0/16".into()],
        fingerprint_window_ms: 5_000,
        max_hops: 4,
        dedup_capacity: 4096,
        max_forward_per_second: 10_000,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailer_roundtrip_preserves_base_and_arcount() {
        let base = test_query();
        let frame = encode_frame(&base, 3).unwrap();
        assert_eq!(frame.len(), base.len() + TRAILER_LEN);
        assert_eq!(u16::from_be_bytes([frame[10], frame[11]]), 1);
        let decoded = decode_frame(&frame).unwrap();
        assert_eq!(decoded.base, base);
        assert_eq!(decoded.ttl, Some(3));
        assert_eq!(u16::from_be_bytes([decoded.base[10], decoded.base[11]]), 0);
        assert_eq!(decode_frame(&base).unwrap().ttl, None);
    }

    #[test]
    fn guard1_fingerprint_window_deduplicates_and_expires() {
        let mut guard = LoopGuard::new(config());
        let t0 = Instant::now();
        let packet = test_query();
        let first = guard.decide(&packet, "eth0", Ipv4Addr::new(192, 168, 1, 5), false, t0);
        assert!(matches!(first, Verdict::Forward { ttl: 3, .. }));
        let second = guard.decide(&packet, "eth1", Ipv4Addr::new(192, 168, 1, 5), false, t0);
        assert_eq!(second, Verdict::Duplicate);
        // Same content, different TTL, still a duplicate (loop reflection).
        let with_ttl = encode_frame(&packet, 1).unwrap();
        assert_eq!(
            guard.decide(&with_ttl, "eth1", Ipv4Addr::new(192, 168, 1, 5), false, t0),
            Verdict::Duplicate
        );
        // After the 5s window the same content is legitimate again.
        let later = t0 + Duration::from_millis(5_001);
        assert!(matches!(
            guard.decide(&packet, "eth0", Ipv4Addr::new(192, 168, 1, 5), false, later),
            Verdict::Forward { .. }
        ));
        assert_eq!(guard.counters.duplicate, 2);
    }

    #[test]
    fn guard2_ingress_is_never_an_egress_target() {
        let mut guard = LoopGuard::new(config());
        let t0 = Instant::now();
        let verdict = guard.decide(
            &test_query(),
            "eth0",
            Ipv4Addr::new(192, 168, 1, 5),
            false,
            t0,
        );
        match verdict {
            Verdict::Forward { targets, .. } => {
                assert_eq!(targets, vec!["eth1".to_string()]);
                assert!(!targets.contains(&"eth0".to_string()));
            }
            other => panic!("expected forward, got {:?}", other),
        }
        // A single-NIC topology has no legal egress at all.
        let single = GuardConfig {
            interfaces: vec!["eth0".into()],
            ..config()
        };
        let mut one = LoopGuard::new(single);
        assert_eq!(
            one.decide(
                &test_query(),
                "eth0",
                Ipv4Addr::new(192, 168, 1, 5),
                false,
                t0
            ),
            Verdict::SameInterface
        );
    }

    #[test]
    fn guard3_ttl_decrements_to_zero_then_drops() {
        let mut guard = LoopGuard::new(config());
        let t0 = Instant::now();
        let mut frame = test_query();
        let mut seen_ttls = Vec::new();
        for step in 0..6 {
            let side = if step % 2 == 0 { "eth0" } else { "eth1" };
            match guard.decide(&frame, side, Ipv4Addr::new(192, 168, 1, 5), false, t0) {
                Verdict::Forward {
                    ttl, frame: next, ..
                } => {
                    seen_ttls.push(ttl);
                    frame = next;
                }
                other => {
                    assert_eq!(other, Verdict::Duplicate, "reflected copies dedup first");
                    break;
                }
            }
        }
        // A packet explicitly carrying ttl=0 must be dropped, not relayed.
        let zero = encode_frame(&test_query(), 0).unwrap();
        let mut fresh = LoopGuard::new(config());
        assert_eq!(
            fresh.decide(&zero, "eth0", Ipv4Addr::new(192, 168, 1, 5), false, t0),
            Verdict::TtlExceeded
        );
        assert_eq!(fresh.counters.ttl_exceeded, 1);
        assert_eq!(seen_ttls.first(), Some(&3));
    }

    #[test]
    fn guard4_self_generated_packets_are_never_forwarded() {
        let mut guard = LoopGuard::new(config());
        let t0 = Instant::now();
        let packet = test_query();
        guard.mark_self_generated(&packet, t0);
        assert_eq!(
            guard.decide(&packet, "eth0", Ipv4Addr::new(192, 168, 1, 5), false, t0),
            Verdict::SelfGenerated
        );
        // Explicit self marker on an unregistered packet is also rejected.
        let mut other = test_query();
        other[1] = 0x01;
        assert_eq!(
            guard.decide(&other, "eth0", Ipv4Addr::new(192, 168, 1, 5), true, t0),
            Verdict::SelfGenerated
        );
        assert_eq!(guard.counters.forwarded, 0);
    }

    #[test]
    fn whitelist_and_rate_limit_are_enforced() {
        let mut guard = LoopGuard::new(config());
        let t0 = Instant::now();
        assert_eq!(
            guard.decide(&test_query(), "eth0", Ipv4Addr::new(10, 0, 0, 1), false, t0),
            Verdict::OutsideWhitelist
        );
        let limited = GuardConfig {
            max_forward_per_second: 2,
            ..config()
        };
        let mut small = LoopGuard::new(limited);
        for i in 0..4 {
            let mut packet = test_query();
            packet[0] = i;
            let verdict = small.decide(&packet, "eth0", Ipv4Addr::new(192, 168, 1, 5), false, t0);
            if i < 2 {
                assert!(matches!(verdict, Verdict::Forward { .. }));
            } else {
                assert_eq!(verdict, Verdict::RateLimited);
            }
        }
        assert_eq!(small.counters.rate_limited, 2);
    }

    #[test]
    fn fingerprint_window_is_bounded() {
        let mut window = FingerprintWindow::new(5_000, 16);
        let t0 = Instant::now();
        for i in 0..1000u64 {
            window.seen_or_insert(i, t0);
        }
        assert!(window.len() <= 16);
    }

    #[test]
    fn sixty_second_loop_storm_produces_no_broadcast_storm() {
        // Simulate a two-NIC loop: every forwarded frame is immediately fed
        // back on the opposite interface with a decreasing TTL, 1000 times per
        // second for 60 seconds. Without the guards this grows exponentially.
        let cfg = GuardConfig {
            max_forward_per_second: 2_000,
            // Must be able to hold the whole working set (1000 distinct
            // packets); an undersized window would evict legitimate entries
            // and measure capacity, not loop suppression.
            dedup_capacity: 4_096,
            ..config()
        };
        let mut guard = LoopGuard::new(cfg);
        let t0 = Instant::now();
        let mut forwards = 0u64;
        for second in 0..60u64 {
            let now = t0 + Duration::from_secs(second);
            for i in 0..1000u32 {
                let mut packet = test_query();
                packet[0] = (i & 0xff) as u8;
                packet[1] = ((i >> 8) & 0xff) as u8;
                let ingress = if i % 2 == 0 { "eth0" } else { "eth1" };
                let ttl = (i % 5) as u8;
                let frame = encode_frame(&packet, ttl).unwrap();
                if let Verdict::Forward { frame: out, .. } =
                    guard.decide(&frame, ingress, Ipv4Addr::new(192, 168, 1, 5), false, now)
                {
                    forwards += 1;
                    // Immediate reflection on the opposite NIC.
                    let reflected = if ingress == "eth0" { "eth1" } else { "eth0" };
                    let _ =
                        guard.decide(&out, reflected, Ipv4Addr::new(192, 168, 1, 5), false, now);
                }
            }
        }
        assert_eq!(guard.counters.received, 60 * 1000 + forwards);
        assert!(
            forwards <= 60 * 2_000,
            "forward ceiling exceeded: {}",
            forwards
        );
        assert!(
            guard.dedup_len() <= 4_096,
            "dedup window must stay bounded: {}",
            guard.dedup_len()
        );
        assert_eq!(guard.counters.malformed, 0);
        // Reflections are absorbed by the fingerprint window, not amplified:
        // the vast majority of injected packets never produce a new egress.
        assert!(guard.counters.duplicate > guard.counters.forwarded);
        assert!(forwards <= guard.counters.received / 4);
    }
}
