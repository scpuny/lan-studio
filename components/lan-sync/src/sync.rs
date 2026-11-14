//! lan-sync — multi-node configuration synchronisation primitives (M5, PRD §4.8).
//!
//! Zero third-party crates. Everything is hand-written JSON (no serde) and
//! persisted as JSON lines with an atomic `.tmp` + `rename` replacement. The
//! component only ever speaks the **standard** `/api/config/snapshot` contract
//! (contracts/snapshot.json); it never calls a component-specific endpoint.
//!
//! Credentials boundary: a `Node.token`, the `shared_secret` and admin
//! passwords are NEVER written into logs, metrics, audit rows, snapshots,
//! status output or error responses. The token is only ever forwarded as an
//! HTTP request header; `public_json()` is the only representation that may
//! cross an API response, and it deliberately drops the token field.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common_api::json_quote;
use common_component::json_str;
use common_crypto::{constant_eq, hmac_sha256, sha256_hex, to_hex};

fn q(value: &str) -> String {
    json_quote(value)
}

/// The eight first-party component identifiers allowed in
/// `sync.allowed_components` (PRD §2.2: no ninth component, no Shell entry).
pub const STANDARD_COMPONENTS: [&str; 8] = [
    "lan-mdns",
    "lan-relay",
    "lan-ca",
    "lan-scan",
    "lan-hosts",
    "lan-wol",
    "lan-sync",
    "lan-ssdp",
];

// ---------------------------------------------------------------------------
// Time (no date/time crate in the workspace). Copied from lan-wol's wol.rs.
// ---------------------------------------------------------------------------

/// RFC 3339 UTC timestamp for audit rows and exported snapshots.
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
// Tiny JSON readers (no serde). Strings are read with common_component::json_str
// (it finds the key anywhere, so a snapshot nested in a `data` envelope parses
// too); numbers and booleans are read here.
// ---------------------------------------------------------------------------

/// Read a JSON boolean, returning `None` when the key is absent.
pub fn json_bool_opt(body: &str, key: &str) -> Option<bool> {
    let compact: String = body.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.contains(&format!("\"{}\":true", key)) {
        Some(true)
    } else if compact.contains(&format!("\"{}\":false", key)) {
        Some(false)
    } else {
        None
    }
}

/// Read a JSON unsigned integer (e.g. the snapshot `schema` field).
fn json_u64(body: &str, key: &str) -> Option<u64> {
    if let Some(text) = json_str(body, key) {
        if let Ok(value) = text.parse::<u64>() {
            return Some(value);
        }
    }
    let needle = format!("\"{}\"", key);
    let at = body.find(&needle)?;
    let rest = &body[at + needle.len()..];
    let colon = rest.find(':')?;
    let digits: String = rest[colon + 1..]
        .trim_start()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<u64>().ok()
}

/// JSON lines: write `<path>.tmp`, then atomically rename over `<path>`.
fn atomic_write(path: &str, content: &str) -> io::Result<()> {
    let tmp = format!("{}.tmp", path);
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)
}

// ---------------------------------------------------------------------------
// Conflict strategy and decision (PRD §4.8 S3, docs/M5-PLAN.md §7.2).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Default: never silently overwrite a newer/equal local snapshot.
    Reject,
    /// Explicit opt-in: always overwrite.
    LastWriteWins,
    /// Explicit opt-in: queue for manual confirmation.
    Manual,
}

impl Strategy {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "reject" => Some(Strategy::Reject),
            "last_write_wins" => Some(Strategy::LastWriteWins),
            "manual" => Some(Strategy::Manual),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::Reject => "reject",
            Strategy::LastWriteWins => "last_write_wins",
            Strategy::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Accepted,
    Rejected,
    Pending,
    Overwritten,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Accepted => "accepted",
            Decision::Rejected => "rejected",
            Decision::Pending => "pending",
            Decision::Overwritten => "overwritten",
        }
    }
}

// ---------------------------------------------------------------------------
// Business configuration (mirrors config.template.yaml, `sync.*`).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SyncConfig {
    pub nodes_path: String,
    pub history_path: String,
    pub max_nodes: usize,
    pub strategy: Strategy,
    pub request_timeout_ms: u64,
    /// Empty disables node signing. Never logged / exported / audited.
    pub shared_secret: String,
    pub history_limit: usize,
    pub allowed_components: Vec<String>,
}

// ---------------------------------------------------------------------------
// Node registry. `token` is a per-node credential: written to disk but NEVER
// returned by the API (`public_json`) and never put in logs/metrics/audit.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    /// `http://127.0.0.1:18086` — loopback only, never https / non-loopback.
    pub endpoint: String,
    pub token: String,
    pub enabled: bool,
    pub note: String,
    pub registered_at: String,
}

impl Node {
    /// Full representation used only for on-disk persistence (includes token).
    pub fn to_json(&self) -> String {
        format!(
            "{{\"id\":{},\"endpoint\":{},\"token\":{},\"enabled\":{},\"note\":{},\"registered_at\":{}}}",
            q(&self.id),
            q(&self.endpoint),
            q(&self.token),
            self.enabled,
            q(&self.note),
            q(&self.registered_at),
        )
    }

    /// API-safe representation: the token field is intentionally absent.
    pub fn public_json(&self) -> String {
        format!(
            "{{\"id\":{},\"endpoint\":{},\"enabled\":{},\"note\":{},\"registered_at\":{}}}",
            q(&self.id),
            q(&self.endpoint),
            self.enabled,
            q(&self.note),
            q(&self.registered_at),
        )
    }

    pub fn from_json(line: &str) -> Option<Node> {
        Some(Node {
            id: json_str(line, "id")?,
            endpoint: json_str(line, "endpoint")?,
            token: json_str(line, "token").unwrap_or_default(),
            enabled: json_bool_opt(line, "enabled").unwrap_or(true),
            note: json_str(line, "note").unwrap_or_default(),
            registered_at: json_str(line, "registered_at").unwrap_or_default(),
        })
    }
}

pub struct NodeStore {
    pub nodes: Vec<Node>,
    pub path: String,
    pub max_nodes: usize,
}

impl NodeStore {
    pub fn open(path: &str, max_nodes: usize) -> Self {
        let max_nodes = max_nodes.max(1);
        let mut nodes = Vec::new();
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                if line.trim_start().starts_with('{') {
                    if let Some(node) = Node::from_json(line) {
                        nodes.push(node);
                    }
                }
            }
        }
        nodes.truncate(max_nodes);
        Self {
            nodes,
            path: path.to_string(),
            max_nodes,
        }
    }

    pub fn get(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn set_limit(&mut self, max_nodes: usize) {
        self.max_nodes = max_nodes.max(1);
        self.nodes.truncate(self.max_nodes);
    }

    /// Insert or replace by id; returns the total after a successful write.
    pub fn upsert(&mut self, node: Node) -> Result<usize, String> {
        if node.id.trim().is_empty() {
            return Err("node id must not be empty".to_string());
        }
        if !valid_endpoint(&node.endpoint) {
            return Err("endpoint must be an http:// loopback address".to_string());
        }
        match self.nodes.iter().position(|item| item.id == node.id) {
            Some(index) => self.nodes[index] = node,
            None => {
                if self.nodes.len() >= self.max_nodes {
                    return Err(format!("node limit ({}) reached", self.max_nodes));
                }
                self.nodes.push(node);
            }
        }
        self.persist().map_err(|error| error.to_string())?;
        Ok(self.nodes.len())
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.nodes.len();
        self.nodes.retain(|node| node.id != id);
        let removed = self.nodes.len() != before;
        if removed {
            let _ = self.persist();
        }
        removed
    }

    fn persist(&self) -> io::Result<()> {
        let lines: Vec<String> = self.nodes.iter().map(Node::to_json).collect();
        atomic_write(&self.path, &lines.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// SnapshotLite: the six canonical §3.6 fields. No id, no credentials.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotLite {
    pub schema: u64,
    pub component: String,
    pub created_at: String,
    pub reason: String,
    pub config_text: String,
    pub checksum: String,
}

impl SnapshotLite {
    /// Build a local snapshot from raw config text (used by /api/sync/push).
    pub fn from_config(component: &str, reason: &str, config_text: &str) -> Self {
        Self {
            schema: 1,
            component: component.to_string(),
            created_at: now_rfc3339(),
            reason: reason.to_string(),
            config_text: config_text.to_string(),
            checksum: format!("sha256:{}", sha256_hex(config_text.as_bytes())),
        }
    }

    /// Canonical §3.6 shape — also the body POSTed to `/api/config/snapshot`.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"schema\":{},\"component\":{},\"created_at\":{},\"reason\":{},\"config_text\":{},\"checksum\":{}}}",
            self.schema,
            q(&self.component),
            q(&self.created_at),
            q(&self.reason),
            q(&self.config_text),
            q(&self.checksum),
        )
    }

    /// Parse a bare snapshot OR a snapshot nested in the standard `{"data":...}`
    /// envelope returned by `GET /api/config/snapshot`. Only `config_text` and
    /// `checksum` are mandatory; the metadata defaults are harmless.
    pub fn from_json(body: &str) -> Option<Self> {
        let config_text = json_str(body, "config_text")?;
        let checksum = json_str(body, "checksum")?;
        Some(Self {
            schema: json_u64(body, "schema").unwrap_or(1),
            component: json_str(body, "component").unwrap_or_default(),
            created_at: json_str(body, "created_at").unwrap_or_default(),
            reason: json_str(body, "reason").unwrap_or_default(),
            config_text,
            checksum,
        })
    }

    /// `checksum` must equal `sha256:<hex>` of the raw `config_text`.
    pub fn verify_checksum(&self) -> bool {
        let expected = format!("sha256:{}", sha256_hex(self.config_text.as_bytes()));
        constant_eq(expected.as_bytes(), self.checksum.as_bytes())
    }
}

/// Pure conflict decision (docs/M5-PLAN.md §7.2). A bad checksum is always
/// rejected first; `reject` only blocks when a local snapshot is >= incoming.
pub fn decide(
    local_created_at: Option<&str>,
    incoming: &SnapshotLite,
    strategy: Strategy,
) -> (Decision, &'static str) {
    if !incoming.verify_checksum() {
        return (Decision::Rejected, "bad_checksum");
    }
    let newer_local = match local_created_at {
        Some(text) => text >= incoming.created_at.as_str(),
        None => false,
    };
    match strategy {
        Strategy::Reject if newer_local => (Decision::Rejected, "conflict"),
        Strategy::Manual => (Decision::Pending, "awaiting_confirmation"),
        Strategy::LastWriteWins => (Decision::Overwritten, "explicit_override"),
        _ => (Decision::Accepted, "no_conflict"),
    }
}

// ---------------------------------------------------------------------------
// Bounded audit log. Every decision is recorded; nothing here is a credential.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub time: String,
    /// pull | push | confirm
    pub direction: String,
    pub node: String,
    pub component: String,
    /// Only the first 16 characters of the checksum are retained.
    pub checksum: String,
    pub decision: String,
    pub reason: String,
}

impl AuditEntry {
    pub fn new(
        direction: &str,
        node: &str,
        component: &str,
        checksum: &str,
        decision: &str,
        reason: &str,
    ) -> Self {
        Self {
            time: now_rfc3339(),
            direction: direction.to_string(),
            node: node.to_string(),
            component: component.to_string(),
            checksum: checksum.chars().take(16).collect(),
            decision: decision.to_string(),
            reason: reason.to_string(),
        }
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"time\":{},\"direction\":{},\"node\":{},\"component\":{},\"checksum\":{},\"decision\":{},\"reason\":{}}}",
            q(&self.time),
            q(&self.direction),
            q(&self.node),
            q(&self.component),
            q(&self.checksum),
            q(&self.decision),
            q(&self.reason),
        )
    }

    pub fn from_json(line: &str) -> Option<AuditEntry> {
        Some(AuditEntry {
            time: json_str(line, "time").unwrap_or_default(),
            direction: json_str(line, "direction").unwrap_or_default(),
            node: json_str(line, "node").unwrap_or_default(),
            component: json_str(line, "component").unwrap_or_default(),
            checksum: json_str(line, "checksum").unwrap_or_default(),
            decision: json_str(line, "decision").unwrap_or_default(),
            reason: json_str(line, "reason").unwrap_or_default(),
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

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn items_json(&self) -> String {
        let items: Vec<String> = self.entries.iter().map(AuditEntry::to_json).collect();
        format!("[{}]", items.join(","))
    }

    fn persist(&self) -> io::Result<()> {
        let lines: Vec<String> = self.entries.iter().map(AuditEntry::to_json).collect();
        atomic_write(&self.path, &lines.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 client over loopback only. No TLS, no chunked decoding: the
// peer (the standard runtime) always answers with Content-Length + close.
// ---------------------------------------------------------------------------

fn split_authority(authority: &str) -> Result<(String, u16), String> {
    if let Some(rest) = authority.strip_prefix('[') {
        let close = rest.find(']').ok_or("malformed IPv6 authority")?;
        let host = rest[..close].to_string();
        let tail = &rest[close + 1..];
        let port = match tail.strip_prefix(':') {
            Some(value) => value.parse::<u16>().map_err(|_| "bad port")?,
            None => 80,
        };
        Ok((host, port))
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        Ok((
            host.to_string(),
            port.parse::<u16>().map_err(|_| "bad port")?,
        ))
    } else {
        Ok((authority.to_string(), 80))
    }
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback(),
        Err(_) => false,
    }
}

/// Validate a registered node endpoint: `http://` + a loopback host.
pub fn valid_endpoint(endpoint: &str) -> bool {
    let rest = match endpoint.strip_prefix("http://") {
        Some(value) => value,
        None => return false,
    };
    let cut = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..cut];
    if authority.is_empty() {
        return false;
    }
    match split_authority(authority) {
        Ok((host, _)) => is_loopback_host(&host),
        Err(_) => false,
    }
}

/// Request target (path + query, always with a leading `/`) used as the GET
/// signing payload.
fn request_target(url: &str) -> String {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let cut = rest.find(['/', '?']).unwrap_or(rest.len());
    let tail = &rest[cut..];
    if tail.is_empty() {
        "/".to_string()
    } else if tail.starts_with('?') {
        format!("/{}", tail)
    } else {
        tail.to_string()
    }
}

fn parse_http_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "only http:// endpoints are supported".to_string())?;
    let cut = rest.find(['/', '?']).unwrap_or(rest.len());
    let authority = &rest[..cut];
    if authority.is_empty() {
        return Err("missing host".to_string());
    }
    let (host, port) = split_authority(authority)?;
    if !is_loopback_host(&host) {
        return Err("only loopback endpoints are allowed".to_string());
    }
    let path = request_target(url);
    Ok((host, port, path))
}

fn parse_status(head: &str) -> Result<u16, String> {
    let line = head
        .lines()
        .next()
        .ok_or_else(|| "empty response".to_string())?;
    let code = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "no status code".to_string())?;
    code.parse::<u16>()
        .map_err(|_| "bad status code".to_string())
}

fn http_exchange(
    method: &str,
    url: &str,
    body: &str,
    timeout_ms: u64,
    headers: &[(&str, &str)],
) -> Result<(u16, String), String> {
    let (host, port, path) = parse_http_url(url)?;
    let connect_addr = if host.contains(':') {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    };
    let addrs: Vec<_> = connect_addr
        .to_socket_addrs()
        .map_err(|error| error.to_string())?
        .collect();
    if addrs.is_empty() {
        return Err("no resolvable address".to_string());
    }
    let timeout = Duration::from_millis(timeout_ms.max(1));
    let mut connected = None;
    for addr in &addrs {
        if let Ok(value) = TcpStream::connect_timeout(addr, timeout) {
            connected = Some(value);
            break;
        }
    }
    let mut stream = match connected {
        Some(value) => value,
        None => return Err("connect: all loopback addresses refused".to_string()),
    };
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;

    let host_header = if host.contains(':') {
        format!("[{}]:{}", host, port)
    } else {
        format!("{}:{}", host, port)
    };
    let mut request = String::new();
    request.push_str(&format!("{} {} HTTP/1.1\r\n", method, path));
    request.push_str(&format!("Host: {}\r\n", host_header));
    request.push_str("Connection: close\r\n");
    if method == "POST" {
        request.push_str("Content-Type: application/json\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    for (key, value) in headers {
        request.push_str(&format!("{}: {}\r\n", key, value));
    }
    request.push_str("\r\n");
    if method == "POST" {
        request.push_str(body);
    }
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("write: {}", error))?;
    stream
        .flush()
        .map_err(|error| format!("flush: {}", error))?;

    let mut raw = Vec::new();
    // Bound the response so a misbehaving peer cannot exhaust memory.
    stream
        .take(1_048_576)
        .read_to_end(&mut raw)
        .map_err(|error| format!("read: {}", error))?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let sep = text
        .find("\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response".to_string())?;
    let status = parse_status(&text[..sep])?;
    Ok((status, text[sep + 4..].to_string()))
}

/// Low-level GET (no signing), the exact shape exercised by the roundtrip test.
pub fn http_get(url: &str, timeout_ms: u64) -> Result<String, String> {
    let (_, text) = http_exchange("GET", url, "", timeout_ms, &[])?;
    Ok(text)
}

/// Low-level POST (no signing), the exact shape exercised by the roundtrip test.
pub fn http_post(url: &str, body: &str, timeout_ms: u64) -> Result<(u16, String), String> {
    http_exchange("POST", url, body, timeout_ms, &[])
}

/// Optional request headers. `shared_secret` empty => skip the signature
/// entirely; an empty token is simply not forwarded. The token lives only in
/// this transient header, never in logs/metrics/audit.
fn sync_headers(token: &str, secret: &str, payload: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !token.is_empty() {
        out.push(("X-Admin-Token".to_string(), token.to_string()));
    }
    if !secret.is_empty() {
        out.push(("X-Lan-Sync-Sig".to_string(), sign_payload(secret, payload)));
    }
    out
}

/// GET with the standard snapshot path as the HMAC payload and optional node
/// credentials. Used by /api/sync/pull.
pub(crate) fn http_get_signed(
    url: &str,
    timeout_ms: u64,
    token: &str,
    secret: &str,
) -> Result<String, String> {
    let payload = request_target(url);
    let headers = sync_headers(token, secret, &payload);
    if headers.is_empty() {
        return http_get(url, timeout_ms);
    }
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let (_, text) = http_exchange("GET", url, "", timeout_ms, &refs)?;
    Ok(text)
}

/// POST with the request body as the HMAC payload. Used by /api/sync/push.
pub(crate) fn http_post_signed(
    url: &str,
    body: &str,
    timeout_ms: u64,
    token: &str,
    secret: &str,
) -> Result<(u16, String), String> {
    let headers = sync_headers(token, secret, body);
    if headers.is_empty() {
        return http_post(url, body, timeout_ms);
    }
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    http_exchange("POST", url, body, timeout_ms, &refs)
}

// ---------------------------------------------------------------------------
// HMAC-SHA256 node request signing (shared_secret).
// ---------------------------------------------------------------------------

/// `sha256:<hex>` of HMAC-SHA256(secret, payload). Pure function.
pub fn sign_payload(secret: &str, payload: &str) -> String {
    format!(
        "sha256:{}",
        to_hex(&hmac_sha256(secret.as_bytes(), payload.as_bytes()))
    )
}

/// Constant-time signature verification. Pure function kept for the receiving
/// side / unit tests; a tampered payload must fail.
#[allow(dead_code)]
pub fn verify_signature(secret: &str, payload: &str, sig: &str) -> bool {
    constant_eq(sign_payload(secret, payload).as_bytes(), sig.as_bytes())
}

// ---------------------------------------------------------------------------
// Unit tests (all file work uses a unique temp_dir; no real network calls).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
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
        let path = std::env::temp_dir().join(format!("lan-sync-{}-{}", tag, unique));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn sample_snapshot() -> SnapshotLite {
        SnapshotLite::from_config("lan-sync", "sync", "version: 1\ncomponent: lan-sync\n")
    }

    // 1 — checksum verification + tampering.
    #[test]
    fn checksum_verify_and_tamper() {
        let snap = sample_snapshot();
        assert!(snap.verify_checksum());
        assert_eq!(
            snap.checksum,
            format!("sha256:{}", sha256_hex(snap.config_text.as_bytes()))
        );
        let mut tampered = snap.clone();
        tampered.checksum =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string();
        assert!(!tampered.verify_checksum());
        let (decision, reason) = decide(None, &tampered, Strategy::Reject);
        assert_eq!(decision, Decision::Rejected);
        assert_eq!(reason, "bad_checksum");
    }

    // 2 — reject strategy.
    #[test]
    fn decide_reject_blocks_newer_local() {
        let snap = sample_snapshot();
        let (decision, reason) = decide(Some(&snap.created_at), &snap, Strategy::Reject);
        assert_eq!(decision, Decision::Rejected);
        assert_eq!(reason, "conflict");
    }

    // 3 — last_write_wins strategy.
    #[test]
    fn decide_last_write_wins_overwrites() {
        let snap = sample_snapshot();
        let (decision, reason) =
            decide(Some("2099-01-01T00:00:00Z"), &snap, Strategy::LastWriteWins);
        assert_eq!(decision, Decision::Overwritten);
        assert_eq!(reason, "explicit_override");
    }

    // 4 — manual strategy.
    #[test]
    fn decide_manual_goes_pending() {
        let snap = sample_snapshot();
        let (decision, reason) = decide(None, &snap, Strategy::Manual);
        assert_eq!(decision, Decision::Pending);
        assert_eq!(reason, "awaiting_confirmation");
    }

    // 5 — no local snapshot => accepted even under reject.
    #[test]
    fn decide_accepts_when_no_local_snapshot() {
        let snap = sample_snapshot();
        let (decision, reason) = decide(None, &snap, Strategy::Reject);
        assert_eq!(decision, Decision::Accepted);
        assert_eq!(reason, "no_conflict");
        assert_eq!(decision.as_str(), "accepted");
    }

    // 6 — Node roundtrip + token never exposed.
    #[test]
    fn node_json_roundtrip_never_exposes_token() {
        let node = Node {
            id: "n1".to_string(),
            endpoint: "http://127.0.0.1:18086".to_string(),
            token: "super-secret-token".to_string(),
            enabled: true,
            note: "rack-a".to_string(),
            registered_at: "2026-11-14T00:00:00Z".to_string(),
        };
        let line = node.to_json();
        assert!(line.contains("super-secret-token")); // disk only
        assert_eq!(Node::from_json(&line), Some(node.clone()));
        let public = node.public_json();
        assert!(!public.contains("token"));
        assert!(!public.contains("super-secret-token"));
        assert!(public.contains("\"enabled\":true"));
        assert!(public.contains("\"registered_at\""));
    }

    // 7 — bounded, credential-free audit log.
    #[test]
    fn audit_log_is_bounded_and_credential_free() {
        let dir = temp_dir("audit");
        let path = dir.join("history.json").to_string_lossy().into_owned();
        let mut log = AuditLog::open(&path, 3);
        for index in 0..10 {
            log.push(AuditEntry::new(
                "pull",
                &format!("n{}", index),
                "lan-sync",
                "sha256:abcdef0123456789",
                "accepted",
                "no_conflict",
            ));
            assert!(log.len() <= 3, "iteration {index}");
        }
        assert_eq!(log.len(), 3);
        assert_eq!(log.entries.back().unwrap().checksum.len(), 16);
        let disk = fs::read_to_string(&path).unwrap();
        assert_eq!(disk.lines().count(), 3);
        assert!(!disk.contains("token"));
        assert!(!disk.contains("password"));
        assert!(!disk.contains("shared_secret"));
        let _ = fs::remove_dir_all(&dir);
    }

    // 8 — HMAC signature verification rejects tampering.
    #[test]
    fn hmac_signature_rejects_tampered_payload() {
        let secret = "s3cr3t";
        let payload = "/api/config/snapshot?reason=sync";
        let sig = sign_payload(secret, payload);
        assert!(sig.starts_with("sha256:"));
        assert_eq!(sig.len(), 7 + 64);
        assert!(verify_signature(secret, payload, &sig));
        assert!(!verify_signature(secret, "tampered", &sig));
        assert!(!verify_signature("wrong-secret", payload, &sig));
        assert!(!verify_signature(secret, payload, "sha256:deadbeef"));
    }

    // 9 — GET / POST roundtrip against a local TcpListener.
    fn serve_one(stream: &mut TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut first = String::new();
        if reader.read_line(&mut first).unwrap_or(0) == 0 {
            return;
        }
        let method = first.split_whitespace().next().unwrap_or("").to_string();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.trim().eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            let _ = reader.read_exact(&mut body);
        }
        let payload = if method == "POST" {
            format!("POST-OK:{}", String::from_utf8_lossy(&body))
        } else {
            "GET-OK".to_string()
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            payload.len(),
            payload
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }

    #[test]
    fn http_get_and_post_roundtrip_over_loopback() {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(value) => value,
            Err(_) => return, // sandbox without loopback sockets
        };
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(_) => return,
                };
                serve_one(&mut stream);
            }
        });
        let url = format!("http://127.0.0.1:{}/api/config/snapshot?reason=sync", port);
        let body = http_get(&url, 2000).expect("http_get must succeed");
        assert_eq!(body, "GET-OK");
        let (status, posted) =
            http_post(&url, "{\"hello\":1}", 2000).expect("http_post must succeed");
        assert_eq!(status, 200);
        assert!(posted.starts_with("POST-OK:"));
        assert!(posted.contains("{\"hello\":1}"));
        let _ = server.join();
    }

    // 10 — SnapshotLite parses both bare and standard-envelope bodies.
    #[test]
    fn snapshot_lite_parses_bare_and_wrapped_envelopes() {
        let snap = sample_snapshot();
        let bare = snap.to_json();
        assert_eq!(SnapshotLite::from_json(&bare), Some(snap.clone()));
        let wrapped = format!("{{\"ok\":true,\"data\":{},\"request_id\":\"abc\"}}", bare);
        let parsed = SnapshotLite::from_json(&wrapped).expect("wrapped envelope must parse");
        assert_eq!(parsed.config_text, snap.config_text);
        assert_eq!(parsed.checksum, snap.checksum);
        assert_eq!(parsed.schema, 1);
        assert!(parsed.verify_checksum());
    }

    // 11 — endpoints must be http:// loopback.
    #[test]
    fn endpoints_must_be_http_loopback() {
        assert!(valid_endpoint("http://127.0.0.1:18086"));
        assert!(valid_endpoint("http://localhost:8086"));
        assert!(valid_endpoint("http://[::1]:8086"));
        assert!(!valid_endpoint("https://127.0.0.1:18086"));
        assert!(!valid_endpoint("http://10.0.0.5:80"));
        assert!(!valid_endpoint("127.0.0.1:18086"));
    }

    // 12 — NodeStore persists and enforces the capacity bound.
    #[test]
    fn node_store_persists_and_enforces_limit() {
        let dir = temp_dir("nodes");
        let path = dir.join("nodes.json").to_string_lossy().into_owned();
        let make = |id: &str| Node {
            id: id.to_string(),
            endpoint: "http://127.0.0.1:18086".to_string(),
            token: "t".to_string(),
            enabled: true,
            note: String::new(),
            registered_at: now_rfc3339(),
        };
        let mut store = NodeStore::open(&path, 2);
        assert_eq!(store.upsert(make("a")).unwrap(), 1);
        assert_eq!(store.upsert(make("b")).unwrap(), 2);
        assert!(store.upsert(make("c")).is_err(), "limit must be enforced");
        let reopened = NodeStore::open(&path, 2);
        assert_eq!(reopened.len(), 2);
        assert!(reopened.get("a").is_some());
        assert!(reopened.get("missing").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    // 13 — json helpers read booleans / numbers.
    #[test]
    fn json_helpers_read_bools_and_numbers() {
        assert_eq!(json_bool_opt("{\"enabled\":true}", "enabled"), Some(true));
        assert_eq!(
            json_bool_opt("{ \"accept\" : false }", "accept"),
            Some(false)
        );
        assert_eq!(json_bool_opt("{}", "enabled"), None);
        assert_eq!(json_u64("{\"schema\":1}", "schema"), Some(1));
        assert_eq!(json_u64("{\"schema\":\"1\"}", "schema"), Some(1));
    }

    // 14 — strategy parsing is closed over the three allowed values.
    #[test]
    fn strategy_parse_is_closed() {
        assert_eq!(Strategy::parse("reject"), Some(Strategy::Reject));
        assert_eq!(
            Strategy::parse("last_write_wins"),
            Some(Strategy::LastWriteWins)
        );
        assert_eq!(Strategy::parse("manual"), Some(Strategy::Manual));
        assert_eq!(Strategy::parse("overwrite_always"), None);
        assert_eq!(Strategy::Manual.as_str(), "manual");
    }
}
