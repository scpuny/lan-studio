//! Read-only reachability probe and a debounced device state machine (M4, PRD 4.7).
//!
//! Zero third-party crates. `TcpProbe` only performs a TCP connect, it never
//! writes a payload. `Debounce` turns flapping samples into stable state
//! transitions so a single timeout cannot be reported as "device off".

use std::net::{IpAddr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// Stable device states. `Waking` is held during the post-wake grace window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceState {
    Unknown,
    Waking,
    Online,
    Offline,
}

impl DeviceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Waking => "waking",
            Self::Online => "online",
            Self::Offline => "offline",
        }
    }
}

/// Connect-only probe. The socket is dropped immediately after connect.
pub struct TcpProbe {
    pub timeout: Duration,
}

impl TcpProbe {
    pub fn probe(&self, ip: IpAddr, port: u16) -> bool {
        // READ-ONLY: there is no `write` call anywhere in this crate.
        TcpStream::connect_timeout(&SocketAddr::new(ip, port), self.timeout).is_ok()
    }
}

/// Consecutive-sample debouncer with a post-wake grace window.
pub struct Debounce {
    pub success_threshold: u32,
    pub failure_threshold: u32,
    state: DeviceState,
    grace_until: Option<Instant>,
    streak_ok: u32,
    streak_fail: u32,
}

impl Debounce {
    pub fn new(success_threshold: u32, failure_threshold: u32) -> Self {
        Self {
            success_threshold: success_threshold.max(1),
            failure_threshold: failure_threshold.max(1),
            state: DeviceState::Unknown,
            grace_until: None,
            streak_ok: 0,
            streak_fail: 0,
        }
    }

    pub fn state(&self) -> DeviceState {
        self.state
    }

    /// Called right after a wake packet is accepted. Offline transitions are
    /// suppressed until `grace` elapses, so a slow-booting host is not
    /// misreported as unreachable.
    pub fn mark_waking(&mut self, grace: Duration, now: Instant) {
        self.state = DeviceState::Waking;
        self.grace_until = Some(now + grace);
        self.streak_ok = 0;
        self.streak_fail = 0;
    }

    /// Feed one sample. Returns `Some(new_state)` only on an actual transition.
    pub fn observe(&mut self, reachable: bool, now: Instant) -> Option<DeviceState> {
        let in_grace = self.grace_until.is_some_and(|until| now < until);
        if !in_grace {
            self.grace_until = None;
        }
        if in_grace && !reachable {
            // Never accumulate failures inside the grace window: otherwise the
            // first sample after the window would immediately look like a long
            // streak of failures and misreport a slow-booting host as offline.
            self.streak_ok = 0;
            self.streak_fail = 0;
            return None;
        }
        if reachable {
            self.streak_ok += 1;
            self.streak_fail = 0;
        } else {
            self.streak_fail += 1;
            self.streak_ok = 0;
        }
        let next = if self.streak_ok >= self.success_threshold {
            Some(DeviceState::Online)
        } else if self.streak_fail >= self.failure_threshold {
            Some(DeviceState::Offline)
        } else {
            None
        };
        if next == Some(DeviceState::Online) {
            self.grace_until = None; // the host is up; grace is no longer needed
        }
        match next {
            Some(state) if state != self.state => {
                self.state = state;
                Some(state)
            }
            _ => None,
        }
    }

    /// Direct state override for administrative actions (e.g. manual probe).
    pub fn reset(&mut self) {
        self.state = DeviceState::Unknown;
        self.grace_until = None;
        self.streak_ok = 0;
        self.streak_fail = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    #[test]
    fn tcp_probe_never_writes_payload() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let probe = TcpProbe {
            timeout: Duration::from_millis(300),
        };
        assert!(probe.probe(IpAddr::from([127, 0, 0, 1]), port));
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        let mut buf = [0u8; 16];
        let received = stream.read(&mut buf).unwrap_or(0);
        assert_eq!(received, 0, "probe must never send a payload");
    }

    #[test]
    fn debounce_requires_consecutive_samples() {
        let mut d = Debounce::new(3, 3);
        let now = Instant::now();
        assert_eq!(d.observe(true, now), None);
        assert_eq!(
            d.observe(false, now),
            None,
            "a single failure must not flap"
        );
        assert_eq!(d.observe(true, now), None);
        assert_eq!(d.observe(false, now), None);
        assert_eq!(d.observe(true, now), None);
        assert_eq!(d.observe(true, now), None);
        assert_eq!(d.observe(true, now), Some(DeviceState::Online));
        assert_eq!(d.state(), DeviceState::Online);
    }

    #[test]
    fn grace_period_blocks_offline_transition() {
        let mut d = Debounce::new(3, 3);
        let now = Instant::now();
        d.mark_waking(Duration::from_secs(90), now);
        assert_eq!(d.state(), DeviceState::Waking);
        for _ in 0..10 {
            assert_eq!(d.observe(false, now), None);
        }
        assert_eq!(d.state(), DeviceState::Waking);
        let later = now + Duration::from_secs(91);
        assert_eq!(d.observe(false, later), None);
        assert_eq!(d.observe(false, later), None);
        assert_eq!(d.observe(false, later), Some(DeviceState::Offline));
    }

    #[test]
    fn state_names_are_stable() {
        assert_eq!(DeviceState::Unknown.as_str(), "unknown");
        assert_eq!(DeviceState::Online.as_str(), "online");
        assert_eq!(DeviceState::Offline.as_str(), "offline");
        assert_eq!(DeviceState::Waking.as_str(), "waking");
    }
}
