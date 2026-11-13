//! Minimal configuration loader for the M1 skeleton.
//!
//! M1 deliberately ships zero third-party crates. This module parses the small
//! YAML subset used by `contracts/examples/config.sample.yaml` (top-level
//! scalars plus one level of sections). See docs/ADR/0001 for the M2 plan to
//! replace it with a full YAML implementation aligned to config.schema.json.

use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone)]
pub struct Config {
    pub version: i64,
    pub component: String,
    pub listen: String,
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
            webui: true,
            admin_token: String::new(),
            log_level: "info".to_string(),
            raw: String::new(),
        }
    }
}

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

/// Flatten a small YAML document into dotted keys, e.g. `server.listen`.
pub fn parse_flat(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut section = String::new();
    for raw_line in text.lines() {
        let line = strip_comment(raw_line).trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let body = line.trim();
        if let Some((key, value)) = body.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            if indent == 0 {
                if value.is_empty() {
                    section = key.to_string();
                } else {
                    out.insert(key.to_string(), unquote(value));
                    section.clear();
                }
            } else {
                let full = if section.is_empty() {
                    key.to_string()
                } else {
                    format!("{}.{}", section, key)
                };
                out.insert(full, unquote(value));
            }
        }
    }
    out
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
        cfg.webui = value.eq_ignore_ascii_case("true");
    }
    if let Some(value) = flat.get("server.admin_token") {
        cfg.admin_token = value.clone();
    }
    if let Some(value) = flat.get("server.log_level") {
        cfg.log_level = value.clone();
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

/// M1 keeps the listening port out of the frozen config schema; it is supplied
/// by the environment. M2 will fold it into config.schema.json properly.
pub fn port_from_env() -> u16 {
    std::env::var("LAN_STUDIO_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8080)
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
}
