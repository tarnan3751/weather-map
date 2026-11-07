//! A shared "this upstream told us to back off" switch.
//!
//! Several parts of the server call the same third-party host (alerts, zone
//! boundaries and point forecasts all go to api.weather.gov). When that host
//! rate-limits or blocks one of them, the block applies to our address, not to
//! the caller, so all of them must stand down together. Each caller checks the
//! gate before a request and pauses it when the upstream says to.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Cheap to clone; clones share the same pause.
#[derive(Debug, Clone, Default)]
pub struct UpstreamGate {
    until: Arc<RwLock<Option<Instant>>>,
}

impl UpstreamGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Time left on the current pause, or `None` when requests may go out.
    pub fn paused_for(&self) -> Option<Duration> {
        let until = (*self.until.read().unwrap())?;
        let left = until.saturating_duration_since(Instant::now());
        (!left.is_zero()).then_some(left)
    }

    /// Ask every user of this gate to hold off for at least `duration`. An
    /// existing longer pause is kept.
    pub fn pause(&self, duration: Duration) {
        let new_until = Instant::now() + duration;
        let mut until = self.until.write().unwrap();
        if until.is_none_or(|current| current < new_until) {
            *until = Some(new_until);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_a_pause_and_it_only_grows() {
        let gate = UpstreamGate::new();
        assert!(gate.paused_for().is_none());
        let other = gate.clone();
        other.pause(Duration::from_secs(30));
        let left = gate.paused_for().unwrap();
        assert!(left > Duration::from_secs(29) && left <= Duration::from_secs(30));
        gate.pause(Duration::from_secs(5));
        assert!(other.paused_for().unwrap() > Duration::from_secs(29));
        gate.pause(Duration::from_millis(0));
        assert!(gate.paused_for().is_some());
    }

    #[test]
    fn a_pause_runs_out() {
        let gate = UpstreamGate::new();
        gate.pause(Duration::from_millis(20));
        std::thread::sleep(Duration::from_millis(40));
        assert!(gate.paused_for().is_none());
    }
}
