//! lan-wol — wake-on-LAN business logic (M4, PRD §4.7).
//!
//! Zero third-party crates. Devices and the bounded wake/state audit log are
//! persisted as JSON lines with an atomic `.tmp` + `rename` replacement. The
//! power-state machine is delegated to `common_probe::Debounce`: this module
//! owns the per-device map but never re-implements the debounce thresholds or
//! the post-wake grace window.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common_api::json_quote;
use common_probe::{Debounce, DeviceState};

fn q(value: &str) -> String {
    json_quote(value)
}

// ---------------------------------------------------------------------------
// Time (no date/time crate in the workspace).
// ---------------------------------------------------------------------------

/// RFC 3339 UTC timestamp for audit rows.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 -> (year, month, day), Howard Hinnant's civil calendar.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + (if mp < 10 { 3 } else { -9 });
    let year = y + (if month <= 2 { 1 } else { 0 });
    (year, month, day)
}

// ---------------------------------------------------------------------------
// Tiny JSON readers (no serde), used for our own JSON lines files.
// ---------------------------------------------------------------------------

fn value_after<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{}\"", key);
    let at = line.find(&needle)?;
    let rest = &line[at + needle.len()..];
    let colon = rest.find(':')?;
    Some(rest[colon + 1..].trim_start())
}

fn read_string(line: &str, key: &str) -> Option<String> {
    let input = value_after(line, key)?;
    let bytes = input.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut i = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some(out),
            b'\\' => {
                i += 1;
                if i >= bytes.len() {
                    return None;
                }
                match bytes[i] {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'u' => {
                        if i + 4 >= bytes.len() {
                            return None;
                        }
                        let hex = std::str::from_utf8(&bytes[i + 1..i + 5]).ok()?;
                        let code = u32::from_str_radix(hex, 16).ok()?;
                        out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        i += 4;
                    }
                    _ => return None,
                }
            }
            _ => {
                let ch = input[i..].chars().next()?;
                out.push(ch);
                i += ch.len_utf8() - 1;
            }
        }
        i += 1;
    }
    None
}

fn read_u16(line: &str, key: &str) -> Option<u16> {
    let digits: String = value_after(line, key)?
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<u16>().ok()
}

fn read_bool(line: &str, key: &str) -> Option<bool> {
    let rest = value_after(line, key)?;
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// JSON lines: write `<path>.tmp`, then atomically rename over `<path>`.
fn atomic_write(path: &str, content: &str) -> io::Result<()> {
    let tmp = format!("{}.tmp", path);
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)
}

// ---------------------------------------------------------------------------
// Business configuration (mirrors config.template.yaml, `wol.*`).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WolConfig {
    pub send_enabled: bool,
    pub broadcast: String,
    pub port: u16,
    pub devices_path: String,
    pub history_path: String,
    pub max_devices: usize,
    pub cooldown_seconds: u64,
    pub grace_seconds: u64,
    pub probe_port_default: u16,
    pub probe_timeout_ms: u64,
    pub probe_interval_seconds: u64,
    pub probe_success_threshold: u32,
    pub probe_failure_threshold: u32,
    pub history_limit: usize,
}

impl Default for WolConfig {
    fn default() -> Self {
        Self {
            send_enabled: false,
            broadcast: "255.255.255.255".to_string(),
            port: 9,
            devices_path: "lan-wol.devices.json".to_string(),
            history_path: "lan-wol.history.json".to_string(),
            max_devices: 512,
            cooldown_seconds: 30,
            grace_seconds: 90,
            probe_port_default: 22,
            probe_timeout_ms: 300,
            probe_interval_seconds: 15,
            probe_success_threshold: 3,
            probe_failure_threshold: 3,
            history_limit: 200,
        }
    }
}

// ---------------------------------------------------------------------------
// Device record + JSON lines store.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub mac: [u8; 6],
    pub ip: String,
    pub probe_port: u16,
    /// Empty means "use the global wol.broadcast".
    pub broadcast: String,
    pub enabled: bool,
    pub note: String,
    pub updated_at: String,
}

pub fn mac_to_string(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|byte| format!("{:02X}", *byte))
        .collect::<Vec<_>>()
        .join(":")
}

/// Strictly six groups of two hex digits, separated by ':' or '-'.
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let separator = if text.contains(':') { ':' } else { '-' };
    let parts: Vec<&str> = text.split(separator).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (index, part) in parts.iter().enumerate() {
        if part.len() != 2 || !part.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        out[index] = u8::from_str_radix(part, 16).ok()?;
    }
    Some(out)
}

impl Device {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"id\":{},\"name\":{},\"mac\":{},\"ip\":{},\"probe_port\":{},\"broadcast\":{},\"enabled\":{},\"note\":{},\"updated_at\":{}}}",
            q(&self.id),
            q(&self.name),
            q(&mac_to_string(&self.mac)),
            q(&self.ip),
            self.probe_port,
            q(&self.broadcast),
            self.enabled,
            q(&self.note),
            q(&self.updated_at),
        )
    }

    pub fn from_json(line: &str) -> Option<Device> {
        Some(Device {
            id: read_string(line, "id")?,
            name: read_string(line, "name").unwrap_or_default(),
            mac: parse_mac(&read_string(line, "mac")?)?,
            ip: read_string(line, "ip")?,
            probe_port: read_u16(line, "probe_port").unwrap_or(0),
            broadcast: read_string(line, "broadcast").unwrap_or_default(),
            enabled: read_bool(line, "enabled").unwrap_or(true),
            note: read_string(line, "note").unwrap_or_default(),
            updated_at: read_string(line, "updated_at").unwrap_or_default(),
        })
    }

    pub fn probe_ip(&self) -> Option<IpAddr> {
        self.ip.parse::<IpAddr>().ok()
    }

    /// Per-device broadcast override, or the global address when empty.
    pub fn broadcast_or<'a>(&'a self, global: &'a str) -> &'a str {
        if self.broadcast.is_empty() {
            global
        } else {
            &self.broadcast
        }
    }
}

pub struct DeviceStore {
    pub devices: Vec<Device>,
    pub path: String,
    pub max_devices: usize,
}

impl DeviceStore {
    pub fn open(path: &str, max_devices: usize) -> Self {
        let max_devices = max_devices.max(1);
        let mut devices = Vec::new();
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                if line.trim_start().starts_with('{') {
                    if let Some(device) = Device::from_json(line) {
                        devices.push(device);
                    }
                }
            }
        }
        devices.truncate(max_devices);
        Self {
            devices,
            path: path.to_string(),
            max_devices,
        }
    }

    pub fn get(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|device| device.id == id)
    }

    /// Insert or replace by id; returns the total after a successful write.
    pub fn upsert(&mut self, device: Device) -> Result<usize, String> {
        if device.id.trim().is_empty() {
            return Err("device id must not be empty".to_string());
        }
        if device.probe_ip().is_none() {
            return Err("device ip must be a valid IP address".to_string());
        }
        match self.devices.iter().position(|item| item.id == device.id) {
            Some(index) => self.devices[index] = device,
            None => {
                if self.devices.len() >= self.max_devices {
                    return Err(format!("device limit ({}) reached", self.max_devices));
                }
                self.devices.push(device);
            }
        }
        self.persist().map_err(|error| error.to_string())?;
        Ok(self.devices.len())
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.devices.len();
        self.devices.retain(|device| device.id != id);
        let removed = self.devices.len() != before;
        if removed {
            let _ = self.persist();
        }
        removed
    }

    fn persist(&self) -> io::Result<()> {
        let lines: Vec<String> = self.devices.iter().map(Device::to_json).collect();
        atomic_write(&self.path, &lines.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// Magic packet + UDP send.
// ---------------------------------------------------------------------------

/// 6 x 0xFF followed by 16 copies of the MAC: strictly 102 bytes.
pub fn magic_packet(mac: &[u8; 6]) -> [u8; 102] {
    let mut buf = [0u8; 102];
    buf[..6].fill(0xFF);
    for index in 0..16 {
        buf[6 + index * 6..12 + index * 6].copy_from_slice(mac);
    }
    buf
}

/// Send a magic packet to the effective broadcast address. `broadcast` may be
/// `127.0.0.1` (test isolation); we never require it to be a broadcast address.
pub fn send_magic(device: &Device, cfg: &WolConfig) -> Result<usize, String> {
    let broadcast: Ipv4Addr = device
        .broadcast_or(&cfg.broadcast)
        .parse()
        .map_err(|_| "broadcast must be an IPv4 address".to_string())?;
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|error| error.to_string())?;
    socket
        .set_broadcast(true)
        .map_err(|error| error.to_string())?;
    let target = SocketAddr::new(IpAddr::V4(broadcast), cfg.port);
    let packet = magic_packet(&device.mac);
    socket
        .send_to(&packet, target)
        .map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Cooldown (per device id).
// ---------------------------------------------------------------------------

pub struct Cooldown {
    last: HashMap<String, Instant>,
}

impl Cooldown {
    pub fn new() -> Self {
        Self {
            last: HashMap::new(),
        }
    }

    /// `true` when a repeat request inside `seconds` must be rejected.
    pub fn is_cooling(&self, id: &str, now: Instant, seconds: u64) -> bool {
        self.last
            .get(id)
            .is_some_and(|last| now.duration_since(*last) < Duration::from_secs(seconds))
    }

    pub fn mark(&mut self, id: &str, now: Instant) {
        self.last.insert(id.to_string(), now);
    }
}

// ---------------------------------------------------------------------------
// Pure wake decision: registered devices only, enable switch, then cooldown.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReject {
    Unregistered,
    Disabled,
    Cooldown,
}

pub fn check_wake(
    device: Option<&Device>,
    send_enabled: bool,
    cooling: bool,
) -> Result<(), WakeReject> {
    let device = device.ok_or(WakeReject::Unregistered)?;
    if !send_enabled || !device.enabled {
        return Err(WakeReject::Disabled);
    }
    if cooling {
        return Err(WakeReject::Cooldown);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bounded audit log (wake + state transitions). Never stores credentials.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub time: String,
    pub action: String,
    pub device_id: String,
    pub result: String,
    /// Only a fixed reason or a `from->to` state label; no credentials.
    pub detail: String,
}

impl AuditEntry {
    pub fn new(action: &str, device_id: &str, result: &str, detail: &str) -> Self {
        Self {
            time: now_rfc3339(),
            action: action.to_string(),
            device_id: device_id.to_string(),
            result: result.to_string(),
            detail: detail.to_string(),
        }
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"time\":{},\"action\":{},\"device_id\":{},\"result\":{},\"detail\":{}}}",
            q(&self.time),
            q(&self.action),
            q(&self.device_id),
            q(&self.result),
            q(&self.detail),
        )
    }

    pub fn from_json(line: &str) -> Option<AuditEntry> {
        Some(AuditEntry {
            time: read_string(line, "time").unwrap_or_default(),
            action: read_string(line, "action").unwrap_or_default(),
            device_id: read_string(line, "device_id").unwrap_or_default(),
            result: read_string(line, "result").unwrap_or_default(),
            detail: read_string(line, "detail").unwrap_or_default(),
        })
    }
}

pub struct AuditLog {
    pub entries: VecDeque<AuditEntry>,
    pub limit: usize,
    pub path: String,
}

impl AuditLog {
    pub fn open(path: &str, limit: usize) -> Self {
        let limit = limit.max(1);
        let mut entries = VecDeque::new();
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                if line.trim_start().starts_with('{') {
                    if let Some(entry) = AuditEntry::from_json(line) {
                        entries.push_back(entry);
                    }
                }
            }
        }
        while entries.len() > limit {
            entries.pop_front();
        }
        Self {
            entries,
            limit,
            path: path.to_string(),
        }
    }

    /// Memory is updated first; a persistence failure never blocks the caller.
    pub fn push(&mut self, entry: AuditEntry) {
        self.entries.push_back(entry);
        while self.entries.len() > self.limit {
            self.entries.pop_front();
        }
        let _ = self.persist();
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
        while self.entries.len() > self.limit {
            self.entries.pop_front();
        }
    }

    fn persist(&self) -> io::Result<()> {
        let lines: Vec<String> = self.entries.iter().map(AuditEntry::to_json).collect();
        atomic_write(&self.path, &lines.join("\n"))
    }

    pub fn items_json(&self) -> String {
        let items: Vec<String> = self.entries.iter().map(AuditEntry::to_json).collect();
        format!("[{}]", items.join(","))
    }
}

// ---------------------------------------------------------------------------
// Per-device power-state machines, backed by common_probe::Debounce.
// ---------------------------------------------------------------------------

pub struct DeviceStates {
    map: HashMap<String, Debounce>,
    success: u32,
    failure: u32,
}

impl DeviceStates {
    pub fn new(success: u32, failure: u32) -> Self {
        Self {
            map: HashMap::new(),
            success: success.max(1),
            failure: failure.max(1),
        }
    }

    /// Adopt new thresholds on hot reload, keeping observed state.
    pub fn reconfigure(&mut self, success: u32, failure: u32) {
        self.success = success.max(1);
        self.failure = failure.max(1);
        let (s, f) = (self.success, self.failure);
        for debounce in self.map.values_mut() {
            debounce.success_threshold = s;
            debounce.failure_threshold = f;
        }
    }

    fn entry(&mut self, id: &str) -> &mut Debounce {
        let (s, f) = (self.success, self.failure);
        self.map
            .entry(id.to_string())
            .or_insert_with(|| Debounce::new(s, f))
    }

    pub fn state_of(&self, id: &str) -> DeviceState {
        self.map
            .get(id)
            .map(|debounce| debounce.state())
            .unwrap_or(DeviceState::Unknown)
    }

    /// Hold `waking` for `grace` right after an accepted wake packet.
    pub fn mark_waking(&mut self, id: &str, grace: Duration, now: Instant) {
        self.entry(id).mark_waking(grace, now);
    }

    /// Feed one probe sample; returns (previous state, Some(new) on a flip).
    pub fn observe(
        &mut self,
        id: &str,
        reachable: bool,
        now: Instant,
    ) -> (DeviceState, Option<DeviceState>) {
        let before = self.state_of(id);
        let transition = self.entry(id).observe(reachable, now);
        (before, transition)
    }

    pub fn forget(&mut self, id: &str) {
        self.map.remove(id);
    }
}

// ---------------------------------------------------------------------------
// Unit tests (all file work uses a unique temp_dir; no LAN broadcasts).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let unique = format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(format!("lan-wol-{}-{}", tag, unique));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn sample_device() -> Device {
        Device {
            id: "dev-01".to_string(),
            name: "工作站".to_string(),
            mac: [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF],
            ip: "192.168.5.20".to_string(),
            probe_port: 22,
            broadcast: "255.255.255.255".to_string(),
            enabled: true,
            note: "三楼".to_string(),
            updated_at: "2026-11-14T01:00:00Z".to_string(),
        }
    }

    #[test]
    fn magic_packet_is_102_bytes_with_16_mac_repeats() {
        let mac = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let packet = magic_packet(&mac);
        assert_eq!(packet.len(), 102);
        assert_eq!(&packet[..6], &[0xFFu8; 6]);
        for index in 0..16 {
            assert_eq!(&packet[6 + index * 6..12 + index * 6], &mac);
        }
        assert_eq!(&packet[96..102], &mac);
    }

    #[test]
    fn parse_mac_accepts_valid_and_rejects_invalid() {
        assert_eq!(
            parse_mac("AA:BB:CC:DD:EE:FF"),
            Some([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
        );
        assert_eq!(
            parse_mac("aa-bb-cc-dd-ee-ff"),
            Some([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
        );
        for bad in [
            "AA:BB:CC:DD:EE",
            "AA:BB:CC:DD:EE:GG",
            "AA:BB:CC:DD:EE:FFF",
            "AABBCCDDEEFF",
            "AA:BB-CC:DD:EE:FF",
            "",
        ] {
            assert!(parse_mac(bad).is_none(), "should reject {bad:?}");
        }
    }

    #[test]
    fn unregistered_disabled_and_cooldown_wakes_are_rejected() {
        let device = sample_device();
        assert_eq!(check_wake(None, true, false), Err(WakeReject::Unregistered));
        assert_eq!(
            check_wake(Some(&device), false, false),
            Err(WakeReject::Disabled)
        );
        let mut disabled = device.clone();
        disabled.enabled = false;
        assert_eq!(
            check_wake(Some(&disabled), true, false),
            Err(WakeReject::Disabled)
        );
        assert_eq!(
            check_wake(Some(&device), true, true),
            Err(WakeReject::Cooldown)
        );
        assert_eq!(check_wake(Some(&device), true, false), Ok(()));
    }

    #[test]
    fn cooldown_window_is_enforced() {
        let base = Instant::now();
        let mut cooldown = Cooldown::new();
        assert!(!cooldown.is_cooling("dev-01", base, 30));
        cooldown.mark("dev-01", base);
        assert!(cooldown.is_cooling("dev-01", base + Duration::from_secs(5), 30));
        assert!(!cooldown.is_cooling("dev-01", base + Duration::from_secs(31), 30));
        assert!(!cooldown.is_cooling("dev-02", base, 30));
    }

    #[test]
    fn audit_log_is_bounded() {
        let dir = temp_dir("audit");
        let path = dir.join("history.json").to_string_lossy().into_owned();
        let mut log = AuditLog::open(&path, 3);
        for index in 0..10 {
            log.push(AuditEntry::new(
                "wake",
                "dev-01",
                "sent",
                "magic packet sent",
            ));
            assert!(log.entries.len() <= 3, "iteration {index}");
        }
        assert_eq!(log.entries.len(), 3);
        let disk = fs::read_to_string(&path).unwrap();
        assert_eq!(disk.lines().count(), 3);
        assert!(!disk.contains("password"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn device_json_roundtrip() {
        let device = sample_device();
        let line = device.to_json();
        assert_eq!(Device::from_json(&line), Some(device));
    }

    #[test]
    fn device_store_persists_lines_and_enforces_limit() {
        let dir = temp_dir("store");
        let path = dir.join("devices.json").to_string_lossy().into_owned();
        let mut store = DeviceStore::open(&path, 2);
        assert_eq!(store.upsert(sample_device()).unwrap(), 1);
        let mut second = sample_device();
        second.id = "dev-02".to_string();
        assert_eq!(store.upsert(second).unwrap(), 2);
        let mut third = sample_device();
        third.id = "dev-03".to_string();
        assert!(store.upsert(third).is_err(), "limit must be enforced");
        let mut reopened = DeviceStore::open(&path, 2);
        assert_eq!(reopened.devices.len(), 2);
        assert!(reopened.get("dev-01").is_some());
        assert!(reopened.get("missing").is_none());
        assert!(reopened.remove("dev-01"));
        assert!(!reopened.remove("dev-01"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_magic_targets_local_loopback_without_lan_broadcast() {
        let listener = match UdpSocket::bind("127.0.0.1:0") {
            Ok(value) => value,
            Err(_) => return, // sandbox without loopback sockets
        };
        let port = listener.local_addr().unwrap().port();
        let cfg = WolConfig {
            send_enabled: true,
            broadcast: "127.0.0.1".to_string(),
            port,
            ..WolConfig::default()
        };
        let mut device = sample_device();
        device.broadcast = String::new(); // use the global loopback address
        let sent = send_magic(&device, &cfg).unwrap();
        assert_eq!(sent, 102);
        let mut buf = [0u8; 256];
        listener
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let (len, _) = listener.recv_from(&mut buf).unwrap();
        assert_eq!(len, 102);
        assert_eq!(&buf[..6], &[0xFFu8; 6]);
        assert_eq!(&buf[6..12], &device.mac);
        assert_eq!(&buf[96..102], &device.mac);
    }
}
