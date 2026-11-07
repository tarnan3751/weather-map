//! Feed supervision: every data feed is a long-running task that is restarted
//! with exponential backoff when it fails, and that reports its health to a
//! registry the status page reads.
//!
//! A feed is written as a plain loop that returns `Err` (or panics) when
//! something goes wrong:
//!
//! ```ignore
//! spawn_feed("mrms", Duration::from_secs(120), &health, cancel, Backoff::default(), move |ctx| {
//!     let deps = deps.clone();
//!     async move {
//!         loop {
//!             let newest = poll_once(&deps).await?;      // any error restarts the feed
//!             ctx.ok(Some(newest), "radar frame stored"); // marks the feed healthy
//!             tokio::time::sleep(Duration::from_secs(20)).await;
//!         }
//!     }
//! });
//! ```

use crate::events::{Hub, WsEvent};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedState {
    /// Started (or restarted) and has not completed a cycle yet.
    Starting,
    /// The last cycle succeeded.
    Healthy,
    /// The task failed and is waiting to be restarted.
    BackingOff,
    /// Shut down on purpose.
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedHealth {
    pub name: String,
    pub state: FeedState,
    /// How often the feed is expected to succeed; the status page calls a feed
    /// stale when it has been quiet for several of these.
    pub expected_interval_secs: u64,
    pub started_at: Option<DateTime<Utc>>,
    /// Last successful cycle (not necessarily one that found new data).
    pub last_success: Option<DateTime<Utc>>,
    /// Timestamp of the newest data the feed has delivered, e.g. the valid
    /// time of the latest radar frame.
    pub last_data_time: Option<DateTime<Utc>>,
    /// How old `last_data_time` may get before the feed counts as stale even
    /// while its cycles succeed (a source that publishes nothing new, a model
    /// run that is late). `None`: the feed is judged by its cycles alone.
    pub max_data_age_secs: Option<u64>,
    /// One line about what the feed last did.
    pub detail: String,
    pub last_error: Option<String>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub restarts: u64,
    pub next_retry_at: Option<DateTime<Utc>>,
}

impl FeedHealth {
    fn new(name: &str, expected_interval: Duration) -> Self {
        Self {
            name: name.to_string(),
            state: FeedState::Starting,
            expected_interval_secs: expected_interval.as_secs(),
            started_at: None,
            last_success: None,
            last_data_time: None,
            max_data_age_secs: None,
            detail: String::new(),
            last_error: None,
            last_error_at: None,
            consecutive_failures: 0,
            restarts: 0,
            next_retry_at: None,
        }
    }

    /// True when the feed has not succeeded for three expected intervals (or
    /// has never succeeded and has been running that long), or when it has a
    /// [`FeedHealth::max_data_age_secs`] and its newest data (or, with none
    /// yet, its start) is older than that.
    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        let limit = chrono::Duration::seconds(self.expected_interval_secs.max(30) as i64 * 3);
        let quiet = match self.last_success.or(self.started_at) {
            Some(t) => now - t > limit,
            None => false,
        };
        let old = self.max_data_age_secs.is_some_and(|max| {
            self.last_data_time
                .or(self.started_at)
                .is_some_and(|t| now - t > chrono::Duration::seconds(max as i64))
        });
        quiet || old
    }
}

/// Shared table of feed health. Cheap to clone.
#[derive(Clone)]
pub struct HealthRegistry {
    hub: Hub,
    feeds: Arc<RwLock<BTreeMap<String, FeedHealth>>>,
}

impl HealthRegistry {
    pub fn new(hub: Hub) -> Self {
        Self {
            hub,
            feeds: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub fn snapshot(&self) -> Vec<FeedHealth> {
        self.feeds.read().unwrap().values().cloned().collect()
    }

    pub fn get(&self, name: &str) -> Option<FeedHealth> {
        self.feeds.read().unwrap().get(name).cloned()
    }

    /// Add a feed to the table. Supervised feeds are registered automatically.
    pub fn register(&self, name: &str, expected_interval: Duration) {
        let mut feeds = self.feeds.write().unwrap();
        feeds
            .entry(name.to_string())
            .or_insert_with(|| FeedHealth::new(name, expected_interval));
    }

    /// Record a successful cycle. `data_time` only moves forward.
    pub fn ok(&self, name: &str, data_time: Option<DateTime<Utc>>, detail: impl Into<String>) {
        self.update(name, |h| {
            h.state = FeedState::Healthy;
            h.last_success = Some(Utc::now());
            h.consecutive_failures = 0;
            h.next_retry_at = None;
            if let Some(t) = data_time {
                h.last_data_time = Some(h.last_data_time.map_or(t, |old| old.max(t)));
            }
            h.detail = detail.into();
        });
    }

    /// Say how often new data is expected: see
    /// [`FeedHealth::max_data_age_secs`].
    pub fn expect_data_within(&self, name: &str, max_age: Option<Duration>) {
        self.update(name, |h| h.max_data_age_secs = max_age.map(|d| d.as_secs()));
    }

    fn update(&self, name: &str, f: impl FnOnce(&mut FeedHealth)) {
        let changed = {
            let mut feeds = self.feeds.write().unwrap();
            let Some(h) = feeds.get_mut(name) else {
                return;
            };
            let before = (h.state, h.last_data_time, h.consecutive_failures);
            f(h);
            (before != (h.state, h.last_data_time, h.consecutive_failures)).then(|| h.clone())
        };
        // Only state changes and new data are broadcast, not every quiet poll.
        if let Some(feed) = changed {
            self.hub.send(WsEvent::Feed { feed });
        }
    }
}

/// Restart policy for a supervised feed.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    /// Delay before the first restart.
    pub base: Duration,
    /// Upper bound on the delay.
    pub max: Duration,
    /// Multiplier applied per consecutive failure.
    pub factor: f64,
    /// A run that lasted at least this long and reported success resets the
    /// failure count.
    pub reset_after: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(2),
            max: Duration::from_secs(300),
            factor: 2.0,
            reset_after: Duration::from_secs(120),
        }
    }
}

impl Backoff {
    /// Delay before restart number `failures` (1-based), without jitter.
    pub fn delay(&self, failures: u32) -> Duration {
        let exp = self.factor.powi(failures.saturating_sub(1).min(30) as i32);
        let secs = (self.base.as_secs_f64() * exp).min(self.max.as_secs_f64());
        Duration::from_secs_f64(secs)
    }

    /// [`Backoff::delay`] spread by +/-25% so feeds that fail together do not
    /// retry together.
    pub fn jittered(&self, failures: u32) -> Duration {
        self.delay(failures)
            .mul_f64(0.75 + 0.5 * rand::random::<f64>())
    }
}

/// Handle a feed uses to report progress and notice shutdown.
#[derive(Clone)]
pub struct FeedCtx {
    name: Arc<str>,
    health: HealthRegistry,
    cancel: CancellationToken,
}

impl FeedCtx {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Report a successful cycle. See [`HealthRegistry::ok`].
    pub fn ok(&self, data_time: Option<DateTime<Utc>>, detail: impl Into<String>) {
        self.health.ok(&self.name, data_time, detail);
    }

    /// Say how often new data is expected. See
    /// [`HealthRegistry::expect_data_within`].
    pub fn expect_data_within(&self, max_age: Option<Duration>) {
        self.health.expect_data_within(&self.name, max_age);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Resolves when the server is shutting down.
    pub async fn cancelled(&self) {
        self.cancel.cancelled().await
    }

    /// Sleep, returning early (with `false`) on shutdown.
    pub async fn sleep(&self, d: Duration) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(d) => true,
            _ = self.cancel.cancelled() => false,
        }
    }
}

/// Run `run` forever as its own task. When it returns an error, panics, or
/// returns at all, it is restarted after a backoff delay; the delay grows with
/// consecutive failures and resets once the feed has run healthily for a
/// while. Cancelling `cancel` stops the feed and the supervisor.
pub fn spawn_feed<F, Fut>(
    name: &str,
    expected_interval: Duration,
    health: &HealthRegistry,
    cancel: CancellationToken,
    backoff: Backoff,
    run: F,
) -> JoinHandle<()>
where
    F: Fn(FeedCtx) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
{
    health.register(name, expected_interval);
    let ctx = FeedCtx {
        name: name.into(),
        health: health.clone(),
        cancel: cancel.clone(),
    };
    let health = health.clone();
    let name = name.to_string();
    tokio::spawn(async move {
        let mut failures = 0u32;
        loop {
            let started = Instant::now();
            let started_at = Utc::now();
            health.update(&name, |h| {
                h.state = FeedState::Starting;
                h.started_at = Some(started_at);
                h.next_retry_at = None;
            });
            let mut task = tokio::spawn(run(ctx.clone()));
            let outcome = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    task.abort();
                    health.update(&name, |h| h.state = FeedState::Stopped);
                    return;
                }
                r = &mut task => r,
            };
            // A feed that returns because it noticed shutdown has not failed,
            // even when it wins the race against the branch above.
            if cancel.is_cancelled() {
                health.update(&name, |h| h.state = FeedState::Stopped);
                return;
            }
            let error = match outcome {
                Ok(Ok(())) => "feed task exited".to_string(),
                Ok(Err(e)) => format!("{e:#}"),
                Err(join) if join.is_panic() => {
                    let payload = join.into_panic();
                    let msg = payload
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_string());
                    format!("panicked: {msg}")
                }
                Err(_) => return,
            };

            let succeeded_this_run = health
                .get(&name)
                .and_then(|h| h.last_success)
                .is_some_and(|t| t >= started_at);
            if succeeded_this_run && started.elapsed() >= backoff.reset_after {
                failures = 0;
            }
            failures += 1;
            let delay = backoff.jittered(failures);
            tracing::warn!(feed = %name, failures, retry_in = ?delay, "feed failed: {error}");
            health.update(&name, |h| {
                h.state = FeedState::BackingOff;
                h.last_error = Some(error);
                h.last_error_at = Some(Utc::now());
                h.consecutive_failures = failures;
                h.restarts += 1;
                h.next_retry_at =
                    Some(Utc::now() + chrono::Duration::from_std(delay).unwrap_or_default());
            });
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = cancel.cancelled() => {
                    health.update(&name, |h| h.state = FeedState::Stopped);
                    return;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn quick() -> Backoff {
        Backoff {
            base: Duration::from_millis(10),
            max: Duration::from_millis(80),
            factor: 2.0,
            reset_after: Duration::from_secs(3600),
        }
    }

    #[test]
    fn delay_grows_then_caps() {
        let b = Backoff::default();
        assert_eq!(b.delay(1), Duration::from_secs(2));
        assert_eq!(b.delay(2), Duration::from_secs(4));
        assert_eq!(b.delay(5), Duration::from_secs(32));
        assert_eq!(b.delay(20), Duration::from_secs(300));
        assert_eq!(b.delay(u32::MAX), Duration::from_secs(300));
        for n in 1..10 {
            let j = b.jittered(n).as_secs_f64();
            let d = b.delay(n).as_secs_f64();
            assert!(j >= d * 0.75 && j <= d * 1.25);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn restarts_after_errors_and_panics() {
        let health = HealthRegistry::new(Hub::new());
        let cancel = CancellationToken::new();
        let runs = Arc::new(AtomicU32::new(0));
        let counter = runs.clone();
        let handle = spawn_feed(
            "flaky",
            Duration::from_secs(60),
            &health,
            cancel.clone(),
            quick(),
            move |ctx| {
                let counter = counter.clone();
                async move {
                    match counter.fetch_add(1, Ordering::SeqCst) {
                        0 => anyhow::bail!("upstream said no"),
                        1 => panic!("decoder blew up"),
                        _ => {
                            ctx.ok(Some(Utc::now()), "caught up");
                            ctx.cancelled().await;
                            Ok(())
                        }
                    }
                }
            },
        );

        tokio::time::sleep(Duration::from_millis(5)).await;
        let h = health.get("flaky").unwrap();
        assert_eq!(h.state, FeedState::BackingOff);
        assert_eq!(h.last_error.as_deref(), Some("upstream said no"));
        assert_eq!(h.consecutive_failures, 1);

        tokio::time::sleep(Duration::from_secs(2)).await;
        let h = health.get("flaky").unwrap();
        assert_eq!(runs.load(Ordering::SeqCst), 3);
        assert_eq!(h.state, FeedState::Healthy);
        assert_eq!(h.restarts, 2);
        assert_eq!(h.consecutive_failures, 0);
        assert_eq!(h.last_error.as_deref(), Some("panicked: decoder blew up"));
        assert_eq!(h.detail, "caught up");
        assert!(h.last_data_time.is_some());

        cancel.cancel();
        handle.await.unwrap();
        assert_eq!(health.get("flaky").unwrap().state, FeedState::Stopped);
    }

    #[tokio::test(start_paused = true)]
    async fn announces_state_changes() {
        let hub = Hub::new();
        let mut rx = hub.subscribe();
        let health = HealthRegistry::new(hub);
        health.register("quiet", Duration::from_secs(30));
        health.ok("quiet", None, "first");
        health.ok("quiet", None, "second");
        let mut events = 0;
        while let Ok(ev) = rx.try_recv() {
            assert!(matches!(&*ev, WsEvent::Feed { .. }));
            events += 1;
        }
        assert_eq!(events, 1, "a repeat success with no new data is not news");
    }

    /// A feed whose cycles succeed while it delivers nothing new is stale
    /// once its data is older than it says it should get.
    #[test]
    fn data_that_stops_coming_is_stale() {
        let health = HealthRegistry::new(Hub::new());
        health.register("model", Duration::from_secs(600));
        let now = Utc::now();
        health.ok("model", Some(now - chrono::Duration::hours(5)), "run 12Z");
        assert!(
            !health.get("model").unwrap().is_stale(now),
            "judged by its cycles alone"
        );
        health.expect_data_within("model", Some(Duration::from_secs(4 * 3600)));
        let h = health.get("model").unwrap();
        assert_eq!(h.max_data_age_secs, Some(4 * 3600));
        assert!(
            h.is_stale(now),
            "the cycle succeeded, but nothing new in five hours"
        );
        health.ok("model", Some(now - chrono::Duration::hours(1)), "run 16Z");
        assert!(!health.get("model").unwrap().is_stale(now));
    }

    #[test]
    fn staleness() {
        let mut h = FeedHealth::new("x", Duration::from_secs(120));
        let now = Utc::now();
        assert!(!h.is_stale(now));
        h.last_success = Some(now - chrono::Duration::seconds(300));
        assert!(!h.is_stale(now));
        h.last_success = Some(now - chrono::Duration::seconds(400));
        assert!(h.is_stale(now));
    }
}
