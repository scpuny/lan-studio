//! Liveness and readiness state for /healthz and /readyz.

use std::sync::atomic::{AtomicBool, Ordering};

pub struct Health {
    ready: AtomicBool,
}

impl Health {
    pub fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
        }
    }

    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::Relaxed);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }
}

impl Default for Health {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_not_ready_then_becomes_ready() {
        let health = Health::new();
        assert!(!health.is_ready());
        health.mark_ready();
        assert!(health.is_ready());
    }
}
