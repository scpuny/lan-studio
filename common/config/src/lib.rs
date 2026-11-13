//! Configuration loader for the Lan Studio unified schema.
//!
//! Zero third-party crates. The parser handles the YAML subset used by
//! `contracts/config.schema.json`: nested maps, inline scalars, quoted values,
//! comments, and lists of maps (`records` / `services`). List items are
//! flattened into `prefix[0].field` keys so callers need no YAML AST.
//! See docs/ADR/0001 for the M1 decision and its M2 extension.

use std::collections::HashMap;
use std::fmt;
use std::fs;

#[derive(Debug, Clone)]
pub struct Config {
    pub version: i64,
    pub component: String,
    pub listen: String,
    pub port: u16,
    pub webui: bool,
    pub admin_token: String,
    pub log_level: String,
    pub raw: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            component: "lan-mdns".to_string(),
            listen: "127.0.0.1".to_string(),
            port: 8080,
            webui: true,
            admin_token: String::new(),
            log_level: "info".to_string(),
            raw: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub code: &'static str,
    pub field: String,
    pub detail: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} ({})", self.code, self.field, self.detail)
    }
}

impl std::error::Error for ConfigError {}

fn strip_comment(line: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    for (index, ch) in line.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return &line[..index],
            _ => {}
        }
    }
    line
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

#[derive(Debug)]
struct Frame {
    indent: usize,
    prefix: String,
    next_index: usize,
}

/// Flatten a YAML document into dotted keys. Lists become `prefix[i].field`.
pub fn parse_flat(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut stack: Vec<Frame> = vec![Frame {
        indent: 0,
        prefix: String::new(),
        next_index: 0,
    }];
    for raw_line in text.lines() {
        let line = strip_comment(raw_line).trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let body = line.trim();
        while stack.len() > 1 && indent <= stack.last().map_or(0, |f| f.indent) {
            stack.pop();
        }
        let parent = stack.last().map_or("", |f| f.prefix.as_str()).to_string();

        if let Some(item) = body.strip_prefix("- ") {
            let item_prefix = {
                let frame = stack.last_mut().expect("root frame");
                let index = frame.next_index;
                frame.next_index += 1;
                format!("{}[{}]", frame.prefix, index)
            };
            if let Some((key, value)) = item.split_once(':') {
                let full = format!("{}.{}", item_prefix, key.trim());
                if value.trim().is_empty() {
                    stack.push(Frame {
                        indent,
                        prefix: full,
                        next_index: 0,
                    });
                    continue;
                }
                out.insert(full, unquote(value));
            } else {
                out.insert(item_prefix.clone(), unquote(item));
            }
            stack.push(Frame {
                indent,
                prefix: item_prefix,
                next_index: 0,
            });
            continue;
        }

        if let Some((key, value)) = body.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            let full = if parent.is_empty() {
                key.to_string()
            } else {
                format!("{}.{}", parent, key)
            };
            if value.is_empty() {
                stack.push(Frame {
                    indent,
                    prefix: full,
                    next_index: 0,
                });
            } else if value == "[]" || value == "{}" {
                out.insert(full, String::new());
            } else {
                out.insert(full, unquote(value));
            }
        }
    }
    out
}

pub fn get_str(flat: &HashMap<String, String>, key: &str, default: &str) -> String {
    flat.get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

pub fn get_bool(flat: &HashMap<String, String>, key: &str, default: bool) -> bool {
    flat.get(key)
        .map(|value| value.eq_ignore_ascii_case("true") || value == "1")
        .unwrap_or(default)
}

pub fn get_u16(flat: &HashMap<String, String>, key: &str, default: u16) -> u16 {
    flat.get(key)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
}

pub fn get_u32(flat: &HashMap<String, String>, key: &str, default: u32) -> u32 {
    flat.get(key)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

pub fn get_u64(flat: &HashMap<String, String>, key: &str, default: u64) -> u64 {
    flat.get(key)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

/// Number of list items under `prefix`, derived from `prefix[i].*` keys.
pub fn list_len(flat: &HashMap<String, String>, prefix: &str) -> usize {
    let marker = format!("{}[", prefix);
    let mut indices: Vec<usize> = flat
        .keys()
        .filter_map(|key| key.strip_prefix(&marker))
        .filter_map(|tail| tail.split(']').next())
        .filter_map(|index| index.parse::<usize>().ok())
        .collect();
    indices.sort_unstable();
    indices.dedup();
    indices.last().map_or(0, |last| last + 1)
}

/// Offline validation of the common schema segments (F-E02). No network is
/// touched, so it is safe to run before binding any socket.
pub fn validate_common(flat: &HashMap<String, String>) -> Result<(), ConfigError> {
    let bad = |field: &str, detail: &str| ConfigError {
        code: "invalid_config",
        field: field.to_string(),
        detail: detail.to_string(),
    };

    let version = get_str(flat, "version", "");
    if version != "1" {
        return Err(bad("version", "must be 1"));
    }
    let component = get_str(flat, "component", "");
    let valid_component = component.starts_with("lan-")
        && component.len() > 4
        && component
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
    if !valid_component {
        return Err(bad("component", "must match ^lan-[a-z0-9-]+$"));
    }
    let level = get_str(flat, "server.log_level", "info");
    if !["debug", "info", "warning", "error"].contains(&level.as_str()) {
        return Err(bad("server.log_level", "invalid log level"));
    }
    let log_level = get_str(flat, "logging.level", "info");
    if !["debug", "info", "warning", "error"].contains(&log_level.as_str()) {
        return Err(bad("logging.level", "invalid log level"));
    }
    if let Some(raw) = flat.get("server.port") {
        let port = raw.parse::<u32>().unwrap_or(0);
        if !(1..=65535).contains(&port) {
            return Err(bad("server.port", "must be 1..65535"));
        }
    }
    let history = get_u32(flat, "observability.history_limit", 30);
    if !(1..=1000).contains(&history) {
        return Err(bad("observability.history_limit", "must be 1..1000"));
    }
    let max_bytes = get_u64(flat, "logging.max_bytes", 1_048_576);
    if max_bytes < 1024 {
        return Err(bad("logging.max_bytes", "must be >= 1024"));
    }
    let backups = get_u32(flat, "logging.backup_count", 3);
    if backups > 100 {
        return Err(bad("logging.backup_count", "must be <= 100"));
    }
    let retries = get_u32(flat, "alerts.max_retries", 2);
    if retries > 2 {
        return Err(bad("alerts.max_retries", "must be <= 2"));
    }
    Ok(())
}

pub fn apply_flat(cfg: &mut Config, flat: &HashMap<String, String>) {
    if let Some(value) = flat.get("version") {
        if let Ok(number) = value.parse::<i64>() {
            cfg.version = number;
        }
    }
    if let Some(value) = flat.get("component") {
        cfg.component = value.clone();
    }
    if let Some(value) = flat.get("server.listen") {
        cfg.listen = value.clone();
    }
    if let Some(value) = flat.get("server.webui") {
        cfg.webui = value.eq_ignore_ascii_case("true") || value == "1";
    }
    if let Some(value) = flat.get("server.admin_token") {
        cfg.admin_token = value.clone();
    }
    if let Some(value) = flat.get("server.log_level") {
        cfg.log_level = value.clone();
    }
    if let Some(value) = flat.get("server.port") {
        if let Ok(port) = value.parse::<u16>() {
            cfg.port = port;
        }
    }
}

pub fn load(path: &str) -> Config {
    let mut cfg = Config::default();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return cfg,
    };
    cfg.raw = text.clone();
    let flat = parse_flat(&text);
    apply_flat(&mut cfg, &flat);
    cfg
}

/// Parse and validate a config text without binding any socket (F-E02).
pub fn validate_text(text: &str) -> Result<HashMap<String, String>, ConfigError> {
    let flat = parse_flat(text);
    validate_common(&flat)?;
    Ok(flat)
}

/// Resolve the listening port. `server.port` in config.schema.json is the
/// authoritative value; `LAN_STUDIO_PORT` remains a test/CI isolation hook so
/// smoke tests can bind a high port without editing the config file.
pub fn port_from_env(configured: u16) -> u16 {
    std::env::var("LAN_STUDIO_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(configured)
}

pub fn config_path_from_env() -> String {
    std::env::var("LAN_STUDIO_CONFIG").unwrap_or_else(|_| "config.yaml".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_level_yaml() {
        let text = "version: 1\ncomponent: lan-mdns\nserver:\n  listen: \"0.0.0.0\"\n  webui: true\n  admin_token: \"\"\n";
        let flat = parse_flat(text);
        assert_eq!(flat.get("version").unwrap(), "1");
        assert_eq!(flat.get("component").unwrap(), "lan-mdns");
        assert_eq!(flat.get("server.listen").unwrap(), "0.0.0.0");
        assert_eq!(flat.get("server.webui").unwrap(), "true");
        assert_eq!(flat.get("server.admin_token").unwrap(), "");
    }

    #[test]
    fn strips_comments_but_keeps_quoted_hash() {
        let flat = parse_flat("server:\n  admin_token: \"a#b\" # note\n");
        assert_eq!(flat.get("server.admin_token").unwrap(), "a#b");
    }

    #[test]
    fn applies_common_fields() {
        let mut cfg = Config::default();
        let flat = parse_flat(
            "version: 2\ncomponent: lan-relay\nserver:\n  listen: 0.0.0.0\n  webui: false\n",
        );
        apply_flat(&mut cfg, &flat);
        assert_eq!(cfg.version, 2);
        assert_eq!(cfg.component, "lan-relay");
        assert_eq!(cfg.listen, "0.0.0.0");
        assert!(!cfg.webui);
    }

    #[test]
    fn parses_list_of_maps_into_indexed_keys() {
        let text = "\
mdns:
  records:
    - name: nas.local
      type: A
      value: 192.168.5.10
      ttl: 120
    - name: printer.local
      type: A
      value: 192.168.5.11
  services:
    - instance: Web Server
      type: _http._tcp.local
      host: nas.local
      port: 8080
  ttl: 90
";
        let flat = parse_flat(text);
        assert_eq!(flat.get("mdns.records[0].name").unwrap(), "nas.local");
        assert_eq!(flat.get("mdns.records[0].type").unwrap(), "A");
        assert_eq!(flat.get("mdns.records[1].name").unwrap(), "printer.local");
        assert_eq!(flat.get("mdns.services[0].port").unwrap(), "8080");
        assert_eq!(flat.get("mdns.ttl").unwrap(), "90");
        assert_eq!(list_len(&flat, "mdns.records"), 2);
        assert_eq!(list_len(&flat, "mdns.services"), 1);
        assert_eq!(list_len(&flat, "mdns.missing"), 0);
    }

    #[test]
    fn empty_inline_lists_are_safe() {
        let flat = parse_flat("alerts:\n  webhooks: []\nmdns:\n  interfaces: []\n");
        assert_eq!(flat.get("alerts.webhooks").unwrap(), "");
        assert_eq!(list_len(&flat, "mdns.interfaces"), 0);
    }

    #[test]
    fn typed_getters_fall_back_on_bad_values() {
        let flat = parse_flat("mdns:\n  ttl: abc\n  announce: 7\n");
        assert_eq!(get_u32(&flat, "mdns.ttl", 120), 120);
        assert_eq!(get_u16(&flat, "mdns.announce", 3), 7);
        assert!(!get_bool(&flat, "mdns.enabled", false));
        assert_eq!(get_str(&flat, "mdns.group", "224.0.0.251"), "224.0.0.251");
    }

    #[test]
    fn server_port_is_part_of_the_frozen_schema() {
        let text = "version: 1\ncomponent: lan-mdns\nserver:\n  port: 9100\n  webui: true\n";
        let flat = validate_text(text).unwrap();
        let mut cfg = Config::default();
        apply_flat(&mut cfg, &flat);
        assert_eq!(cfg.port, 9100);
        let err =
            validate_text("version: 1\ncomponent: lan-mdns\nserver:\n  port: 70000\n").unwrap_err();
        assert_eq!(err.field, "server.port");
    }

    #[test]
    fn offline_validation_accepts_template_and_rejects_bad_fields() {
        let good = "version: 1\ncomponent: lan-mdns\nserver:\n  webui: true\n  log_level: info\n";
        assert!(validate_text(good).is_ok());
        let cases = [
            ("version: 2\ncomponent: lan-mdns\n", "version"),
            ("version: 1\ncomponent: BadName\n", "component"),
            (
                "version: 1\ncomponent: lan-mdns\nserver:\n  log_level: trace\n",
                "server.log_level",
            ),
            (
                "version: 1\ncomponent: lan-mdns\nlogging:\n  max_bytes: 10\n",
                "logging.max_bytes",
            ),
            (
                "version: 1\ncomponent: lan-mdns\nalerts:\n  max_retries: 9\n",
                "alerts.max_retries",
            ),
            (
                "version: 1\ncomponent: lan-mdns\nobservability:\n  history_limit: 0\n",
                "observability.history_limit",
            ),
        ];
        for (text, field) in cases {
            let err = validate_text(text).unwrap_err();
            assert_eq!(err.code, "invalid_config");
            assert_eq!(err.field, field);
        }
    }
}
