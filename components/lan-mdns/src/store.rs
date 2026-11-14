//! Configuration lifecycle: bounded snapshots, one-click rollback, import /
//! export and atomic save (F-E03, F-E05..F-E08).
//!
//! Snapshot shape is frozen in `contracts/snapshot.json`: six fields, a
//! `sha256:` checksum over `config_text`, and no credentials. Snapshot IDs are
//! whitelisted before touching the filesystem, so path traversal is impossible.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use common_api::json_quote;
use common_config::validate_text;
use common_crypto::{random_hex, sha256_hex};

use crate::config::MdnsConfig;

#[derive(Debug, Clone)]
pub struct StoreError {
    pub code: &'static str,
    pub message: String,
}

impl StoreError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

fn io_error(err: io::Error) -> StoreError {
    StoreError::new("internal_error", err.to_string())
}

/// Self-contained config version record (contracts/snapshot.json).
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub schema: i64,
    pub component: String,
    pub created_at: String,
    pub reason: String,
    pub config_text: String,
    pub checksum: String,
}

impl Snapshot {
    pub fn new(component: &str, reason: &str, config_text: &str) -> Self {
        Self {
            schema: 1,
            component: component.to_string(),
            created_at: utc_now_rfc3339(),
            reason: reason.to_string(),
            config_text: config_text.to_string(),
            checksum: format!("sha256:{}", sha256_hex(config_text.as_bytes())),
        }
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"schema\":{},\"component\":{},\"created_at\":{},\"reason\":{},\"config_text\":{},\"checksum\":{}}}",
            self.schema,
            json_quote(&self.component),
            json_quote(&self.created_at),
            json_quote(&self.reason),
            json_quote(&self.config_text),
            json_quote(&self.checksum),
        )
    }

    pub fn from_json(text: &str) -> Result<Self, StoreError> {
        let bad = |message: &str| StoreError::new("invalid_config", message);
        let schema = json_i64(text, "schema").ok_or_else(|| bad("snapshot.schema missing"))?;
        let component =
            json_str(text, "component").ok_or_else(|| bad("snapshot.component missing"))?;
        let created_at =
            json_str(text, "created_at").ok_or_else(|| bad("snapshot.created_at missing"))?;
        let reason = json_str(text, "reason").unwrap_or_default();
        let config_text =
            json_str(text, "config_text").ok_or_else(|| bad("snapshot.config_text missing"))?;
        let checksum =
            json_str(text, "checksum").ok_or_else(|| bad("snapshot.checksum missing"))?;
        Ok(Self {
            schema,
            component,
            created_at,
            reason,
            config_text,
            checksum,
        })
    }

    /// Full offline validation: shape, component, checksum and config text.
    pub fn validate(&self, expected_component: &str) -> Result<(), StoreError> {
        let bad = |message: &str| StoreError::new("invalid_config", message);
        if self.schema != 1 {
            return Err(bad("snapshot.schema must be 1"));
        }
        if self.component != expected_component || !valid_component(&self.component) {
            return Err(bad("snapshot.component mismatch"));
        }
        if !self.created_at.ends_with('Z') || self.created_at.len() < 20 {
            return Err(bad("snapshot.created_at must be RFC3339 UTC"));
        }
        let expected = format!("sha256:{}", sha256_hex(self.config_text.as_bytes()));
        if self.checksum != expected || !valid_checksum(&self.checksum) {
            return Err(bad("snapshot.checksum mismatch"));
        }
        let flat = validate_text(&self.config_text)
            .map_err(|err| bad(&format!("snapshot config invalid: {}", err)))?;
        MdnsConfig::from_flat(&flat)
            .map_err(|err| bad(&format!("snapshot mdns config invalid: {}", err)))?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: String,
    pub created_at: String,
    pub reason: String,
    pub checksum: String,
    pub size: u64,
}

pub struct ConfigStore {
    path: PathBuf,
    history_dir: PathBuf,
    history_limit: usize,
    component: String,
    raw: RwLock<String>,
    saves: AtomicU64,
    reloads: AtomicU64,
}

impl ConfigStore {
    pub fn new(
        path: impl Into<PathBuf>,
        history_dir: impl Into<PathBuf>,
        history_limit: usize,
        component: &str,
        raw: String,
    ) -> io::Result<Self> {
        let history_dir = history_dir.into();
        fs::create_dir_all(&history_dir)?;
        Ok(Self {
            path: path.into(),
            history_dir,
            history_limit: history_limit.clamp(1, 1000),
            component: component.to_string(),
            raw: RwLock::new(raw),
            saves: AtomicU64::new(0),
            reloads: AtomicU64::new(0),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn history_limit(&self) -> usize {
        self.history_limit
    }

    pub fn raw(&self) -> String {
        self.raw.read().unwrap().clone()
    }

    pub fn set_raw(&self, text: &str) {
        *self.raw.write().unwrap() = text.to_string();
    }

    pub fn saves(&self) -> u64 {
        self.saves.load(Ordering::Relaxed)
    }

    pub fn reloads(&self) -> u64 {
        self.reloads.load(Ordering::Relaxed)
    }

    pub fn inc_reloads(&self) {
        self.reloads.fetch_add(1, Ordering::Relaxed);
    }

    /// F-E08: export the current config as a self-contained snapshot.
    pub fn export(&self, reason: &str) -> Snapshot {
        Snapshot::new(&self.component, reason, &self.raw())
    }

    /// F-E05: validate -> snapshot current -> atomic replace -> update raw.
    /// Returns the id of the just-written `before-save` snapshot.
    pub fn save(&self, text: &str, reason: &str) -> Result<String, StoreError> {
        validate_config_text(text)?;
        let before = Snapshot::new(&self.component, &format!("before-{}", reason), &self.raw());
        let id = self.write_snapshot(&before)?;
        self.atomic_write(text).map_err(io_error)?;
        self.set_raw(text);
        self.saves.fetch_add(1, Ordering::Relaxed);
        Ok(id)
    }

    /// F-E06: list bounded history, newest first.
    pub fn list(&self) -> Vec<HistoryEntry> {
        let mut files: Vec<(SystemTime, PathBuf)> = match fs::read_dir(&self.history_dir) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().to_str().is_some_and(valid_snapshot_id))
                .filter_map(|entry| {
                    let modified = entry.metadata().and_then(|meta| meta.modified()).ok()?;
                    Some((modified, entry.path()))
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        files.sort_by_key(|item| std::cmp::Reverse(item.0));
        files
            .into_iter()
            .filter_map(|(_, path)| {
                let id = path.file_name()?.to_str()?.to_string();
                let text = fs::read_to_string(&path).ok()?;
                let size = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
                let snap = Snapshot::from_json(&text).ok();
                Some(HistoryEntry {
                    id,
                    created_at: snap
                        .as_ref()
                        .map(|s| s.created_at.clone())
                        .unwrap_or_default(),
                    reason: snap.as_ref().map(|s| s.reason.clone()).unwrap_or_default(),
                    checksum: snap
                        .as_ref()
                        .map(|s| s.checksum.clone())
                        .unwrap_or_default(),
                    size,
                })
            })
            .collect()
    }

    pub fn history_len(&self) -> usize {
        self.list().len()
    }

    /// F-E07: validate the snapshot id, verify its checksum, snapshot the
    /// current config, then atomically restore the snapshot's config text.
    /// Returns the text that should be hot-reloaded by the caller.
    pub fn rollback(&self, id: &str) -> Result<String, StoreError> {
        if !valid_snapshot_id(id) {
            return Err(StoreError::new("invalid_config", "invalid snapshot id"));
        }
        let path = self.history_dir.join(id);
        let text = fs::read_to_string(&path)
            .map_err(|_| StoreError::new("not_found", "snapshot not found"))?;
        let snap = Snapshot::from_json(&text)?;
        snap.validate(&self.component)?;
        validate_config_text(&snap.config_text)?;
        let before = Snapshot::new(
            &self.component,
            &format!("before-rollback:{}", id),
            &self.raw(),
        );
        self.write_snapshot(&before)?;
        self.atomic_write(&snap.config_text).map_err(io_error)?;
        self.set_raw(&snap.config_text);
        self.saves.fetch_add(1, Ordering::Relaxed);
        Ok(snap.config_text)
    }

    /// F-E08: validate an imported snapshot, then persist its config text.
    pub fn import(&self, snap: &Snapshot) -> Result<String, StoreError> {
        snap.validate(&self.component)?;
        self.save(&snap.config_text, "import")?;
        Ok(snap.config_text.clone())
    }

    pub fn read_snapshot(&self, id: &str) -> Result<Snapshot, StoreError> {
        if !valid_snapshot_id(id) {
            return Err(StoreError::new("invalid_config", "invalid snapshot id"));
        }
        let path = self.history_dir.join(id);
        let text = fs::read_to_string(&path)
            .map_err(|_| StoreError::new("not_found", "snapshot not found"))?;
        let snap = Snapshot::from_json(&text)?;
        snap.validate(&self.component)?;
        Ok(snap)
    }

    fn atomic_write(&self, text: &str) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let temp = PathBuf::from(format!("{}.tmp", self.path.display()));
        fs::write(&temp, text)?;
        fs::rename(&temp, &self.path)
    }

    fn write_snapshot(&self, snap: &Snapshot) -> Result<String, StoreError> {
        let id = next_snapshot_id();
        fs::write(self.history_dir.join(&id), snap.to_json()).map_err(io_error)?;
        self.prune(Some(&id)).map_err(io_error)?;
        Ok(id)
    }

    /// Keep at most `history_limit` snapshots by mtime; never delete `keep`
    /// (the file we just wrote) even if clocks are coarse.
    fn prune(&self, keep: Option<&str>) -> io::Result<()> {
        let mut files: Vec<(SystemTime, PathBuf)> = fs::read_dir(&self.history_dir)?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_str().is_some_and(valid_snapshot_id))
            .filter_map(|entry| {
                let modified = entry.metadata().and_then(|meta| meta.modified()).ok()?;
                Some((modified, entry.path()))
            })
            .collect();
        files.sort_by_key(|item| std::cmp::Reverse(item.0));
        for (_, path) in files.iter().skip(self.history_limit) {
            if keep.is_some_and(|id| path.file_name().and_then(|n| n.to_str()) == Some(id)) {
                continue;
            }
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

/// F-E02: offline validation of both the common and component namespaces.
pub fn validate_config_text(text: &str) -> Result<(), StoreError> {
    let flat = validate_text(text).map_err(|err| {
        StoreError::new("invalid_config", format!("common config invalid: {}", err))
    })?;
    MdnsConfig::from_flat(&flat)
        .map(|_| ())
        .map_err(|err| StoreError::new("invalid_config", format!("mdns config invalid: {}", err)))
}

/// `^config-[0-9TZa-f-]+\.json$` — also the path-traversal guard.
pub fn valid_snapshot_id(id: &str) -> bool {
    let stem = match id
        .strip_prefix("config-")
        .and_then(|rest| rest.strip_suffix(".json"))
    {
        Some(stem) => stem,
        None => return false,
    };
    !stem.is_empty()
        && stem.chars().all(|ch| {
            ch.is_ascii_digit() || ch == 'T' || ch == 'Z' || ch == '-' || ('a'..='f').contains(&ch)
        })
}

fn valid_checksum(value: &str) -> bool {
    let hex = match value.strip_prefix("sha256:") {
        Some(hex) => hex,
        None => return false,
    };
    hex.len() == 64
        && hex
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
}

fn valid_component(value: &str) -> bool {
    value.starts_with("lan-")
        && value.len() > 4
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn next_snapshot_id() -> String {
    format!(
        "config-{}Z-{}-{}.json",
        utc_stamp(),
        random_hex(3),
        random_hex(2)
    )
}

fn utc_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let sod = secs % 86_400;
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}",
        year,
        month,
        day,
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

fn utc_now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (year, month, day) = civil_from_days((secs / 86_400) as i64);
    let sod = secs % 86_400;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Howard Hinnant's civil-from-days algorithm (proleptic Gregorian, UTC).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (if month <= 2 { y + 1 } else { y }, month, day)
}

// ------------------------------------------------------------- JSON helpers

fn field_start<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{}\"", key);
    let start = body.find(&needle)? + needle.len();
    let rest = &body[start..];
    let colon = rest.find(':')?;
    Some(rest[colon + 1..].trim_start())
}

pub fn json_str(body: &str, key: &str) -> Option<String> {
    let value = field_start(body, key)?;
    let bytes = value.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut index = 1usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Some(out),
            b'\\' => {
                index += 1;
                if index >= bytes.len() {
                    return None;
                }
                match bytes[index] {
                    b'n' => out.push('\n'),
                    b't' => out.push('\t'),
                    b'r' => out.push('\r'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000c}'),
                    b'u' => {
                        if index + 4 >= bytes.len() {
                            return None;
                        }
                        let hex = std::str::from_utf8(&bytes[index + 1..index + 5]).ok()?;
                        let code = u32::from_str_radix(hex, 16).ok()?;
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        index += 4;
                    }
                    other => out.push(other as char),
                }
            }
            _ => {
                let rest = std::str::from_utf8(&bytes[index..]).ok()?;
                let ch = rest.chars().next()?;
                out.push(ch);
                index += ch.len_utf8() - 1;
            }
        }
        index += 1;
    }
    None
}

fn json_i64(body: &str, key: &str) -> Option<i64> {
    let value = field_start(body, key)?;
    let end = value
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '-'))
        .unwrap_or(value.len());
    value[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "\
version: 1
component: lan-mdns
server:
  listen: 127.0.0.1
  port: 8080
  webui: true
mdns:
  group: 224.0.0.251
  port: 5353
";

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lanstudio-store-{}-{}-{}",
            label,
            std::process::id(),
            random_hex(4)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn snapshot_roundtrip_preserves_yaml_and_checksum() {
        let snap = Snapshot::new("lan-mdns", "before-save", CONFIG);
        let json = snap.to_json();
        let parsed = Snapshot::from_json(&json).unwrap();
        assert_eq!(parsed.config_text, CONFIG);
        assert_eq!(parsed.checksum, snap.checksum);
        parsed.validate("lan-mdns").unwrap();
        assert!(parsed.validate("lan-relay").is_err());
    }

    #[test]
    fn tampered_checksum_is_rejected() {
        let mut snap = Snapshot::new("lan-mdns", "before-save", CONFIG);
        snap.checksum = format!("sha256:{}", "0".repeat(64));
        assert!(snap.validate("lan-mdns").is_err());
    }

    #[test]
    fn snapshot_ids_whitelist_blocks_path_traversal() {
        let id = next_snapshot_id();
        assert!(valid_snapshot_id(&id), "{}", id);
        for bad in [
            "../config.yaml",
            "config-../evil.json",
            "config-ABC.json",
            "config-1.jsonx",
            "config-.json",
            "other.json",
            "config-a/b.json",
        ] {
            assert!(!valid_snapshot_id(bad), "{}", bad);
        }
    }

    #[test]
    fn save_rollback_and_bounded_history() {
        let dir = temp_dir("lifecycle");
        let path = dir.join("config.yaml");
        let history = dir.join("history");
        fs::write(&path, CONFIG).unwrap();
        let store = ConfigStore::new(&path, &history, 3, "lan-mdns", CONFIG.to_string()).unwrap();

        let modified = CONFIG.replace("port: 8080", "port: 9090");
        let id = store.save(&modified, "save").unwrap();
        assert!(valid_snapshot_id(&id));
        assert_eq!(store.raw(), modified);
        assert!(fs::read_to_string(&path).unwrap().contains("port: 9090"));
        assert_eq!(store.saves(), 1);

        let entries = store.list();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].reason.starts_with("before-save"));

        let restored = store.rollback(&entries[0].id).unwrap();
        assert_eq!(restored, CONFIG);
        assert_eq!(store.raw(), CONFIG);
        assert!(fs::read_to_string(&path).unwrap().contains("port: 8080"));

        for index in 0..8 {
            let text = CONFIG.replace("port: 8080", &format!("port: {}", 9000 + index));
            store.save(&text, "batch").unwrap();
        }
        assert!(store.list().len() <= 3, "history must stay bounded");
        assert!(store.rollback("../evil.json").is_err());

        let exported = store.export("manual");
        exported.validate("lan-mdns").unwrap();

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn import_rejects_invalid_config_text() {
        let dir = temp_dir("import");
        let path = dir.join("config.yaml");
        let history = dir.join("history");
        let store = ConfigStore::new(&path, &history, 5, "lan-mdns", CONFIG.to_string()).unwrap();
        let mut snap = Snapshot::new("lan-mdns", "external", "version: 2\ncomponent: lan-mdns\n");
        snap.checksum = format!("sha256:{}", sha256_hex(snap.config_text.as_bytes()));
        assert!(store.import(&snap).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
