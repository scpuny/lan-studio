//! SSDP / UPnP **read-only discovery** primitives (M5 · PRD §4.9).
//!
//! READ-ONLY DISCOVERY: 本模块没有 HTTP 客户端、没有 SOAP 调用、不抓设备描述 XML。
//! `LOCATION` 头只作为有界属性登记，绝不发起任何 HTTP GET/POST，也绝不调用设备
//! 控制接口。报文只做解析与规范化，统一输出 `contracts/service-record.json` 模型。
//!
//! All parsing, record shaping and the device table are pure and clock-injectable:
//! no socket, no filesystem and no global clock is touched here, which keeps the
//! unit tests offline (they never emit multicast onto a real LAN).

use std::collections::{BTreeMap, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

use common_api::json_quote;

/// Standard active discovery request. Sent at most once per configured
/// interval (and once per explicit `refresh`), and only in multicast mode.
pub const M_SEARCH: &str = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: ssdp:all\r\n\r\n";

// Bounded field lengths. The contract caps name/type at 253, address at 64 and
// ttl at 86400; attributes are a flat whitelist whose values are all clipped.
const NAME_MAX: usize = 253;
const TYPE_MAX: usize = 253;
const ADDRESS_MAX: usize = 64;
const ATTR_VALUE_MAX: usize = 256;
const LAST_SEEN_MAX: usize = 64;
const TTL_MAX: u64 = 86_400;

// ---------------------------------------------------------------------------
// Configuration (flat keys come from `config.template.yaml`).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SsdpConfig {
    pub group: String,
    pub port: u16,
    pub search_interval_seconds: u64,
    pub max_age_seconds: u64,
    pub max_devices: usize,
    pub socket_timeout_ms: u64,
    pub results_path: String,
    pub fetch_descriptions: bool,
}

// ---------------------------------------------------------------------------
// Unified service record (contracts/service-record.json).
// ---------------------------------------------------------------------------

/// Exactly the eight properties of `service-record.json`; `source` is always
/// `"ssdp"` and `attributes` is a flat, whitelisted, length-bounded map.
#[derive(Debug, Clone)]
pub struct ServiceRecord {
    pub name: String,
    pub r#type: String,
    pub address: String,
    pub port: u16,
    pub attributes: BTreeMap<String, String>,
    pub ttl: u64,
    pub last_seen: String,
    pub source: String,
}

impl ServiceRecord {
    /// Hand-written JSON (no serde). `json_quote` escapes every string, so a
    /// hostile USN / SERVER / LOCATION value can never break the envelope.
    pub fn to_json(&self) -> String {
        let attrs: Vec<String> = self
            .attributes
            .iter()
            .map(|(key, value)| format!("{}:{}", json_quote(key), json_quote(value)))
            .collect();
        format!(
            "{{\"name\":{},\"type\":{},\"address\":{},\"port\":{},\"attributes\":{{{}}},\"ttl\":{},\"last_seen\":{},\"source\":{}}}",
            json_quote(&self.name),
            json_quote(&self.r#type),
            json_quote(&self.address),
            self.port,
            attrs.join(","),
            self.ttl.min(TTL_MAX),
            json_quote(&self.last_seen),
            json_quote(&self.source),
        )
    }
}

/// A byebye is a separate signal, never a `ServiceRecord`.
#[derive(Debug)]
pub enum NotifyOutcome {
    Alive(ServiceRecord),
    ByeBye(String),
}

/// USN-keyed, capacity-bounded device table. `seen_epoch` is kept outside the
/// record so `to_json` stays exactly the contract shape.
#[derive(Debug)]
struct Entry {
    record: ServiceRecord,
    seen_epoch: u64,
}

#[derive(Debug)]
pub struct DeviceTable {
    entries: HashMap<String, Entry>,
    max_devices: usize,
}

impl DeviceTable {
    pub fn new(max_devices: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_devices,
        }
    }

    /// Insert or refresh a record. Returns false (drops the record) when a new
    /// USN would exceed the configured capacity.
    pub fn upsert(&mut self, record: ServiceRecord, seen_epoch: u64) -> bool {
        let key = match record.attributes.get("usn") {
            Some(value) if !value.is_empty() => value.clone(),
            _ => return false,
        };
        if !self.entries.contains_key(&key) && self.entries.len() >= self.max_devices {
            return false;
        }
        self.entries.insert(key, Entry { record, seen_epoch });
        true
    }

    /// `ssdp:byebye` removes the USN immediately, regardless of its TTL.
    pub fn byebye(&mut self, usn: &str) -> bool {
        self.entries.remove(usn).is_some()
    }

    /// Pure, clock-injectable TTL sweep. `now_epoch` / `max_age` are supplied by
    /// the caller so tests can freeze time; returns how many rows were removed.
    pub fn sweep(&mut self, now_epoch: u64, max_age: u64) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|_, entry| now_epoch.saturating_sub(entry.seen_epoch) < max_age);
        before - self.entries.len()
    }

    pub fn records(&self) -> Vec<ServiceRecord> {
        let mut out: Vec<ServiceRecord> = self
            .entries
            .values()
            .map(|entry| entry.record.clone())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }

    pub fn max_devices(&self) -> usize {
        self.max_devices
    }

    pub fn set_max_devices(&mut self, max_devices: usize) {
        self.max_devices = max_devices;
    }
}

// ---------------------------------------------------------------------------
// Parsing and packet construction.
// ---------------------------------------------------------------------------

/// Build a bounded M-SEARCH for an optional search target. `st` is sanitized to
/// printable ASCII without CR/LF/quotes, so a request body can never inject new
/// SSDP headers or a forged body.
pub fn m_search_text(st: &str) -> String {
    let mut clean: String = st
        .chars()
        .filter(|ch| ch.is_ascii() && !ch.is_ascii_control() && *ch != '"')
        .take(128)
        .collect();
    if clean.trim().is_empty() {
        clean = "ssdp:all".to_string();
    }
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {}\r\n\r\n",
        clean
    )
}

/// Parse a `NOTIFY` / M-SEARCH `HTTP/1.x 200` reply into a unified record.
///
/// `address` / `port` / `ttl` / `last_seen` come from the receive context (the
/// contract needs them but the wire text does not carry all of them). Malformed
/// packets (bad start line, missing USN, missing NT/ST) return `None`; a
/// `ssdp:byebye` returns the dedicated `NotifyOutcome::ByeBye` signal.
pub fn parse_notify(
    text: &str,
    address: &str,
    port: u16,
    ttl: u64,
    last_seen: &str,
) -> Option<NotifyOutcome> {
    let mut lines = text.split('\n');
    let start = lines.next()?.trim();
    let is_notify = start.starts_with("NOTIFY ") && start.contains("HTTP/1.");
    let is_reply = start.starts_with("HTTP/1.") && start.split_whitespace().nth(1) == Some("200");
    if !is_notify && !is_reply {
        return None;
    }

    let mut usn: Option<String> = None;
    let mut st: Option<String> = None;
    let mut nts: Option<String> = None;
    let mut server: Option<String> = None;
    let mut cache_control: Option<String> = None;
    let mut location: Option<String> = None;
    for line in lines {
        let line = line.trim_end_matches('\r');
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "usn" => usn = Some(value.to_string()),
            "nt" | "st" => {
                if st.is_none() {
                    st = Some(value.to_string());
                }
            }
            "nts" => nts = Some(value.to_string()),
            "server" => server = Some(value.to_string()),
            "cache-control" => cache_control = Some(value.to_string()),
            "location" => location = Some(value.to_string()),
            _ => {}
        }
    }

    let usn = usn.filter(|value| !value.is_empty())?;
    let usn = truncate_chars(&usn, ATTR_VALUE_MAX);
    if nts
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("ssdp:byebye"))
    {
        return Some(NotifyOutcome::ByeBye(usn));
    }

    let st = st.filter(|value| !value.is_empty())?;
    let mut attributes = BTreeMap::new();
    attributes.insert("usn".to_string(), usn.clone());
    attributes.insert("st".to_string(), truncate_chars(&st, ATTR_VALUE_MAX));
    if let Some(value) = server {
        attributes.insert("server".to_string(), truncate_chars(&value, ATTR_VALUE_MAX));
    }
    if let Some(value) = cache_control {
        attributes.insert(
            "cache-control".to_string(),
            truncate_chars(&value, ATTR_VALUE_MAX),
        );
    }
    if let Some(value) = location {
        // READ-ONLY: only recorded, never dereferenced / fetched / invoked.
        attributes.insert(
            "location".to_string(),
            truncate_chars(&value, ATTR_VALUE_MAX),
        );
    }

    Some(NotifyOutcome::Alive(ServiceRecord {
        name: truncate_chars(&usn, NAME_MAX),
        r#type: truncate_chars(&st, TYPE_MAX),
        address: truncate_chars(address, ADDRESS_MAX),
        port,
        attributes,
        ttl: ttl.min(TTL_MAX),
        last_seen: truncate_chars(last_seen, LAST_SEEN_MAX),
        source: "ssdp".to_string(),
    }))
}

/// RFC 3339 UTC timestamp, `Z`-terminated, with no date/time crate.
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

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

// ---------------------------------------------------------------------------
// Offline unit tests. No socket, no filesystem, no multicast on a real LAN.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-11-14T02:10:00Z";

    fn alive_text(usn: &str, nt: &str) -> String {
        format!(
            "NOTIFY * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nNT: {}\r\nNTS: ssdp:alive\r\nUSN: {}\r\nLOCATION: http://192.168.5.42:8080/desc.xml\r\nSERVER: Linux/3.0 UPnP/1.0\r\nCACHE-CONTROL: max-age=1800\r\n\r\n",
            nt, usn
        )
    }

    #[test]
    fn parse_notify_alive_builds_a_complete_contract_record() {
        let nt = "urn:schemas-upnp-org:device:MediaRenderer:1";
        let usn = format!("uuid:abc-123::{}", nt);
        let text = alive_text(&usn, nt);
        match parse_notify(&text, "192.168.5.42", 1900, 1800, NOW) {
            Some(NotifyOutcome::Alive(rec)) => {
                assert_eq!(rec.name, usn);
                assert_eq!(rec.r#type, nt);
                assert_eq!(rec.address, "192.168.5.42");
                assert_eq!(rec.port, 1900);
                assert_eq!(rec.ttl, 1800);
                assert_eq!(rec.source, "ssdp");
                assert!(rec.last_seen.ends_with('Z'));
                assert_eq!(rec.attributes.get("usn").unwrap().as_str(), usn.as_str());
                assert_eq!(rec.attributes.get("st").unwrap().as_str(), nt);
                assert!(rec.attributes.contains_key("server"));
                assert!(rec.attributes.contains_key("cache-control"));
                assert!(rec.attributes.contains_key("location"));
                let json = rec.to_json();
                for key in [
                    "\"name\":",
                    "\"type\":",
                    "\"address\":",
                    "\"port\":",
                    "\"attributes\":",
                    "\"ttl\":",
                    "\"last_seen\":",
                    "\"source\":",
                ] {
                    assert!(json.contains(key), "missing contract key {key}");
                }
                assert!(json.contains("\"source\":\"ssdp\""));
                assert!(!json.contains("\"state\":"));
                assert!(!json.contains("\"control_url\":"));
            }
            other => panic!("expected Alive, got {other:?}"),
        }
    }

    #[test]
    fn parse_notify_m_search_reply_uses_st_and_only_records_location() {
        let reply = "HTTP/1.1 200 OK\r\n\
            ST: urn:schemas-upnp-org:service:ContentDirectory:1\r\n\
            USN: uuid:def-456::urn:schemas-upnp-org:service:ContentDirectory:1\r\n\
            LOCATION: http://192.168.5.77:8200/rootDesc.xml\r\n\
            SERVER: MediaTomb/0.12\r\n\
            CACHE-CONTROL: max-age=1200\r\n\r\n";
        match parse_notify(reply, "192.168.5.77", 1900, 1800, NOW) {
            Some(NotifyOutcome::Alive(rec)) => {
                assert_eq!(
                    rec.r#type,
                    "urn:schemas-upnp-org:service:ContentDirectory:1"
                );
                assert_eq!(
                    rec.attributes.get("location").unwrap().as_str(),
                    "http://192.168.5.77:8200/rootDesc.xml"
                );
                // LOCATION is an attribute only: no control/description field is emitted.
                let json = rec.to_json();
                assert!(json.contains("\"location\":"));
                assert!(!json.contains("\"control_url\":"));
                assert!(!json.contains("\"description\":"));
            }
            other => panic!("expected Alive, got {other:?}"),
        }
    }

    #[test]
    fn parse_notify_byebye_is_a_separate_signal() {
        let text = "NOTIFY * HTTP/1.1\r\n\
            NT: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\
            NTS: ssdp:byebye\r\n\
            USN: uuid:abc-123\r\n\r\n";
        assert!(matches!(
            parse_notify(text, "127.0.0.1", 1900, 1800, NOW),
            Some(NotifyOutcome::ByeBye(usn)) if usn == "uuid:abc-123"
        ));
    }

    #[test]
    fn parse_notify_rejects_malformed_packets() {
        // Bad start line.
        assert!(parse_notify(
            "HELLO * HTTP/1.1\r\nUSN: x\r\n",
            "127.0.0.1",
            1900,
            1800,
            NOW
        )
        .is_none());
        // Missing USN.
        assert!(parse_notify(
            "NOTIFY * HTTP/1.1\r\nNT: urn:x\r\nNTS: ssdp:alive\r\n",
            "127.0.0.1",
            1900,
            1800,
            NOW
        )
        .is_none());
        // Missing both NT and ST.
        assert!(parse_notify(
            "NOTIFY * HTTP/1.1\r\nUSN: uuid:x\r\nNTS: ssdp:alive\r\n",
            "127.0.0.1",
            1900,
            1800,
            NOW
        )
        .is_none());
        // byebye without USN is malformed too.
        assert!(parse_notify(
            "NOTIFY * HTTP/1.1\r\nNTS: ssdp:byebye\r\n",
            "127.0.0.1",
            1900,
            1800,
            NOW
        )
        .is_none());
    }

    #[test]
    fn service_record_json_escapes_hostile_values() {
        // A real '\n' inside a header value would be split by the line parser,
        // so use quote / backslash / tab (all survive header parsing) here.
        let hostile = "uuid:quote\"and\\slash\ttab";
        let text = alive_text(hostile, "urn:x");
        let rec = match parse_notify(&text, "127.0.0.1", 1900, 1800, NOW) {
            Some(NotifyOutcome::Alive(rec)) => rec,
            other => panic!("expected Alive, got {other:?}"),
        };
        let json = rec.to_json();
        assert!(json.contains("\\\""));
        assert!(json.contains("\\\\"));
        assert!(json.contains("\\t"));
        // No raw control characters survive into the JSON body.
        assert!(!json.contains('\n'));
        assert!(!json.contains('\r'));
        assert!(!json.contains('\t'));
        assert!(json.contains("\"port\":1900"));
    }

    #[test]
    fn attribute_values_and_record_fields_are_bounded() {
        let long = "X".repeat(600);
        let text = format!(
            "NOTIFY * HTTP/1.1\r\nNT: {}\r\nUSN: {}\r\nSERVER: {}\r\nLOCATION: {}\r\nCACHE-CONTROL: {}\r\n\r\n",
            long, long, long, long, long
        );
        let rec = match parse_notify(&text, &long, 1900, 99_999, NOW) {
            Some(NotifyOutcome::Alive(rec)) => rec,
            other => panic!("expected Alive, got {other:?}"),
        };
        assert_eq!(rec.name.chars().count(), NAME_MAX);
        assert_eq!(rec.r#type.chars().count(), TYPE_MAX);
        assert_eq!(rec.address.chars().count(), ADDRESS_MAX);
        assert_eq!(rec.ttl, TTL_MAX);
        for value in rec.attributes.values() {
            assert!(value.chars().count() <= ATTR_VALUE_MAX);
        }
        assert_eq!(rec.attributes.len(), 5);
    }

    #[test]
    fn device_table_sweep_expires_and_counts() {
        let mut table = DeviceTable::new(10);
        for (index, epoch) in [100u64, 200, 300].iter().enumerate() {
            let usn = format!("uuid:{}", index);
            let text = alive_text(&usn, "urn:x");
            if let Some(NotifyOutcome::Alive(rec)) =
                parse_notify(&text, "127.0.0.1", 1900, 1800, NOW)
            {
                assert!(table.upsert(rec, *epoch));
            }
        }
        // now=250, max_age=100: epoch 100 (age 150) expires; 200 (age 50) and
        // 300 (clock ahead, saturating to 0) are retained.
        assert_eq!(table.sweep(250, 100), 1);
        assert_eq!(table.count(), 2);
        // Clock skew (now behind seen_epoch) must never wipe the table.
        assert_eq!(table.sweep(0, 100), 0);
        assert_eq!(table.count(), 2);
        // now=400: both remaining rows (age 200 / 100) expire.
        assert_eq!(table.sweep(400, 100), 2);
        assert_eq!(table.count(), 0);
    }

    #[test]
    fn device_table_byebye_removes_immediately() {
        let mut table = DeviceTable::new(8);
        let text = alive_text("uuid:gone", "urn:x");
        if let Some(NotifyOutcome::Alive(rec)) = parse_notify(&text, "127.0.0.1", 1900, 1800, NOW) {
            assert!(table.upsert(rec, 100));
        }
        assert_eq!(table.count(), 1);
        assert!(table.byebye("uuid:gone"));
        assert_eq!(table.count(), 0);
        assert!(!table.byebye("uuid:gone"));
    }

    #[test]
    fn device_table_capacity_is_bounded_and_dedupes_by_usn() {
        let mut table = DeviceTable::new(2);
        for index in 0..5 {
            let usn = format!("uuid:{}", index);
            let text = alive_text(&usn, "urn:x");
            if let Some(NotifyOutcome::Alive(rec)) =
                parse_notify(&text, "127.0.0.1", 1900, 1800, NOW)
            {
                let _ = table.upsert(rec, 100);
            }
        }
        assert_eq!(table.count(), 2);
        assert_eq!(table.max_devices(), 2);
        // Re-posting an existing USN refreshes in place, never grows the table.
        let text = alive_text("uuid:0", "urn:x");
        if let Some(NotifyOutcome::Alive(rec)) = parse_notify(&text, "127.0.0.1", 1900, 1800, NOW) {
            assert!(table.upsert(rec, 200));
        }
        assert_eq!(table.count(), 2);
    }

    #[test]
    fn m_search_text_matches_constant_and_blocks_injection() {
        assert_eq!(m_search_text("ssdp:all"), M_SEARCH);
        assert!(M_SEARCH.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(M_SEARCH.contains("HOST: 239.255.255.250:1900\r\n"));
        assert!(M_SEARCH.contains("MAN: \"ssdp:discover\"\r\n"));
        assert!(M_SEARCH.contains("MX: 2\r\n"));
        assert!(M_SEARCH.contains("ST: ssdp:all\r\n"));
        let injected = m_search_text("urn:x\r\nEXT: forged\r\nBODY: evil");
        // CR/LF are stripped, so no extra SSDP header line can be injected:
        // the CRLF count stays identical to the canonical request.
        assert_eq!(
            injected.matches("\r\n").count(),
            M_SEARCH.matches("\r\n").count()
        );
        assert!(injected.contains("urn:xEXT: forgedBODY: evil"));
        assert_eq!(m_search_text(""), M_SEARCH);
        assert_eq!(m_search_text("   "), M_SEARCH);
    }

    #[test]
    fn now_rfc3339_is_utc_and_z_terminated() {
        let stamp = now_rfc3339();
        assert_eq!(stamp.len(), 20);
        assert!(stamp.ends_with('Z'));
        assert_eq!(&stamp[4..5], "-");
        assert_eq!(&stamp[10..11], "T");
        assert_eq!(&stamp[13..14], ":");
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        assert_eq!(civil_from_days(365), (1971, 1, 1));
    }
}
