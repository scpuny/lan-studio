//! DNS / mDNS wire-format codec (RFC 1035 + RFC 6762 subset).
//!
//! Zero third-party crates. Handles name compression pointers, the five record
//! types used by `lan-mdns` (A / AAAA / PTR / SRV / TXT), the QU and
//! cache-flush bits, and RFC 6762 Known-Answer suppression.

use std::collections::HashMap;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

pub const TYPE_A: u16 = 1;
pub const TYPE_PTR: u16 = 12;
pub const TYPE_TXT: u16 = 16;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_SRV: u16 = 33;
pub const TYPE_ANY: u16 = 255;

pub const CLASS_IN: u16 = 1;
pub const CLASS_ANY: u16 = 255;
pub const QU_BIT: u16 = 0x8000;
pub const CACHE_FLUSH_BIT: u16 = 0x8000;
pub const FLAG_QR: u16 = 0x8000;
pub const FLAG_AA: u16 = 0x0400;

const MAX_POINTER_JUMPS: usize = 32;
const MAX_NAME_LEN: usize = 255;
const MAX_SECTION_ENTRIES: usize = 256;

pub type Result<T> = std::result::Result<T, DnsError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsError {
    Truncated,
    BadLabel,
    BadPointer,
    TooManyPointers,
    InvalidUtf8,
    NameTooLong,
    InvalidName(String),
}

impl fmt::Display for DnsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Truncated => "truncated DNS message".to_string(),
            Self::BadLabel => "invalid label length".to_string(),
            Self::BadPointer => "compression pointer out of range".to_string(),
            Self::TooManyPointers => "too many compression pointers".to_string(),
            Self::InvalidUtf8 => "name is not valid UTF-8".to_string(),
            Self::NameTooLong => "name exceeds 255 bytes".to_string(),
            Self::InvalidName(reason) => format!("invalid DNS name: {}", reason),
        };
        f.write_str(&text)
    }
}

impl std::error::Error for DnsError {}

fn normalize(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

/// Compare two DNS names case-insensitively, ignoring a trailing dot.
pub fn name_eq(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

/// True for names in the `.local` mDNS namespace.
pub fn is_local(name: &str) -> bool {
    let n = normalize(name);
    n == "local" || n.ends_with(".local")
}

/// Validate a DNS name without touching the network (F-A04).
pub fn validate_name(name: &str) -> Result<()> {
    let n = normalize(name);
    if n.is_empty() {
        return Err(DnsError::InvalidName("empty name".into()));
    }
    if n.len() + 1 > MAX_NAME_LEN {
        return Err(DnsError::NameTooLong);
    }
    let labels: Vec<&str> = n.split('.').collect();
    if labels.len() < 2 {
        return Err(DnsError::InvalidName("need at least two labels".into()));
    }
    for label in labels {
        if label.is_empty() || label.len() > 63 {
            return Err(DnsError::InvalidName("label length must be 1..63".into()));
        }
        let bytes = label.as_bytes();
        if bytes[0] == b'-' || bytes[bytes.len() - 1] == b'-' {
            return Err(DnsError::InvalidName(
                "label must not start or end with '-'".into(),
            ));
        }
    }
    Ok(())
}

pub fn reverse_ptr_name(ip: Ipv4Addr) -> String {
    let o = ip.octets();
    format!("{}.{}.{}.{}.in-addr.arpa", o[3], o[2], o[1], o[0])
}

pub fn type_name(rtype: u16) -> &'static str {
    match rtype {
        TYPE_A => "A",
        TYPE_AAAA => "AAAA",
        TYPE_PTR => "PTR",
        TYPE_SRV => "SRV",
        TYPE_TXT => "TXT",
        TYPE_ANY => "ANY",
        other => {
            let _ = other;
            "UNKNOWN"
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RData {
    A(Ipv4Addr),
    Aaaa(Ipv6Addr),
    Ptr(String),
    Srv {
        priority: u16,
        weight: u16,
        port: u16,
        target: String,
    },
    Txt(Vec<String>),
    Raw(u16, Vec<u8>),
}

impl RData {
    pub fn rtype(&self) -> u16 {
        match self {
            Self::A(_) => TYPE_A,
            Self::Aaaa(_) => TYPE_AAAA,
            Self::Ptr(_) => TYPE_PTR,
            Self::Srv { .. } => TYPE_SRV,
            Self::Txt(_) => TYPE_TXT,
            Self::Raw(rtype, _) => *rtype,
        }
    }

    /// DNS names are case-insensitive; normalize embedded names too so an
    /// encode/decode roundtrip is stable (F-C05).
    fn normalized(self) -> Self {
        match self {
            Self::Ptr(name) => Self::Ptr(normalize(&name)),
            Self::Srv {
                priority,
                weight,
                port,
                target,
            } => Self::Srv {
                priority,
                weight,
                port,
                target: normalize(&target),
            },
            other => other,
        }
    }

    fn encode(&self, out: &mut Vec<u8>, names: &mut HashMap<String, u16>) {
        match self {
            Self::A(ip) => out.extend_from_slice(&ip.octets()),
            Self::Aaaa(ip) => out.extend_from_slice(&ip.octets()),
            Self::Ptr(name) => encode_name(out, name, names),
            Self::Srv {
                priority,
                weight,
                port,
                target,
            } => {
                out.extend_from_slice(&priority.to_be_bytes());
                out.extend_from_slice(&weight.to_be_bytes());
                out.extend_from_slice(&port.to_be_bytes());
                encode_name(out, target, names);
            }
            Self::Txt(parts) => {
                for part in parts {
                    let bytes = part.as_bytes();
                    let len = bytes.len().min(255);
                    out.push(len as u8);
                    out.extend_from_slice(&bytes[..len]);
                }
            }
            Self::Raw(_, bytes) => out.extend_from_slice(bytes),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRecord {
    pub name: String,
    pub rtype: u16,
    pub rclass: u16,
    pub ttl: u32,
    pub rdata: RData,
}

impl ResourceRecord {
    pub fn new(name: &str, rdata: RData, ttl: u32, cache_flush: bool) -> Self {
        let rclass = if cache_flush {
            CLASS_IN | CACHE_FLUSH_BIT
        } else {
            CLASS_IN
        };
        let rdata = rdata.normalized();
        Self {
            name: normalize(name),
            rtype: rdata.rtype(),
            rclass,
            ttl,
            rdata,
        }
    }

    pub fn class(&self) -> u16 {
        self.rclass & !CACHE_FLUSH_BIT
    }

    pub fn cache_flush(&self) -> bool {
        self.rclass & CACHE_FLUSH_BIT != 0
    }

    /// A Goodbye is the same record with TTL 0 (RFC 6762 10.1).
    pub fn goodbye(&self) -> Self {
        let mut rr = self.clone();
        rr.ttl = 0;
        rr
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub name: String,
    pub qtype: u16,
    pub qclass: u16,
}

impl Question {
    pub fn new(name: &str, qtype: u16) -> Self {
        Self {
            name: normalize(name),
            qtype,
            qclass: CLASS_IN,
        }
    }

    /// QU bit: request a unicast reply (RFC 6762 5.4).
    pub fn with_qu(mut self) -> Self {
        self.qclass |= QU_BIT;
        self
    }

    pub fn class(&self) -> u16 {
        self.qclass & !QU_BIT
    }

    pub fn wants_unicast(&self) -> bool {
        self.qclass & QU_BIT != 0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    pub id: u16,
    pub flags: u16,
    pub questions: Vec<Question>,
    pub answers: Vec<ResourceRecord>,
    pub authorities: Vec<ResourceRecord>,
    pub additionals: Vec<ResourceRecord>,
}

impl Message {
    pub fn query(id: u16, questions: Vec<Question>) -> Self {
        Self {
            id,
            flags: 0,
            questions,
            ..Self::default()
        }
    }

    pub fn response(id: u16) -> Self {
        Self {
            id,
            flags: FLAG_QR | FLAG_AA,
            ..Self::default()
        }
    }

    pub fn is_response(&self) -> bool {
        self.flags & FLAG_QR != 0
    }

    pub fn is_query(&self) -> bool {
        !self.is_response()
    }

    pub fn encode(&self) -> Vec<u8> {
        encode(self)
    }

    pub fn decode(data: &[u8]) -> Result<Self> {
        decode(data)
    }
}

/// Whether an RR answers a question (type, class, and name).
pub fn question_matches(rr: &ResourceRecord, q: &Question) -> bool {
    let class = q.class();
    let type_ok = q.qtype == TYPE_ANY || q.qtype == rr.rtype;
    let class_ok = class == CLASS_IN || class == CLASS_ANY;
    type_ok && class_ok && rr.class() == CLASS_IN && name_eq(&rr.name, &q.name)
}

/// RFC 6762 7.1 Known-Answer suppression: a record is suppressed when the
/// querier already holds it with a remaining TTL >= half of ours.
pub fn suppressed(rr: &ResourceRecord, known_answers: &[ResourceRecord]) -> bool {
    known_answers.iter().any(|known| {
        known.rtype == rr.rtype
            && name_eq(&known.name, &rr.name)
            && known.rdata == rr.rdata
            && known.ttl >= rr.ttl / 2
    })
}

fn encode_name(out: &mut Vec<u8>, name: &str, names: &mut HashMap<String, u16>) {
    let normalized = normalize(name);
    let mut rest = normalized.as_str();
    loop {
        if rest.is_empty() {
            break;
        }
        if let Some(&offset) = names.get(rest) {
            out.push(0xC0 | ((offset >> 8) as u8 & 0x3F));
            out.push(offset as u8);
            return;
        }
        if out.len() < 0x3FFF {
            names.insert(rest.to_string(), out.len() as u16);
        }
        let (label, tail) = match rest.split_once('.') {
            Some(parts) => parts,
            None => (rest, ""),
        };
        if label.is_empty() {
            break;
        }
        let bytes = label.as_bytes();
        let len = bytes.len().min(63);
        out.push(len as u8);
        out.extend_from_slice(&bytes[..len]);
        if tail.is_empty() {
            break;
        }
        rest = tail;
    }
    out.push(0);
}

fn read_u16(data: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([data[pos], data[pos + 1]])
}

fn read_u32(data: &[u8], pos: usize) -> u32 {
    u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
}

fn decode_name(data: &[u8], start: usize) -> Result<(String, usize)> {
    let mut pos = start;
    let mut labels: Vec<String> = Vec::new();
    let mut end: Option<usize> = None;
    let mut jumps = 0usize;
    loop {
        if pos >= data.len() {
            return Err(DnsError::Truncated);
        }
        let len = data[pos];
        if len & 0xC0 == 0xC0 {
            if pos + 1 >= data.len() {
                return Err(DnsError::Truncated);
            }
            let ptr = (((len as usize) & 0x3F) << 8) | data[pos + 1] as usize;
            if ptr >= data.len() {
                return Err(DnsError::BadPointer);
            }
            jumps += 1;
            if jumps > MAX_POINTER_JUMPS {
                return Err(DnsError::TooManyPointers);
            }
            if end.is_none() {
                end = Some(pos + 2);
            }
            pos = ptr;
            continue;
        }
        if len & 0xC0 != 0 {
            return Err(DnsError::BadLabel);
        }
        if len == 0 {
            if end.is_none() {
                end = Some(pos + 1);
            }
            break;
        }
        let label_len = len as usize;
        if pos + 1 + label_len > data.len() {
            return Err(DnsError::Truncated);
        }
        let text = std::str::from_utf8(&data[pos + 1..pos + 1 + label_len])
            .map_err(|_| DnsError::InvalidUtf8)?;
        labels.push(text.to_ascii_lowercase());
        pos += 1 + label_len;
        let used: usize = labels.iter().map(|label| label.len() + 1).sum();
        if used + 1 > MAX_NAME_LEN {
            return Err(DnsError::NameTooLong);
        }
    }
    Ok((labels.join("."), end.unwrap_or(pos + 1)))
}

fn decode_rdata(data: &[u8], start: usize, end: usize, rtype: u16) -> Result<RData> {
    let slice = &data[start..end];
    match rtype {
        TYPE_A if slice.len() == 4 => Ok(RData::A(Ipv4Addr::new(
            slice[0], slice[1], slice[2], slice[3],
        ))),
        TYPE_A => Err(DnsError::Truncated),
        TYPE_AAAA if slice.len() == 16 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(slice);
            Ok(RData::Aaaa(Ipv6Addr::from(octets)))
        }
        TYPE_AAAA => Err(DnsError::Truncated),
        TYPE_PTR => {
            let (name, _) = decode_name(data, start)?;
            Ok(RData::Ptr(name))
        }
        TYPE_SRV if slice.len() >= 6 => {
            let priority = read_u16(data, start);
            let weight = read_u16(data, start + 2);
            let port = read_u16(data, start + 4);
            let (target, _) = decode_name(data, start + 6)?;
            Ok(RData::Srv {
                priority,
                weight,
                port,
                target,
            })
        }
        TYPE_SRV => Err(DnsError::Truncated),
        TYPE_TXT => {
            let mut parts = Vec::new();
            let mut pos = start;
            while pos < end {
                let len = data[pos] as usize;
                pos += 1;
                if pos + len > end {
                    return Err(DnsError::Truncated);
                }
                parts.push(String::from_utf8_lossy(&data[pos..pos + len]).into_owned());
                pos += len;
            }
            Ok(RData::Txt(parts))
        }
        other => Ok(RData::Raw(other, slice.to_vec())),
    }
}

fn decode_rrs(data: &[u8], pos: &mut usize, count: usize) -> Result<Vec<ResourceRecord>> {
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let (name, next) = decode_name(data, *pos)?;
        *pos = next;
        if *pos + 10 > data.len() {
            return Err(DnsError::Truncated);
        }
        let rtype = read_u16(data, *pos);
        let rclass = read_u16(data, *pos + 2);
        let ttl = read_u32(data, *pos + 4);
        let rdlen = read_u16(data, *pos + 8) as usize;
        let rd_start = *pos + 10;
        if rd_start + rdlen > data.len() {
            return Err(DnsError::Truncated);
        }
        let rd_end = rd_start + rdlen;
        let rdata = decode_rdata(data, rd_start, rd_end, rtype)?;
        *pos = rd_end;
        out.push(ResourceRecord {
            name,
            rtype,
            rclass,
            ttl,
            rdata,
        });
    }
    Ok(out)
}

pub fn encode(msg: &Message) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&msg.id.to_be_bytes());
    out.extend_from_slice(&msg.flags.to_be_bytes());
    out.extend_from_slice(&(msg.questions.len() as u16).to_be_bytes());
    out.extend_from_slice(&(msg.answers.len() as u16).to_be_bytes());
    out.extend_from_slice(&(msg.authorities.len() as u16).to_be_bytes());
    out.extend_from_slice(&(msg.additionals.len() as u16).to_be_bytes());
    let mut names: HashMap<String, u16> = HashMap::new();
    for question in &msg.questions {
        encode_name(&mut out, &question.name, &mut names);
        out.extend_from_slice(&question.qtype.to_be_bytes());
        out.extend_from_slice(&question.qclass.to_be_bytes());
    }
    for rr in msg
        .answers
        .iter()
        .chain(&msg.authorities)
        .chain(&msg.additionals)
    {
        encode_name(&mut out, &rr.name, &mut names);
        out.extend_from_slice(&rr.rtype.to_be_bytes());
        out.extend_from_slice(&rr.rclass.to_be_bytes());
        out.extend_from_slice(&rr.ttl.to_be_bytes());
        let rd_start = out.len();
        out.extend_from_slice(&0u16.to_be_bytes());
        rr.rdata.encode(&mut out, &mut names);
        let rdlen = (out.len() - rd_start - 2) as u16;
        out[rd_start..rd_start + 2].copy_from_slice(&rdlen.to_be_bytes());
    }
    out
}

pub fn decode(data: &[u8]) -> Result<Message> {
    if data.len() < 12 {
        return Err(DnsError::Truncated);
    }
    let id = read_u16(data, 0);
    let flags = read_u16(data, 2);
    let qd = (read_u16(data, 4) as usize).min(MAX_SECTION_ENTRIES);
    let an = (read_u16(data, 6) as usize).min(MAX_SECTION_ENTRIES);
    let ns = (read_u16(data, 8) as usize).min(MAX_SECTION_ENTRIES);
    let ar = (read_u16(data, 10) as usize).min(MAX_SECTION_ENTRIES);
    let mut pos = 12usize;
    let mut questions = Vec::with_capacity(qd);
    for _ in 0..qd {
        let (name, next) = decode_name(data, pos)?;
        pos = next;
        if pos + 4 > data.len() {
            return Err(DnsError::Truncated);
        }
        let qtype = read_u16(data, pos);
        let qclass = read_u16(data, pos + 2);
        pos += 4;
        questions.push(Question {
            name,
            qtype,
            qclass,
        });
    }
    let answers = decode_rrs(data, &mut pos, an)?;
    let authorities = decode_rrs(data, &mut pos, ns)?;
    let additionals = decode_rrs(data, &mut pos, ar)?;
    Ok(Message {
        id,
        flags,
        questions,
        answers,
        authorities,
        additionals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_record(name: &str, ip: [u8; 4], ttl: u32) -> ResourceRecord {
        ResourceRecord::new(name, RData::A(Ipv4Addr::from(ip)), ttl, true)
    }

    #[test]
    fn query_roundtrip_preserves_name_and_type() {
        let msg = Message::query(
            0x1234,
            vec![
                Question::new("NAS.Local.", TYPE_A),
                Question::new("_http._tcp.local", TYPE_PTR).with_qu(),
            ],
        );
        let bytes = msg.encode();
        let decoded = Message::decode(&bytes).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(decoded.questions[0].name, "nas.local");
        assert!(decoded.questions[1].wants_unicast());
        assert_eq!(decoded.questions[1].class(), CLASS_IN);
        assert!(decoded.is_query());
    }

    #[test]
    fn compression_reuses_shared_suffix() {
        let msg = Message {
            id: 1,
            flags: FLAG_QR | FLAG_AA,
            answers: vec![
                a_record("nas.local", [192, 168, 5, 10], 120),
                a_record("printer.local", [192, 168, 5, 11], 120),
                a_record("nas.local", [192, 168, 5, 10], 120),
            ],
            ..Message::default()
        };
        let bytes = msg.encode();
        let decoded = Message::decode(&bytes).unwrap();
        assert_eq!(decoded.answers.len(), 3);
        assert_eq!(decoded.answers[0].name, "nas.local");
        assert_eq!(decoded.answers[1].name, "printer.local");
        assert_eq!(decoded.answers[2].name, "nas.local");

        // Repeating the exact same name collapses to a 2-byte pointer, so the
        // second RR costs only 10 (fixed header) + 4 (A rdata) + 2 (name).
        let one = Message {
            answers: vec![a_record("nas.local", [10, 0, 0, 1], 120)],
            ..Message::default()
        };
        let two = Message {
            answers: vec![
                a_record("nas.local", [10, 0, 0, 1], 120),
                a_record("nas.local", [10, 0, 0, 1], 120),
            ],
            ..Message::default()
        };
        assert_eq!(two.encode().len() - one.encode().len(), 16);
    }

    #[test]
    fn all_five_record_types_roundtrip() {
        let msg = Message {
            id: 7,
            flags: FLAG_QR | FLAG_AA,
            answers: vec![
                a_record("nas.local", [10, 0, 0, 1], 120),
                ResourceRecord::new(
                    "nas.local",
                    RData::Aaaa("fe80::1".parse().unwrap()),
                    120,
                    false,
                ),
                ResourceRecord::new(
                    "_http._tcp.local",
                    RData::Ptr("Web Server._http._tcp.local".into()),
                    4500,
                    false,
                ),
                ResourceRecord::new(
                    "Web Server._http._tcp.local",
                    RData::Srv {
                        priority: 0,
                        weight: 0,
                        port: 8080,
                        target: "nas.local".into(),
                    },
                    120,
                    true,
                ),
                ResourceRecord::new(
                    "Web Server._http._tcp.local",
                    RData::Txt(vec!["path=/".into(), "version=1".into()]),
                    120,
                    true,
                ),
            ],
            ..Message::default()
        };
        let decoded = Message::decode(&msg.encode()).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(decoded.answers[3].rdata, msg.answers[3].rdata);
    }

    #[test]
    fn cache_flush_and_goodbye_bits() {
        let rr = a_record("nas.local", [10, 0, 0, 1], 120);
        assert!(rr.cache_flush());
        assert_eq!(rr.class(), CLASS_IN);
        let bye = rr.goodbye();
        assert_eq!(bye.ttl, 0);
        assert_eq!(bye.name, rr.name);
        let plain = ResourceRecord::new("nas.local", RData::A(Ipv4Addr::LOCALHOST), 120, false);
        assert!(!plain.cache_flush());
    }

    #[test]
    fn known_answer_suppression_follows_half_ttl_rule() {
        let rr = a_record("nas.local", [10, 0, 0, 1], 120);
        let fresh = a_record("nas.local", [10, 0, 0, 1], 60);
        let stale = a_record("nas.local", [10, 0, 0, 1], 10);
        let other = a_record("nas.local", [10, 0, 0, 2], 60);
        assert!(suppressed(&rr, &[fresh]));
        assert!(!suppressed(&rr, &[stale]));
        assert!(!suppressed(&rr, &[other]));
        assert!(!suppressed(&rr, &[]));
    }

    #[test]
    fn question_matching_respects_type_class_name() {
        let rr = a_record("nas.local", [10, 0, 0, 1], 120);
        assert!(question_matches(&rr, &Question::new("NAS.local", TYPE_A)));
        assert!(question_matches(&rr, &Question::new("nas.local", TYPE_ANY)));
        assert!(!question_matches(
            &rr,
            &Question::new("nas.local", TYPE_AAAA)
        ));
        assert!(!question_matches(
            &rr,
            &Question::new("other.local", TYPE_A)
        ));
    }

    #[test]
    fn malformed_messages_are_rejected() {
        assert_eq!(Message::decode(&[0u8; 8]), Err(DnsError::Truncated));
        let mut bad_pointer = Message::query(1, vec![Question::new("nas.local", TYPE_A)]).encode();
        bad_pointer[12] = 0xC0;
        bad_pointer[13] = 0xFF;
        assert_eq!(Message::decode(&bad_pointer), Err(DnsError::BadPointer));
        let mut bad_label = Message::query(1, vec![Question::new("nas.local", TYPE_A)]).encode();
        bad_label[12] = 0x80;
        assert_eq!(Message::decode(&bad_label), Err(DnsError::BadLabel));
    }

    #[test]
    fn name_validation_and_reverse_ptr() {
        assert!(validate_name("nas.local").is_ok());
        assert!(validate_name("eam.apollo.local").is_ok());
        assert!(validate_name("_http._tcp.local").is_ok());
        assert!(validate_name("bad").is_err());
        assert!(validate_name("-bad.local").is_err());
        assert!(validate_name("bad..local").is_err());
        assert!(is_local("nas.LOCAL"));
        assert!(!is_local("nas.example.com"));
        assert_eq!(
            reverse_ptr_name(Ipv4Addr::new(172, 15, 103, 30)),
            "30.103.15.172.in-addr.arpa"
        );
    }

    #[test]
    fn unknown_record_type_is_preserved_as_raw() {
        let rr = ResourceRecord::new("nas.local", RData::Raw(99, vec![1, 2, 3, 4]), 30, false);
        let msg = Message {
            answers: vec![rr.clone()],
            ..Message::default()
        };
        let decoded = Message::decode(&msg.encode()).unwrap();
        assert_eq!(decoded.answers[0].rdata, RData::Raw(99, vec![1, 2, 3, 4]));
        assert_eq!(decoded.answers[0].rtype, 99);
    }

    #[test]
    fn response_flags_and_goodbye_encode() {
        let mut msg = Message::response(0);
        msg.answers
            .push(a_record("nas.local", [10, 0, 0, 1], 120).goodbye());
        let decoded = Message::decode(&msg.encode()).unwrap();
        assert!(decoded.is_response());
        assert!(!decoded.is_query());
        assert_eq!(decoded.answers[0].ttl, 0);
    }
}
