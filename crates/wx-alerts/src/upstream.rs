//! Standing down when api.weather.gov pushes back.
//!
//! A 403 from its CDN is a block on our address and a 429 a rate limit, and
//! both apply to every caller on this host (the point-forecast code in
//! `wx-obs` included), so they pause the shared [`UpstreamGate`]. Nothing is
//! sent to api.weather.gov while the gate is paused.

use std::sync::Mutex;
use std::time::{Duration, Instant};
use wx_core::UpstreamGate;

/// First pause after a 403; doubles with each further block, up to the max.
const BLOCKED_PAUSE: Duration = Duration::from_secs(60);
const MAX_BLOCKED_PAUSE: Duration = Duration::from_secs(15 * 60);
/// Longest `Retry-After` taken at its word.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(15 * 60);

pub(crate) struct NwsLimits {
    gate: UpstreamGate,
    /// Pause after a 429 that gave no Retry-After (the API says its limit
    /// clears within about five seconds).
    rate_limited_pause: Duration,
    state: Mutex<BlockState>,
}

#[derive(Default)]
struct BlockState {
    strikes: u32,
    /// End of the pause the current block started. A 403 that arrives
    /// before then belongs to the same burst (requests already in flight).
    blocked_until: Option<Instant>,
}

impl NwsLimits {
    pub fn new(gate: UpstreamGate, rate_limited_pause: Duration) -> Self {
        Self {
            gate,
            rate_limited_pause,
            state: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BlockState> {
        crate::lock(&self.state)
    }

    pub fn paused_for(&self) -> Option<Duration> {
        self.gate.paused_for()
    }

    /// Wait until requests may go out again.
    pub async fn wait_open(&self) {
        while let Some(left) = self.gate.paused_for() {
            tokio::time::sleep(left).await;
        }
    }

    /// The CDN refused us (HTTP 403). Returns how long everyone pauses.
    pub fn refused(&self, retry_after: Option<Duration>) -> Duration {
        let now = Instant::now();
        let pause = {
            let mut s = self.lock();
            match s.blocked_until {
                // Same burst: one strike, however many requests saw it.
                Some(until) if until > now => until - now,
                _ => {
                    s.strikes = s.strikes.saturating_add(1);
                    let pause = retry_after
                        .map(|r| r.min(MAX_RETRY_AFTER))
                        .unwrap_or_else(|| {
                            (BLOCKED_PAUSE * 2u32.saturating_pow(s.strikes - 1))
                                .min(MAX_BLOCKED_PAUSE)
                        });
                    s.blocked_until = Some(now + pause);
                    pause
                }
            }
        };
        self.gate.pause(pause);
        pause
    }

    /// HTTP 429. Returns how long everyone pauses.
    pub fn rate_limited(&self, retry_after: Option<Duration>) -> Duration {
        let pause = retry_after.map_or(self.rate_limited_pause, |r| r.min(MAX_RETRY_AFTER));
        self.gate.pause(pause);
        pause
    }

    /// A normal answer: the block, if any, is over.
    pub fn ok(&self) {
        let mut s = self.lock();
        if s.blocked_until.is_none_or(|until| until <= Instant::now()) {
            s.strikes = 0;
            s.blocked_until = None;
        }
    }
}

/// The `Retry-After` of an answer, when given in seconds.
pub(crate) fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let secs: u64 = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_burst_of_refusals_is_one_strike() {
        let gate = UpstreamGate::new();
        let limits = NwsLimits::new(gate.clone(), Duration::from_secs(5));
        assert_eq!(limits.refused(None), BLOCKED_PAUSE);
        // Four more requests that were already in flight come back refused.
        for _ in 0..4 {
            assert!(limits.refused(None) <= BLOCKED_PAUSE);
        }
        assert_eq!(limits.lock().strikes, 1);
        assert!(gate.paused_for().unwrap() > BLOCKED_PAUSE - Duration::from_secs(1));

        // The pause runs out and the next request is refused again.
        limits.lock().blocked_until = Some(Instant::now() - Duration::from_millis(1));
        assert_eq!(limits.refused(None), BLOCKED_PAUSE * 2);
        assert_eq!(limits.lock().strikes, 2);

        // An answer after the block resets the escalation.
        limits.lock().blocked_until = Some(Instant::now() - Duration::from_millis(1));
        limits.ok();
        assert_eq!(limits.refused(None), BLOCKED_PAUSE);
    }

    #[test]
    fn retry_after_is_honoured_and_shared() {
        let gate = UpstreamGate::new();
        let limits = NwsLimits::new(gate.clone(), Duration::from_secs(5));
        assert_eq!(
            limits.refused(Some(Duration::from_secs(90))),
            Duration::from_secs(90)
        );
        let other = UpstreamGate::new();
        let limits = NwsLimits::new(other.clone(), Duration::from_secs(5));
        assert_eq!(
            limits.rate_limited(Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        let left = other.paused_for().unwrap();
        assert!(left > Duration::from_secs(6) && left <= Duration::from_secs(7));
        assert_eq!(limits.rate_limited(None), Duration::from_secs(5));
        assert_eq!(
            limits.rate_limited(Some(Duration::from_secs(86_400))),
            MAX_RETRY_AFTER
        );
    }
}
