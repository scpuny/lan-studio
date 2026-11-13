//! Structured JSON log records, one object per line (contracts/log-record.json).
//!
//! M2.2 adds two bounded sinks:
//! * `RotatingFile` — size-based rotation with a hard disk upper bound
//!   `max_bytes * (backup_count + 1)` (F-D06).
//! * `LogBuffer` — in-memory ring buffer plus a condition variable, exposed to
//!   the HTTP layer through the `EventSource` trait for SSE (F-D05).
//!
//! No third-party crates: disk writes use `std::fs`, SSE wakeups use `Condvar`.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Read-only event feed consumed by `/api/logs/stream`.
pub trait EventSource: Send + Sync {
    /// Sequence number of the newest record; clients pass it as their cursor.
    fn cursor(&self) -> u64;
    /// Newest `limit` records, oldest first.
    fn recent(&self, limit: usize) -> Vec<String>;
    /// Block up to `timeout` for records newer than `cursor`.
    /// Returns `(new_cursor, records)`; an empty vec means timeout.
    fn poll(&self, cursor: u64, timeout: Duration) -> (u64, Vec<String>);
}

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

/// Size-based rotating log file. The total number of files is bounded by
/// `backup_count + 1`, so disk usage can never exceed `limit_bytes()`.
pub struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    backup_count: u32,
    lock: Mutex<()>,
}

impl RotatingFile {
    pub fn new(
        dir: impl AsRef<Path>,
        name: &str,
        max_bytes: u64,
        backup_count: u32,
    ) -> io::Result<Self> {
        fs::create_dir_all(dir.as_ref())?;
        Ok(Self {
            path: dir.as_ref().join(name),
            max_bytes: max_bytes.max(1),
            backup_count,
            lock: Mutex::new(()),
        })
    }

    pub fn append(&self, line: &str) -> io::Result<()> {
        let _guard = self.lock.lock().unwrap();
        let mut data = line.trim_end_matches(['\r', '\n']).to_string();
        data.push('\n');
        let current = fs::metadata(&self.path).map(|meta| meta.len()).unwrap_or(0);
        if current.saturating_add(data.len() as u64) > self.max_bytes {
            self.rotate()?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(data.as_bytes())
    }

    fn backup_path(&self, index: u32) -> PathBuf {
        PathBuf::from(format!("{}.{}", self.path.display(), index))
    }

    fn rotate(&self) -> io::Result<()> {
        let oldest = self.backup_path(self.backup_count);
        if oldest.exists() {
            fs::remove_file(&oldest)?;
        }
        for index in (1..self.backup_count).rev() {
            let from = self.backup_path(index);
            if from.exists() {
                fs::rename(&from, self.backup_path(index + 1))?;
            }
        }
        if self.path.exists() {
            fs::rename(&self.path, self.backup_path(1))?;
        }
        Ok(())
    }

    /// Sum of the active file and all backups.
    pub fn disk_bytes(&self) -> u64 {
        let mut total = fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        for index in 1..=self.backup_count {
            total += fs::metadata(self.backup_path(index))
                .map(|m| m.len())
                .unwrap_or(0);
        }
        total
    }

    /// Hard upper bound: `max_bytes * (backup_count + 1)`.
    pub fn limit_bytes(&self) -> u64 {
        self.max_bytes.saturating_mul(self.backup_count as u64 + 1)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

struct BufferInner {
    records: VecDeque<(u64, String)>,
    next_seq: u64,
}

/// Bounded in-memory ring buffer + optional rotating disk sink.
pub struct LogBuffer {
    inner: Mutex<BufferInner>,
    changed: Condvar,
    capacity: usize,
    disk: Option<RotatingFile>,
}

impl LogBuffer {
    pub fn new(capacity: usize, disk: Option<RotatingFile>) -> Self {
        Self {
            inner: Mutex::new(BufferInner {
                records: VecDeque::new(),
                next_seq: 1,
            }),
            changed: Condvar::new(),
            capacity: capacity.max(1),
            disk,
        }
    }

    /// Append one already-serialized JSON record. Returns its sequence number.
    pub fn append(&self, line: impl Into<String>) -> u64 {
        let line = line.into();
        let seq = {
            let mut guard = self.inner.lock().unwrap();
            let seq = guard.next_seq;
            guard.next_seq = guard.next_seq.wrapping_add(1).max(1);
            while guard.records.len() >= self.capacity {
                guard.records.pop_front();
            }
            guard.records.push_back((seq, line.clone()));
            seq
        };
        if let Some(disk) = &self.disk {
            let _ = disk.append(&line);
        }
        self.changed.notify_all();
        seq
    }

    /// Convenience: format a structured record and append it.
    pub fn emit(&self, level: &str, source: &str, msg: &str) -> u64 {
        self.append(record(level, source, msg, &[], None))
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn disk_bytes(&self) -> u64 {
        self.disk.as_ref().map_or(0, RotatingFile::disk_bytes)
    }

    pub fn disk_limit_bytes(&self) -> u64 {
        self.disk.as_ref().map_or(0, RotatingFile::limit_bytes)
    }
}

impl EventSource for LogBuffer {
    fn cursor(&self) -> u64 {
        self.inner.lock().unwrap().next_seq.saturating_sub(1)
    }

    fn recent(&self, limit: usize) -> Vec<String> {
        let guard = self.inner.lock().unwrap();
        let take = limit.clamp(1, self.capacity);
        let skip = guard.records.len().saturating_sub(take);
        guard
            .records
            .iter()
            .skip(skip)
            .map(|(_, line)| line.clone())
            .collect()
    }

    fn poll(&self, cursor: u64, timeout: Duration) -> (u64, Vec<String>) {
        let deadline = Instant::now() + timeout;
        let mut guard = self.inner.lock().unwrap();
        loop {
            let events: Vec<String> = guard
                .records
                .iter()
                .filter(|(seq, _)| *seq > cursor)
                .map(|(_, line)| line.clone())
                .collect();
            if !events.is_empty() {
                let last = guard.records.back().map_or(cursor, |(seq, _)| *seq);
                return (last, events);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return (cursor, Vec::new());
            }
            let (next_guard, _) = self.changed.wait_timeout(guard, remaining).unwrap();
            guard = next_guard;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lanstudio-log-{}-{}-{}",
            label,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

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

    #[test]
    fn rotation_holds_a_hard_disk_bound() {
        let dir = temp_dir("rotate");
        let log = RotatingFile::new(&dir, "component.log", 256, 2).unwrap();
        for index in 0..60 {
            log.append(&format!(
                "{{\"msg\":\"{}\"}}",
                "x".repeat(40) + &index.to_string()
            ))
            .unwrap();
        }
        assert!(log.disk_bytes() <= log.limit_bytes());
        assert_eq!(log.limit_bytes(), 768);
        let mut files = 0;
        for entry in fs::read_dir(&dir).unwrap() {
            if entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("component.log")
            {
                files += 1;
            }
        }
        assert!(files <= 3, "at most active + 2 backups, got {}", files);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn buffer_is_bounded_and_poll_wakes_on_append() {
        let buffer = LogBuffer::new(3, None);
        assert!(buffer.is_empty());
        buffer.emit("info", "lan-mdns", "one");
        buffer.emit("info", "lan-mdns", "two");
        let cursor = buffer.cursor();
        let (next, events) = buffer.poll(cursor, Duration::from_millis(10));
        assert!(events.is_empty());
        assert_eq!(next, cursor);

        buffer.emit("warning", "lan-mdns", "three");
        let (next, events) = buffer.poll(cursor, Duration::from_secs(1));
        assert_eq!(events.len(), 1);
        assert!(events[0].contains("three"));
        assert_eq!(next, cursor + 1);

        for index in 0..10 {
            buffer.emit("info", "lan-mdns", &format!("line-{}", index));
        }
        assert_eq!(buffer.len(), 3);
        let recent = buffer.recent(2);
        assert_eq!(recent.len(), 2);
        assert!(recent[1].contains("line-9"));
        assert_eq!(buffer.disk_bytes(), 0);
    }

    #[test]
    fn event_source_trait_object_is_usable() {
        let buffer = LogBuffer::new(4, None);
        let source: &dyn EventSource = &buffer;
        buffer.emit("info", "lan-mdns", "hello");
        assert_eq!(source.recent(10).len(), 1);
        assert_eq!(source.cursor(), 1);
    }
}
