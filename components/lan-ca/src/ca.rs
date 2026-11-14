//! Lightweight internal CA — single self-signed root (PRD §4.4 / D8).
//!
//! Trust model: one local root key. Each certificate is a canonical record
//! whose TBS (to-be-signed) string is authenticated with HMAC-SHA256 under the
//! root key. This is an **internal credential**, not an X.509 public-key
//! certificate: it is intentionally off the browser TLS trust chain, and the
//! whole workspace stays free of third-party crypto crates. See README.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use common_crypto::{from_hex, hmac_sha256, random_hex, sha256_hex, to_hex};

#[derive(Debug, Clone)]
pub struct RootKey {
    bytes: Vec<u8>,
    pub fingerprint: String,
}

impl RootKey {
    pub fn load_or_create(path: &str) -> io::Result<Self> {
        let bytes = match fs::read_to_string(path) {
            Ok(text) => from_hex(text.trim()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "root key is not valid hex")
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let generated = random_hex(32);
                fs::write(path, &generated)?;
                set_private_permissions(path);
                from_hex(&generated).unwrap_or_default()
            }
            Err(error) => return Err(error),
        };
        if bytes.len() < 16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "root key is too short",
            ));
        }
        let fingerprint = sha256_hex(&bytes)[..16].to_string();
        Ok(Self { bytes, fingerprint })
    }

    pub fn sign(&self, tbs: &str) -> String {
        to_hex(&hmac_sha256(&self.bytes, tbs.as_bytes()))
    }

    pub fn verify(&self, tbs: &str, signature: &str) -> bool {
        common_crypto::constant_eq(self.sign(tbs).as_bytes(), signature.as_bytes())
    }
}

#[cfg(unix)]
fn set_private_permissions(path: &str) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_private_permissions(_: &str) {}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Unix seconds → RFC 3339 UTC (`Z` suffix), civil-date algorithm.
pub fn rfc3339(secs: u64) -> String {
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

#[derive(Debug, Clone)]
pub struct Certificate {
    pub serial: String,
    pub subject: String,
    pub sans: Vec<String>,
    pub not_before: String,
    pub not_after: String,
    pub issuer: String,
    pub signature: String,
    pub status: String,
    pub revoked_at: Option<String>,
    pub revoke_reason: Option<String>,
}

impl Certificate {
    /// Canonical TBS: field order is part of the signature contract.
    pub fn tbs(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.serial,
            self.subject,
            self.sans.join(","),
            self.not_before,
            self.not_after,
            self.issuer,
        )
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"serial\":{},\"subject\":{},\"sans\":{},\"not_before\":{},\"not_after\":{},\"issuer\":{},\"signature\":{},\"status\":{},\"revoked_at\":{},\"revoke_reason\":{}}}",
            q(&self.serial),
            q(&self.subject),
            json_str_array(&self.sans),
            q(&self.not_before),
            q(&self.not_after),
            q(&self.issuer),
            q(&self.signature),
            q(&self.status),
            self.revoked_at.as_deref().map(q).unwrap_or_else(|| "null".to_string()),
            self.revoke_reason.as_deref().map(q).unwrap_or_else(|| "null".to_string()),
        )
    }

    pub fn from_json(text: &str) -> Option<Self> {
        Some(Self {
            serial: json_value(text, "serial")?,
            subject: json_value(text, "subject")?,
            sans: parse_str_array(text, "sans")?,
            not_before: json_value(text, "not_before")?,
            not_after: json_value(text, "not_after")?,
            issuer: json_value(text, "issuer")?,
            signature: json_value(text, "signature")?,
            status: json_value(text, "status")?,
            revoked_at: optional_value(text, "revoked_at"),
            revoke_reason: optional_value(text, "revoke_reason"),
        })
    }

    /// PEM-like envelope carrying a base64 canonical record (no external deps).
    pub fn to_pem(&self) -> String {
        let body = base64_encode(self.to_json().as_bytes());
        let mut out = String::from("-----BEGIN LAN STUDIO CERTIFICATE-----\n");
        for chunk in body.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
            out.push('\n');
        }
        out.push_str("-----END LAN STUDIO CERTIFICATE-----\n");
        out
    }
}

#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub time: String,
    pub action: String,
    pub serial: String,
    pub subject: String,
    pub detail: String,
}

impl AuditEntry {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"time\":{},\"action\":{},\"serial\":{},\"subject\":{},\"detail\":{}}}",
            q(&self.time),
            q(&self.action),
            q(&self.serial),
            q(&self.subject),
            q(&self.detail),
        )
    }
}

pub struct CaStore {
    pub root: RootKey,
    pub certificates: Vec<Certificate>,
    pub audit: VecDeque<AuditEntry>,
    path: String,
    max_certificates: usize,
    audit_capacity: usize,
}

impl CaStore {
    pub fn open(root_path: &str, store_path: &str, max_certificates: usize) -> io::Result<Self> {
        let root = RootKey::load_or_create(root_path)?;
        let mut certificates = Vec::new();
        let mut audit = VecDeque::new();
        if let Ok(text) = fs::read_to_string(store_path) {
            for line in text.lines() {
                if let Some(body) = line.strip_prefix("CERT ") {
                    if let Some(cert) = Certificate::from_json(body) {
                        certificates.push(cert);
                    }
                } else if let Some(body) = line.strip_prefix("AUDIT ") {
                    if let Some(entry) = parse_audit(body) {
                        audit.push_back(entry);
                    }
                }
            }
        }
        Ok(Self {
            root,
            certificates,
            audit,
            path: store_path.to_string(),
            max_certificates: max_certificates.max(1),
            audit_capacity: 500,
        })
    }

    fn record(&mut self, action: &str, serial: &str, subject: &str, detail: &str) {
        self.audit.push_back(AuditEntry {
            time: rfc3339(now_secs()),
            action: action.to_string(),
            serial: serial.to_string(),
            subject: subject.to_string(),
            detail: detail.to_string(),
        });
        while self.audit.len() > self.audit_capacity {
            self.audit.pop_front();
        }
    }

    /// Atomic persist: one line per record, never contains the root key.
    pub fn persist(&self) -> io::Result<()> {
        let mut text = String::new();
        for cert in &self.certificates {
            text.push_str("CERT ");
            text.push_str(&cert.to_json());
            text.push('\n');
        }
        for entry in &self.audit {
            text.push_str("AUDIT ");
            text.push_str(&entry.to_json());
            text.push('\n');
        }
        let tmp = format!("{}.tmp", self.path);
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.path)
    }

    pub fn validate_subject(
        &self,
        subject: &str,
        allowed_suffixes: &[String],
    ) -> Result<(), String> {
        if subject.is_empty() || subject.len() > 253 {
            return Err("subject must be 1..253 characters".to_string());
        }
        if !subject
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '.')
        {
            return Err("subject must be lowercase alphanumeric, '-' or '.'".to_string());
        }
        if subject.starts_with('.') || subject.ends_with('.') || subject.contains("..") {
            return Err("subject has an empty label".to_string());
        }
        if !allowed_suffixes.is_empty()
            && !allowed_suffixes
                .iter()
                .any(|suffix| subject.ends_with(suffix))
        {
            return Err("subject is outside the allowed internal suffixes".to_string());
        }
        Ok(())
    }

    pub fn issue(
        &mut self,
        subject: &str,
        sans: Vec<String>,
        days: u64,
        allowed_suffixes: &[String],
    ) -> Result<Certificate, String> {
        self.validate_subject(subject, allowed_suffixes)?;
        if days == 0 {
            return Err("days must be >= 1".to_string());
        }
        if self.certificates.len() >= self.max_certificates {
            return Err("certificate store is full".to_string());
        }
        let start = now_secs();
        let mut cert = Certificate {
            serial: random_hex(8),
            subject: subject.to_string(),
            sans,
            not_before: rfc3339(start),
            not_after: rfc3339(start + days.saturating_mul(86_400)),
            issuer: self.root.fingerprint.clone(),
            signature: String::new(),
            status: "active".to_string(),
            revoked_at: None,
            revoke_reason: None,
        };
        cert.signature = self.root.sign(&cert.tbs());
        self.certificates.push(cert.clone());
        self.record("issue", &cert.serial, subject, &format!("days={}", days));
        self.persist().map_err(|error| error.to_string())?;
        Ok(cert)
    }

    pub fn renew(&mut self, serial: &str, days: u64) -> Result<Certificate, String> {
        if days == 0 {
            return Err("days must be >= 1".to_string());
        }
        let cert = self
            .certificates
            .iter_mut()
            .find(|item| item.serial == serial)
            .ok_or_else(|| "serial not found".to_string())?;
        if cert.status != "active" {
            return Err("only active certificates can be renewed".to_string());
        }
        let start = now_secs();
        cert.not_before = rfc3339(start);
        cert.not_after = rfc3339(start + days.saturating_mul(86_400));
        cert.signature = self.root.sign(&cert.tbs());
        let result = cert.clone();
        let subject = result.subject.clone();
        self.record("renew", serial, &subject, &format!("days={}", days));
        self.persist().map_err(|error| error.to_string())?;
        Ok(result)
    }

    pub fn revoke(&mut self, serial: &str, reason: &str) -> Result<Certificate, String> {
        let cert = self
            .certificates
            .iter_mut()
            .find(|item| item.serial == serial)
            .ok_or_else(|| "serial not found".to_string())?;
        if cert.status == "revoked" {
            return Err("certificate is already revoked".to_string());
        }
        cert.status = "revoked".to_string();
        cert.revoked_at = Some(rfc3339(now_secs()));
        cert.revoke_reason = Some(reason.to_string());
        let result = cert.clone();
        let subject = result.subject.clone();
        self.record("revoke", serial, &subject, reason);
        self.persist().map_err(|error| error.to_string())?;
        Ok(result)
    }

    pub fn verify(&self, cert: &Certificate) -> bool {
        cert.issuer == self.root.fingerprint && self.root.verify(&cert.tbs(), &cert.signature)
    }

    pub fn crl(&self) -> Vec<Certificate> {
        self.certificates
            .iter()
            .filter(|cert| cert.status == "revoked")
            .cloned()
            .collect()
    }

    pub fn status_json(&self) -> String {
        let active = self
            .certificates
            .iter()
            .filter(|c| c.status == "active")
            .count();
        let revoked = self
            .certificates
            .iter()
            .filter(|c| c.status == "revoked")
            .count();
        format!(
            "{{\"root_fingerprint\":{},\"issued\":{},\"active\":{},\"revoked\":{},\"audit_entries\":{},\"max_certificates\":{}}}",
            q(&self.root.fingerprint),
            self.certificates.len(),
            active,
            revoked,
            self.audit.len(),
            self.max_certificates,
        )
    }

    /// Safe export: certificate records + audit only. Never contains the root
    /// key bytes (PRD §4.4 acceptance).
    pub fn export_json(&self) -> String {
        let certs: Vec<String> = self.certificates.iter().map(Certificate::to_json).collect();
        let audits: Vec<String> = self.audit.iter().map(AuditEntry::to_json).collect();
        format!(
            "{{\"schema\":1,\"component\":\"lan-ca\",\"root_fingerprint\":{},\"certificates\":[{}],\"audit\":[{}]}}",
            q(&self.root.fingerprint),
            certs.join(","),
            audits.join(",")
        )
    }
}

fn q(value: &str) -> String {
    common_api::json_quote(value)
}

fn json_value(text: &str, key: &str) -> Option<String> {
    common_component::json_str(text, key)
}
fn optional_value(text: &str, key: &str) -> Option<String> {
    common_component::json_str(text, key)
}

fn json_str_array(values: &[String]) -> String {
    let parts: Vec<String> = values.iter().map(|value| q(value)).collect();
    format!("[{}]", parts.join(","))
}

/// Parse a flat JSON string array (no nesting needed for SANs).
pub fn parse_str_array(text: &str, key: &str) -> Option<Vec<String>> {
    let needle = format!("\"{}\"", key);
    let at = text.find(&needle)?;
    let rest = &text[at + needle.len()..];
    let colon = rest.find(':')?;
    let after = rest[colon + 1..].trim_start();
    let open = after.find('[')?;
    let close = after.find(']')?;
    if close < open {
        return None;
    }
    let inner = &after[open + 1..close];
    let mut out = Vec::new();
    for piece in inner.split(',') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let value = common_component::json_str(&format!("{{\"x\":{}}}", piece), "x")?;
        out.push(value);
    }
    Some(out)
}

fn parse_audit(line: &str) -> Option<AuditEntry> {
    Some(AuditEntry {
        time: json_value(line, "time")?,
        action: json_value(line, "action")?,
        serial: json_value(line, "serial")?,
        subject: json_value(line, "subject")?,
        detail: json_value(line, "detail")?,
    })
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[((triple >> 18) & 63) as usize] as char);
        out.push(B64[((triple >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[((triple >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64[(triple & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for ch in text.bytes() {
        if ch == b'=' {
            break;
        }
        let value = B64.iter().position(|item| *item == ch)? as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths(tag: &str) -> (String, String) {
        let dir = std::env::temp_dir().join(format!("lan-ca-test-{}-{}", tag, std::process::id()));
        let _ = fs::create_dir_all(&dir);
        (
            dir.join("root.key").to_string_lossy().into_owned(),
            dir.join("store.json").to_string_lossy().into_owned(),
        )
    }

    #[test]
    fn rfc3339_is_utc_and_parseable() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
        assert!(rfc3339(now_secs()).ends_with('Z'));
    }

    #[test]
    fn issue_verify_tamper_renew_revoke_lifecycle() {
        let (root, store) = temp_paths("lifecycle");
        let _ = fs::remove_file(&root);
        let _ = fs::remove_file(&store);
        let mut ca = CaStore::open(&root, &store, 100).unwrap();
        assert!(!ca.root.fingerprint.is_empty());
        let cert = ca
            .issue(
                "nas.lan",
                vec!["nas.lan".into(), "192.168.1.5".into()],
                90,
                &[],
            )
            .unwrap();
        assert!(ca.verify(&cert));
        let mut tampered = cert.clone();
        tampered.subject = "evil.lan".into();
        assert!(!ca.verify(&tampered));
        let renewed = ca.renew(&cert.serial, 365).unwrap();
        assert_ne!(renewed.signature, cert.signature);
        assert!(ca.verify(&renewed));
        let revoked = ca.revoke(&cert.serial, "key-compromise").unwrap();
        assert_eq!(revoked.status, "revoked");
        assert_eq!(ca.crl().len(), 1);
        assert!(ca.renew(&cert.serial, 30).is_err());
        assert_eq!(ca.audit.len(), 3);
    }

    #[test]
    fn root_key_never_leaks_into_export_or_audit() {
        let (root, store) = temp_paths("leak");
        let _ = fs::remove_file(&root);
        let _ = fs::remove_file(&store);
        let mut ca = CaStore::open(&root, &store, 10).unwrap();
        ca.issue("printer.lan", vec![], 30, &[]).unwrap();
        let key_text = fs::read_to_string(&root).unwrap().trim().to_string();
        let export = ca.export_json();
        assert!(!export.contains(&key_text), "root key leaked into export");
        assert!(!ca.status_json().contains(&key_text));
        let disk = fs::read_to_string(&store).unwrap();
        assert!(!disk.contains(&key_text), "root key leaked into store file");
    }

    #[test]
    fn persistence_roundtrip_keeps_signatures_valid() {
        let (root, store) = temp_paths("persist");
        let _ = fs::remove_file(&root);
        let _ = fs::remove_file(&store);
        {
            let mut ca = CaStore::open(&root, &store, 10).unwrap();
            ca.issue("cam.lan", vec!["cam.lan".into()], 30, &[])
                .unwrap();
        }
        let ca = CaStore::open(&root, &store, 10).unwrap();
        assert_eq!(ca.certificates.len(), 1);
        assert!(ca.verify(&ca.certificates[0]));
        assert_eq!(ca.certificates[0].sans, vec!["cam.lan"]);
        assert_eq!(ca.audit.len(), 1);
    }

    #[test]
    fn subject_validation_and_pem_roundtrip() {
        let (root, store) = temp_paths("pem");
        let _ = fs::remove_file(&root);
        let _ = fs::remove_file(&store);
        let mut ca = CaStore::open(&root, &store, 10).unwrap();
        assert!(ca.issue("*.lan", vec![], 30, &[]).is_err());
        assert!(ca
            .issue("nas.example.com", vec![], 30, &[".lan".into()])
            .is_err());
        let cert = ca.issue("nas.lan", vec![], 30, &[".lan".into()]).unwrap();
        let pem = cert.to_pem();
        assert!(pem.starts_with("-----BEGIN LAN STUDIO CERTIFICATE-----"));
        let body: String = pem
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        let decoded = String::from_utf8(base64_decode(&body).unwrap()).unwrap();
        assert_eq!(
            Certificate::from_json(&decoded).unwrap().serial,
            cert.serial
        );
    }
}
