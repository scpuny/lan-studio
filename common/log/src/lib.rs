//! Structured JSON log records, one object per line.
//! Shape is defined by contracts/log-record.json.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

pub fn record(
    level: &str,
    source: &str,
    msg: &str,
    extra: &[(&str, String)],
    request_id: Option<&str>,
) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let mut parts = vec![
        format!("\"ts\":{:.3}", ts),
        format!("\"level\":\"{}\"", escape(level)),
        format!("\"source\":\"{}\"", escape(source)),
        format!("\"msg\":\"{}\"", escape(msg)),
    ];
    let mut extra_obj = String::from("{");
    for (index, (key, value)) in extra.iter().enumerate() {
        if index > 0 {
            extra_obj.push(',');
        }
        extra_obj.push_str(&format!("\"{}\":\"{}\"", escape(key), escape(value)));
    }
    extra_obj.push('}');
    parts.push(format!("\"extra\":{}", extra_obj));
    if let Some(rid) = request_id {
        parts.push(format!("\"request_id\":\"{}\"", escape(rid)));
    }
    format!("{{{}}}", parts.join(","))
}

pub fn info(source: &str, msg: &str) -> String {
    record("info", source, msg, &[], None)
}

pub fn warning(source: &str, msg: &str) -> String {
    record("warning", source, msg, &[], None)
}

pub fn error(source: &str, msg: &str) -> String {
    record("error", source, msg, &[], None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_has_all_required_fields() {
        let line = record(
            "info",
            "lan-mdns",
            "hello",
            &[("k", "v".to_string())],
            Some("r1"),
        );
        assert!(line.starts_with('{') && line.ends_with('}'));
        assert!(line.contains("\"ts\":"));
        assert!(line.contains("\"level\":\"info\""));
        assert!(line.contains("\"source\":\"lan-mdns\""));
        assert!(line.contains("\"msg\":\"hello\""));
        assert!(line.contains("\"extra\":{\"k\":\"v\"}"));
        assert!(line.contains("\"request_id\":\"r1\""));
    }

    #[test]
    fn escapes_quotes_and_newlines() {
        assert_eq!(escape("a\"b\nc"), "a\\\"b\\nc");
    }
}
