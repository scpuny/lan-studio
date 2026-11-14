//! Static name mappings, hosts-text rendering and atomic publish (PRD 4.6 / M4).
//!
//! This module deliberately contains **no** mDNS responder, DNS packet code,
//! HTTP proxy or cross-component push. It is the single source of truth for
//! `name <-> ip` pairs and only renders / writes a standard hosts file.

use std::fs;
use std::net::IpAddr;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use common_api::json_quote;

// ---------------------------------------------------------------------------
// Configuration (the `hosts.*` namespace)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct HostsConfig {
    pub allow_local_suffix: bool,
    /// `reject` | `upsert`.
    pub conflict_policy: String,
    pub mappings_path: String,
    pub output_path: String,
    pub max_mappings: usize,
    pub publish_enabled: bool,
    pub include_header: bool,
}

impl HostsConfig {
    /// Offline validation of every `hosts.*` knob. Paths must never escape the
    /// working directory, and the mapping ceiling must stay bounded.
    pub fn validate(&self) -> Result<(), String> {
        if self.conflict_policy != "reject" && self.conflict_policy != "upsert" {
            return Err("hosts.conflict_policy must be 'reject' or 'upsert'".to_string());
        }
        if self.max_mappings == 0 || self.max_mappings > 100_000 {
            return Err("hosts.max_mappings must be 1..100000".to_string());
        }
        if self.mappings_path.is_empty() || self.mappings_path.contains("..") {
            return Err(
                "hosts.mappings_path must be non-empty and must not contain '..'".to_string(),
            );
        }
        if self.output_path.is_empty() || self.output_path.contains("..") {
            return Err(
                "hosts.output_path must be non-empty and must not contain '..'".to_string(),
            );
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Mapping {
    pub name: String,
    pub ip: String,
    pub aliases: Vec<String>,
    pub comment: String,
    pub tags: Vec<String>,
    pub enabled: bool,
    pub updated_at: String,
}

impl Mapping {
    pub fn new(name: String, ip: String) -> Self {
        Self {
            name,
            ip,
            aliases: Vec::new(),
            comment: String::new(),
            tags: Vec::new(),
            enabled: true,
            updated_at: now_rfc3339(),
        }
    }

    /// One JSON object per line; field order is part of the on-disk contract.
    pub fn to_json(&self) -> String {
        format!(
            "{{\"name\":{},\"ip\":{},\"aliases\":{},\"comment\":{},\"tags\":{},\"enabled\":{},\"updated_at\":{}}}",
            json_quote(&self.name),
            json_quote(&self.ip),
            json_str_array(&self.aliases),
            json_quote(&self.comment),
            json_str_array(&self.tags),
            self.enabled,
            json_quote(&self.updated_at),
        )
    }

    fn from_json(line: &str) -> Option<Self> {
        Some(Self {
            name: parse_string(line, "name")?,
            ip: parse_string(line, "ip")?,
            aliases: parse_str_array(line, "aliases").unwrap_or_default(),
            comment: parse_string(line, "comment").unwrap_or_default(),
            tags: parse_str_array(line, "tags").unwrap_or_default(),
            enabled: parse_bool(line, "enabled").unwrap_or(true),
            updated_at: parse_string(line, "updated_at").unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectKind {
    Invalid,
    Conflict,
    Limit,
    Io,
}

#[derive(Debug)]
pub struct UpsertError {
    pub kind: RejectKind,
    pub message: String,
}

impl UpsertError {
    fn new(kind: RejectKind, message: String) -> Self {
        Self { kind, message }
    }
}

#[derive(Debug, Default)]
pub struct ImportResult {
    pub imported: u64,
    pub skipped: u64,
    pub conflicts: u64,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub struct MappingStore {
    pub path: String,
    pub max_mappings: usize,
    pub items: Vec<Mapping>,
}

impl MappingStore {
    /// A missing file is an empty store; malformed lines are skipped so one bad
    /// row can never make the component unbootable.
    pub fn open(path: &str, max_mappings: usize) -> Self {
        let mut items = Vec::new();
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(mapping) = Mapping::from_json(line) {
                    items.push(mapping);
                }
            }
        }
        Self {
            path: path.to_string(),
            max_mappings,
            items,
        }
    }

    /// Validate, resolve conflicts and atomically persist. The in-memory view
    /// is only replaced after the file write succeeds.
    pub fn upsert(&mut self, cfg: &HostsConfig, mapping: Mapping) -> Result<usize, UpsertError> {
        let mut next = self.items.clone();
        upsert_in(&mut next, cfg, self.max_mappings, mapping)?;
        persist_items(&self.path, &next)
            .map_err(|message| UpsertError::new(RejectKind::Io, message))?;
        let total = next.len();
        self.items = next;
        Ok(total)
    }

    pub fn remove(&mut self, name: &str) -> Result<bool, String> {
        let mut next = self.items.clone();
        let before = next.len();
        next.retain(|item| item.name != name);
        if next.len() == before {
            return Ok(false);
        }
        persist_items(&self.path, &next)?;
        self.items = next;
        Ok(true)
    }

    /// Parse a standard hosts text and UPSERT each row. Empty lines, `#`
    /// comments and inline `#` comments are ignored; the first column is the
    /// IP, the first name becomes `name`, the rest become `aliases`.
    pub fn import(&mut self, cfg: &HostsConfig, text: &str) -> Result<ImportResult, String> {
        let mut result = ImportResult::default();
        let mut next = self.items.clone();
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let columns: Vec<&str> = line
                .split_whitespace()
                .take_while(|column| !column.starts_with('#'))
                .collect();
            if columns.len() < 2 || IpAddr::from_str(columns[0]).is_err() {
                result.skipped += 1;
                continue;
            }
            let mut mapping = Mapping::new(columns[1].to_string(), columns[0].to_string());
            mapping.aliases = columns[2..].iter().map(|value| value.to_string()).collect();
            match upsert_in(&mut next, cfg, self.max_mappings, mapping) {
                Ok(()) => result.imported += 1,
                Err(error) if error.kind == RejectKind::Conflict => result.conflicts += 1,
                Err(_) => result.skipped += 1,
            }
        }
        if result.imported > 0 {
            persist_items(&self.path, &next)?;
            self.items = next;
        }
        Ok(result)
    }
}

fn upsert_in(
    items: &mut Vec<Mapping>,
    cfg: &HostsConfig,
    max_mappings: usize,
    mut mapping: Mapping,
) -> Result<(), UpsertError> {
    validate_mapping(cfg, &mapping)?;
    mapping.updated_at = now_rfc3339();
    if let Some(index) = items.iter().position(|item| item.name == mapping.name) {
        if items[index].ip != mapping.ip && cfg.conflict_policy == "reject" {
            return Err(UpsertError::new(
                RejectKind::Conflict,
                format!(
                    "conflict: '{}' already maps to {}",
                    mapping.name, items[index].ip
                ),
            ));
        }
        items[index] = mapping;
    } else {
        if items.len() >= max_mappings {
            return Err(UpsertError::new(
                RejectKind::Limit,
                format!("max_mappings ({}) reached", max_mappings),
            ));
        }
        items.push(mapping);
    }
    Ok(())
}

fn validate_mapping(cfg: &HostsConfig, mapping: &Mapping) -> Result<(), UpsertError> {
    if !valid_name(&mapping.name) {
        return Err(UpsertError::new(
            RejectKind::Invalid,
            format!("invalid name: {}", mapping.name),
        ));
    }
    if IpAddr::from_str(&mapping.ip).is_err() {
        return Err(UpsertError::new(
            RejectKind::Invalid,
            format!("invalid ip: {}", mapping.ip),
        ));
    }
    for alias in &mapping.aliases {
        if !valid_name(alias) {
            return Err(UpsertError::new(
                RejectKind::Invalid,
                format!("invalid alias: {}", alias),
            ));
        }
    }
    if !cfg.allow_local_suffix {
        for name in std::iter::once(&mapping.name).chain(mapping.aliases.iter()) {
            if name.to_ascii_lowercase().ends_with(".local") {
                return Err(UpsertError::new(
                    RejectKind::Conflict,
                    format!("'.local' is reserved for lan-mdns: {}", name),
                ));
            }
        }
    }
    Ok(())
}

/// Lowercase letters / digits / hyphen / dot, <=253 bytes, no leading or
/// trailing dot. Applied to both names and aliases so a rendered row can never
/// contain whitespace or control characters.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && !name.starts_with('.')
        && !name.ends_with('.')
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '.')
}

// ---------------------------------------------------------------------------
// Rendering / publishing
// ---------------------------------------------------------------------------

/// Expand `name` + `aliases`, filter `enabled=false`, sort by `(name, ip)`,
/// dedup and emit stable `ip<TAB>name` rows.
pub fn render(store: &MappingStore, include_header: bool) -> String {
    let mut rows: Vec<(&str, &str)> = Vec::new();
    for mapping in &store.items {
        if !mapping.enabled {
            continue;
        }
        rows.push((mapping.name.as_str(), mapping.ip.as_str()));
        for alias in &mapping.aliases {
            rows.push((alias.as_str(), mapping.ip.as_str()));
        }
    }
    rows.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(b.1)));
    rows.dedup();
    let mut out = String::new();
    if include_header {
        out.push_str("# generated by lan-hosts — do not edit\n");
    }
    for (name, ip) in rows {
        out.push_str(ip);
        out.push('\t');
        out.push_str(name);
        out.push('\n');
    }
    out
}

/// `publish_enabled=false` is a hard error (never a silent success). The write
/// goes through `<path>.tmp` + `rename`; any failure leaves the old file intact.
pub fn publish(cfg: &HostsConfig, text: &str) -> Result<(), String> {
    if !cfg.publish_enabled {
        return Err("publish is disabled (hosts.publish_enabled=false)".to_string());
    }
    atomic_write(&cfg.output_path, text)
}

fn persist_items(path: &str, items: &[Mapping]) -> Result<(), String> {
    let mut text = String::new();
    for item in items {
        text.push_str(&item.to_json());
        text.push('\n');
    }
    atomic_write(path, &text)
}

fn atomic_write(path: &str, text: &str) -> Result<(), String> {
    if path.is_empty() || path.contains("..") {
        return Err("path must be non-empty and must not contain '..'".to_string());
    }
    let tmp = format!("{}.tmp", path);
    fs::write(&tmp, text).map_err(|error| format!("write {}: {}", tmp, error))?;
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("rename to {}: {}", path, error));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Hand-rolled JSON helpers (no serde)
// ---------------------------------------------------------------------------

fn json_str_array(values: &[String]) -> String {
    let parts: Vec<String> = values.iter().map(|value| json_quote(value)).collect();
    format!("[{}]", parts.join(","))
}

/// Return the raw value token that follows `"key":` in a flat JSON object.
/// The needle must be a real key (preceded by `{` or `,`), not a substring of
/// another value, so an arbitrary `comment` cannot shadow a field.
fn value_after<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{}\"", key);
    let bytes = text.as_bytes();
    let mut from = 0usize;
    while let Some(relative) = text[from..].find(&needle) {
        let at = from + relative;
        let end = at + needle.len();
        let mut before = at;
        while before > 0 && bytes[before - 1].is_ascii_whitespace() {
            before -= 1;
        }
        let is_key = before == 0 || bytes[before - 1] == b'{' || bytes[before - 1] == b',';
        let mut after = end;
        while after < bytes.len() && bytes[after].is_ascii_whitespace() {
            after += 1;
        }
        if is_key && after < bytes.len() && bytes[after] == b':' {
            return Some(text[after + 1..].trim_start());
        }
        from = end;
    }
    None
}

/// Decode a JSON string token (handles escapes) by leaning on the shared
/// `common_component::json_str` parser.
fn parse_string_token(token: &str) -> Option<String> {
    common_component::json_str(&format!("{{\"x\":{}}}", token), "x")
}

fn parse_string(line: &str, key: &str) -> Option<String> {
    parse_string_token(value_after(line, key)?)
}

pub fn parse_bool(line: &str, key: &str) -> Option<bool> {
    let value = value_after(line, key)?;
    if value.starts_with("true") {
        Some(true)
    } else if value.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Parse a flat JSON string array without relying on `find(']')`, so values
/// containing `,` or `]` still round-trip.
pub fn parse_str_array(line: &str, key: &str) -> Option<Vec<String>> {
    let value = value_after(line, key)?;
    let bytes = value.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.get(i) != Some(&b'[') {
        return None;
    }
    i += 1;
    let mut out = Vec::new();
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        if bytes[i] == b']' {
            return Some(out);
        }
        if bytes[i] != b'"' {
            return None;
        }
        let start = i;
        i += 1;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'"' => {
                    i += 1;
                    break;
                }
                _ => i += 1,
            }
        }
        if i > bytes.len() {
            return None;
        }
        out.push(parse_string_token(&value[start..i])?);
    }
}

// ---------------------------------------------------------------------------
// Time (UTC RFC 3339, civil-date algorithm, no external crate)
// ---------------------------------------------------------------------------

pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    rfc3339(secs)
}

fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

// ---------------------------------------------------------------------------
// Tests — every file lives under a unique `temp_dir()` subdir and is removed.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "lan-hosts-{}-{}-{}",
                tag,
                std::process::id(),
                nanos
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self, name: &str) -> String {
            self.0.join(name).to_str().unwrap().to_string()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn mapping(name: &str, ip: &str) -> Mapping {
        Mapping {
            name: name.to_string(),
            ip: ip.to_string(),
            aliases: Vec::new(),
            comment: String::new(),
            tags: Vec::new(),
            enabled: true,
            updated_at: String::new(),
        }
    }

    fn cfg(dir: &TempDir) -> HostsConfig {
        HostsConfig {
            allow_local_suffix: false,
            conflict_policy: "reject".to_string(),
            mappings_path: dir.file("mappings.json"),
            output_path: dir.file("hosts.out"),
            max_mappings: 100,
            publish_enabled: true,
            include_header: true,
        }
    }

    #[test]
    fn name_and_ip_validation_rules() {
        assert!(valid_name("nas.lan"));
        assert!(valid_name("a-b.c0"));
        assert!(!valid_name(""));
        assert!(!valid_name(".nas"));
        assert!(!valid_name("nas."));
        assert!(!valid_name("NAS.lan"));
        assert!(!valid_name("nas_lan"));
        assert!(!valid_name(&"a".repeat(254)));
        assert!(IpAddr::from_str("192.168.5.10").is_ok());
        assert!(IpAddr::from_str("::1").is_ok());
        assert!(IpAddr::from_str("999.1.1.1").is_err());
    }

    #[test]
    fn local_suffix_is_rejected_in_name_and_aliases() {
        let dir = TempDir::new("local");
        let config = cfg(&dir);
        let mut store = MappingStore::open(&config.mappings_path, config.max_mappings);
        let bad = Mapping::new("printer.local".to_string(), "192.168.5.20".to_string());
        assert_eq!(
            store.upsert(&config, bad).unwrap_err().kind,
            RejectKind::Conflict
        );

        let mut aliased = Mapping::new("ok.lan".to_string(), "192.168.5.21".to_string());
        aliased.aliases = vec!["alias.local".to_string()];
        assert_eq!(
            store.upsert(&config, aliased).unwrap_err().kind,
            RejectKind::Conflict
        );
        assert_eq!(store.items.len(), 0);

        let mut allowed = config.clone();
        allowed.allow_local_suffix = true;
        assert!(store
            .upsert(
                &allowed,
                Mapping::new("printer.local".to_string(), "192.168.5.20".to_string())
            )
            .is_ok());
    }

    #[test]
    fn conflict_policy_reject_and_upsert() {
        let dir = TempDir::new("conflict");
        let config = cfg(&dir);
        let mut store = MappingStore::open(&config.mappings_path, config.max_mappings);
        store
            .upsert(
                &config,
                Mapping::new("nas.lan".to_string(), "192.168.5.10".to_string()),
            )
            .unwrap();
        let err = store
            .upsert(
                &config,
                Mapping::new("nas.lan".to_string(), "192.168.5.99".to_string()),
            )
            .unwrap_err();
        assert_eq!(err.kind, RejectKind::Conflict);
        assert_eq!(store.items.len(), 1);
        assert_eq!(store.items[0].ip, "192.168.5.10");

        // same IP is a harmless overwrite even under `reject`
        assert!(store
            .upsert(
                &config,
                Mapping::new("nas.lan".to_string(), "192.168.5.10".to_string())
            )
            .is_ok());

        let mut overwriting = config.clone();
        overwriting.conflict_policy = "upsert".to_string();
        assert!(store
            .upsert(
                &overwriting,
                Mapping::new("nas.lan".to_string(), "192.168.5.99".to_string())
            )
            .is_ok());
        assert_eq!(store.items[0].ip, "192.168.5.99");
    }

    #[test]
    fn render_is_sorted_deduplicated_and_stable() {
        let dir = TempDir::new("render");
        let config = cfg(&dir);
        let mut store = MappingStore::open(&config.mappings_path, config.max_mappings);
        let mut first = mapping("z.lan", "10.0.0.2");
        first.aliases = vec!["a.lan".to_string(), "a.lan".to_string()];
        let mut off = mapping("off.lan", "10.0.0.9");
        off.enabled = false;
        store.items = vec![first, mapping("a.lan", "10.0.0.1"), off];

        let text = render(&store, true);
        assert_eq!(text, render(&store, true));
        assert!(text.starts_with("# generated by lan-hosts — do not edit\n"));
        assert!(text.contains("10.0.0.1\ta.lan\n"));
        assert!(!text.contains("off.lan"));
        assert_eq!(text.matches("10.0.0.1\ta.lan").count(), 1);
        assert!(text.find("a.lan").unwrap() < text.find("z.lan").unwrap());
        assert!(!render(&store, false).starts_with('#'));
    }

    #[test]
    fn import_parses_standard_hosts_text() {
        let dir = TempDir::new("import");
        let config = cfg(&dir);
        let mut store = MappingStore::open(&config.mappings_path, config.max_mappings);
        let text = "# comment\n\n192.168.5.30 printer.lan alias.lan\n\
                    192.168.5.40 cam.lan\nnot-an-ip bad.lan\n";
        let result = store.import(&config, text).unwrap();
        assert_eq!(result.imported, 2);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.conflicts, 0);
        assert_eq!(store.items.len(), 2);
        let printer = store
            .items
            .iter()
            .find(|m| m.name == "printer.lan")
            .unwrap();
        assert_eq!(printer.aliases, vec!["alias.lan".to_string()]);
    }

    #[test]
    fn import_counts_conflicts_and_inline_comments() {
        let dir = TempDir::new("importconf");
        let config = cfg(&dir);
        let mut store = MappingStore::open(&config.mappings_path, config.max_mappings);
        store
            .upsert(
                &config,
                Mapping::new("nas.lan".to_string(), "10.0.0.1".to_string()),
            )
            .unwrap();
        let result = store
            .import(&config, "10.0.0.2 nas.lan\n10.0.0.3 new.lan # trailing\n")
            .unwrap();
        assert_eq!(result.conflicts, 1);
        assert_eq!(result.imported, 1);
        assert_eq!(store.items.len(), 2);
    }

    #[test]
    fn atomic_publish_failure_keeps_old_file_intact() {
        let dir = TempDir::new("publish");
        let config = cfg(&dir);
        publish(&config, "old\n").unwrap();
        let old = fs::read_to_string(&config.output_path).unwrap();

        let mut bad_dir = config.clone();
        bad_dir.output_path = dir.file("no/such/dir/hosts.out");
        assert!(publish(&bad_dir, "new\n").is_err());
        assert_eq!(fs::read_to_string(&config.output_path).unwrap(), old);

        let mut disabled = config.clone();
        disabled.publish_enabled = false;
        assert!(publish(&disabled, "new\n").is_err());

        let mut traversal = config.clone();
        traversal.output_path = dir.file("../evil");
        assert!(publish(&traversal, "new\n").is_err());
        assert_eq!(fs::read_to_string(&config.output_path).unwrap(), old);
    }

    #[test]
    fn json_lines_roundtrip_with_special_characters() {
        let mut record = mapping("nas.lan", "192.168.5.10");
        record.aliases = vec!["storage.lan".to_string()];
        record.comment = "NAS \"primary\" # rack\nsecond".to_string();
        record.tags = vec![
            "storage".to_string(),
            "a,b".to_string(),
            "bracket]x".to_string(),
        ];
        record.enabled = false;
        record.updated_at = "2026-11-14T01:00:00Z".to_string();

        let parsed = Mapping::from_json(&record.to_json()).unwrap();
        assert_eq!(parsed.name, record.name);
        assert_eq!(parsed.ip, record.ip);
        assert_eq!(parsed.aliases, record.aliases);
        assert_eq!(parsed.comment, record.comment);
        assert_eq!(parsed.tags, record.tags);
        assert_eq!(parsed.enabled, record.enabled);
        assert_eq!(parsed.updated_at, record.updated_at);

        let dir = TempDir::new("roundtrip");
        let path = dir.file("mappings.json");
        persist_items(&path, &[record.clone(), mapping("cam.lan", "192.168.5.40")]).unwrap();
        let reopened = MappingStore::open(&path, 10);
        assert_eq!(reopened.items.len(), 2);
        assert_eq!(reopened.items[0].comment, record.comment);
        assert_eq!(reopened.items[0].tags, record.tags);
    }

    #[test]
    fn max_mappings_is_enforced() {
        let dir = TempDir::new("max");
        let mut config = cfg(&dir);
        config.max_mappings = 2;
        let mut store = MappingStore::open(&config.mappings_path, config.max_mappings);
        store
            .upsert(
                &config,
                Mapping::new("a.lan".to_string(), "10.0.0.1".to_string()),
            )
            .unwrap();
        store
            .upsert(
                &config,
                Mapping::new("b.lan".to_string(), "10.0.0.2".to_string()),
            )
            .unwrap();
        assert_eq!(
            store
                .upsert(
                    &config,
                    Mapping::new("c.lan".to_string(), "10.0.0.3".to_string())
                )
                .unwrap_err()
                .kind,
            RejectKind::Limit
        );
        assert_eq!(store.items.len(), 2);
    }

    #[test]
    fn config_validation_rejects_bad_policy_and_paths() {
        let dir = TempDir::new("validate");
        let mut config = cfg(&dir);
        assert!(config.validate().is_ok());

        config.conflict_policy = "merge".to_string();
        assert!(config.validate().is_err());
        config.conflict_policy = "reject".to_string();

        config.output_path = "../hosts.out".to_string();
        assert!(config.validate().is_err());
        config.output_path = "no/such/dir/hosts.out".to_string();
        assert!(config.validate().is_ok());

        config.mappings_path = String::new();
        assert!(config.validate().is_err());
        config.mappings_path = dir.file("mappings.json");

        config.max_mappings = 0;
        assert!(config.validate().is_err());
    }
}
