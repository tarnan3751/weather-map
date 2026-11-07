//! Zone boundaries, fetched in the background so a slow or failing
//! api.weather.gov never holds up the poll loop.
//!
//! The poll cycle queues the zones it lacks (warnings first); a few
//! workers fetch them politely: request starts are spaced out across all
//! workers, nothing is sent while the shared api.weather.gov gate is
//! paused (a 403 or 429 pauses it, see `upstream`), and 5xx are retried
//! with backoff. After zones land, the shaper gives waiting alerts their
//! shapes. A zone that keeps failing cools down and is asked for again by a
//! later cycle; after a couple of failed rounds its alerts are drawn from
//! their other zones meanwhile.

use crate::Inner;
use crate::nws::{self, ZoneRef};
use crate::push::transport_error;
use crate::upstream::retry_after;
use reqwest::StatusCode;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// Attempts per zone within one request, for 429/5xx and network errors.
const ATTEMPTS: u32 = 4;
/// A zone the API does not have (404, or no boundary) is not asked for
/// again for this long.
const UNOBTAINABLE_FOR: Duration = Duration::from_secs(6 * 3600);
const MAX_COOLDOWN: Duration = Duration::from_secs(30 * 60);
/// Failed rounds after which an alert is drawn without the zone, until it
/// arrives.
const GIVE_UP_AFTER_ROUNDS: u32 = 2;
/// Alaskan coastal zones run to several megabytes of GeoJSON.
const MAX_ZONE_BYTES: usize = 64 << 20;
/// Let a burst of zones land before taking the write lock to assign shapes.
const SHAPER_DEBOUNCE: Duration = Duration::from_millis(250);
/// While a cold start works through hundreds of zones, publish in batches:
/// every publish makes every open map refetch `/api/alerts`.
const SHAPER_BUSY_DEBOUNCE: Duration = Duration::from_secs(5);
const SHAPER_BUSY_BACKLOG: usize = 20;

pub(crate) struct ZoneQueue {
    state: Mutex<QueueState>,
    wake: Notify,
    stored: Notify,
    pacer: Pacer,
}

/// Spaces request starts out across all workers.
struct Pacer {
    interval: Duration,
    next: Mutex<tokio::time::Instant>,
}

impl Pacer {
    /// Wait for this request's slot.
    async fn turn(&self) {
        let at = {
            let mut next = crate::lock(&self.next);
            let at = (*next).max(tokio::time::Instant::now());
            *next = at + self.interval;
            at
        };
        tokio::time::sleep_until(at).await;
    }
}

impl ZoneQueue {
    /// `interval`: minimum spacing between zone request starts.
    pub fn new(interval: Duration) -> Self {
        Self {
            state: Mutex::default(),
            wake: Notify::new(),
            stored: Notify::new(),
            pacer: Pacer {
                interval,
                next: Mutex::new(tokio::time::Instant::now()),
            },
        }
    }
}

#[derive(Default)]
struct QueueState {
    /// (priority, arrival) -> zone; priority 0 is for zones a warning needs.
    pending: BTreeMap<(u8, u64), ZoneRef>,
    index: HashMap<String, (u8, u64)>,
    in_flight: HashSet<String>,
    cooldown: HashMap<String, Cooldown>,
    seq: u64,
}

struct Cooldown {
    until: Instant,
    failures: u32,
    unobtainable: bool,
}

#[derive(Debug)]
pub(crate) enum Outcome {
    Stored,
    /// The API has no boundary for this zone.
    Unobtainable,
    Failed(String),
}

impl ZoneQueue {
    fn lock(&self) -> std::sync::MutexGuard<'_, QueueState> {
        crate::lock(&self.state)
    }

    /// Queue zones that are not already queued, in flight or cooling down.
    pub fn request(&self, zones: impl IntoIterator<Item = ZoneRef>, urgent: bool) -> usize {
        let priority = if urgent { 0 } else { 1 };
        let now = Instant::now();
        let mut added = 0;
        {
            let mut s = self.lock();
            for z in zones {
                if s.in_flight.contains(&z.id)
                    || s.cooldown.get(&z.id).is_some_and(|c| c.until > now)
                {
                    continue;
                }
                match s.index.get(&z.id).copied() {
                    Some((p, _)) if p <= priority => continue,
                    Some(key) => {
                        s.pending.remove(&key);
                    }
                    None => {}
                }
                s.seq += 1;
                let key = (priority, s.seq);
                s.index.insert(z.id.clone(), key);
                s.pending.insert(key, z);
                added += 1;
            }
        }
        // One wake-up per new zone (capped) so idle workers all start.
        for _ in 0..added.min(64) {
            self.wake.notify_one();
        }
        added
    }

    fn pop(&self) -> Option<ZoneRef> {
        let mut s = self.lock();
        let (_, z) = s.pending.pop_first()?;
        s.index.remove(&z.id);
        s.in_flight.insert(z.id.clone());
        Some(z)
    }

    fn finish(&self, z: &ZoneRef, outcome: &Outcome, retry_base: Duration) {
        let mut s = self.lock();
        s.in_flight.remove(&z.id);
        match outcome {
            Outcome::Stored => {
                s.cooldown.remove(&z.id);
            }
            Outcome::Unobtainable => {
                s.cooldown.insert(
                    z.id.clone(),
                    Cooldown {
                        until: Instant::now() + UNOBTAINABLE_FOR,
                        failures: 0,
                        unobtainable: true,
                    },
                );
            }
            Outcome::Failed(_) => {
                let failures = s.cooldown.get(&z.id).map_or(0, |c| c.failures) + 1;
                let wait =
                    (retry_base * 15 * 2u32.saturating_pow(failures.min(10))).min(MAX_COOLDOWN);
                s.cooldown.insert(
                    z.id.clone(),
                    Cooldown {
                        until: Instant::now() + wait,
                        failures,
                        unobtainable: false,
                    },
                );
            }
        }
    }

    /// Not worth waiting for: the API has no boundary for the zone, or it has
    /// failed a couple of rounds. Its alerts are drawn from their other
    /// zones meanwhile, and redrawn if it arrives later.
    pub fn given_up(&self, id: &str) -> bool {
        let now = Instant::now();
        self.lock().cooldown.get(id).is_some_and(|c| {
            (c.unobtainable && c.until > now)
                || (!c.unobtainable && c.failures >= GIVE_UP_AFTER_ROUNDS)
        })
    }

    /// Zones queued or being fetched.
    pub fn backlog(&self) -> usize {
        let s = self.lock();
        s.pending.len() + s.in_flight.len()
    }
}

/// One fetch worker: takes zones off the queue until shutdown.
pub(crate) async fn worker(inner: Arc<Inner>, cancel: CancellationToken) {
    loop {
        let Some(zone) = inner.zones.pop() else {
            tokio::select! {
                _ = inner.zones.wake.notified() => continue,
                _ = cancel.cancelled() => return,
            }
        };
        let outcome = tokio::select! {
            o = fetch_and_store(&inner, &zone) => o,
            _ = cancel.cancelled() => return,
        };
        inner
            .zones
            .finish(&zone, &outcome, inner.config.retry_base_delay);
        match &outcome {
            Outcome::Stored => inner.zones.stored.notify_one(),
            Outcome::Unobtainable => {
                tracing::info!(zone = %zone.id, kind = %zone.kind, "zone has no boundary at the NWS");
                // Alerts waiting only on this zone can now use the others.
                inner.zones.stored.notify_one();
            }
            Outcome::Failed(e) => tracing::warn!(zone = %zone.id, "zone fetch failed: {e}"),
        }
    }
}

/// Gives waiting alerts their shapes after zones arrive.
pub(crate) async fn shaper(inner: Arc<Inner>, cancel: CancellationToken) {
    loop {
        tokio::select! {
            _ = inner.zones.stored.notified() => {}
            _ = cancel.cancelled() => return,
        }
        let wait = if inner.zones.backlog() > SHAPER_BUSY_BACKLOG {
            SHAPER_BUSY_DEBOUNCE
        } else {
            SHAPER_DEBOUNCE
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = cancel.cancelled() => return,
        }
        if let Err(e) = inner.apply_new_zones().await {
            tracing::warn!("assigning zone shapes failed: {e:#}");
        }
    }
}

async fn fetch_and_store(inner: &Inner, zone: &ZoneRef) -> Outcome {
    // Kind and id were validated when the reference was parsed, so they
    // are safe path segments.
    let url = format!("{}/zones/{}/{}", inner.api_base, zone.kind, zone.id);
    let limits = &inner.nws_limits;
    let mut last_error = String::new();
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(inner.config.retry_base_delay * 2u32.pow(attempt - 1)).await;
        }
        // Wait out a pause (ours or another crate's), then take a slot.
        limits.wait_open().await;
        inner.zones.pacer.turn().await;
        limits.wait_open().await;
        let resp = inner
            .nws
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/geo+json")
            .timeout(Duration::from_secs(60))
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                last_error = transport_error(&e);
                continue;
            }
        };
        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            limits.ok();
            return Outcome::Unobtainable;
        }
        if status == StatusCode::FORBIDDEN {
            // Retrying into a CDN block only makes it last longer.
            let pause = limits.refused(retry_after(&resp));
            return Outcome::Failed(format!(
                "HTTP 403 (refused; api.weather.gov requests paused for {}s)",
                pause.as_secs()
            ));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            // The next attempt waits for the gate.
            limits.rate_limited(retry_after(&resp));
            last_error = "HTTP 429".to_string();
            continue;
        }
        if status.is_server_error() {
            last_error = format!("HTTP {}", status.as_u16());
            continue;
        }
        if !status.is_success() {
            return Outcome::Failed(format!("HTTP {}", status.as_u16()));
        }
        limits.ok();
        let body = match crate::feed::read_limited(resp, MAX_ZONE_BYTES).await {
            Ok(b) => b,
            Err(e) => {
                last_error = format!("{e:#}");
                continue;
            }
        };
        return match nws::parse_zone(&body, zone) {
            Ok(Some(data)) => match store_zone(&inner.pool, &data).await {
                Ok(true) => Outcome::Stored,
                Ok(false) => Outcome::Unobtainable,
                Err(e) => Outcome::Failed(format!("storing zone: {e}")),
            },
            Ok(None) => Outcome::Unobtainable,
            Err(e) => Outcome::Failed(format!("{e:#}")),
        };
    }
    Outcome::Failed(last_error)
}

/// `false` when nothing polygonal survives `ST_MakeValid`.
async fn store_zone(pool: &sqlx::PgPool, z: &nws::ZoneData) -> Result<bool, sqlx::Error> {
    let stored: Option<String> = sqlx::query_scalar(
        "INSERT INTO zones (id, kind, name, state, geom, fetched_at)
         SELECT $1, $2, $3, $4, g, now()
         FROM (SELECT ST_Multi(ST_CollectionExtract(
                   ST_MakeValid(ST_SetSRID(ST_GeomFromGeoJSON($5), 4326)), 3)) AS g) s
         WHERE NOT ST_IsEmpty(g)
         ON CONFLICT (id) DO UPDATE SET
             kind = EXCLUDED.kind, name = EXCLUDED.name, state = EXCLUDED.state,
             geom = EXCLUDED.geom, fetched_at = now()
         RETURNING id",
    )
    .bind(&z.id)
    .bind(&z.kind)
    .bind(&z.name)
    .bind(&z.state)
    .bind(&z.geometry)
    .fetch_optional(pool)
    .await?;
    Ok(stored.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(id: &str) -> ZoneRef {
        ZoneRef {
            kind: "forecast".into(),
            id: id.into(),
        }
    }

    #[test]
    fn warnings_jump_the_queue_and_duplicates_collapse() {
        let q = ZoneQueue::new(Duration::ZERO);
        assert_eq!(q.request([z("AAZ001"), z("AAZ002")], false), 2);
        assert_eq!(q.request([z("AAZ002"), z("AAZ003")], true), 2);
        assert_eq!(q.request([z("AAZ001")], false), 0);
        assert_eq!(q.backlog(), 3);
        assert_eq!(q.pop().unwrap().id, "AAZ002");
        assert_eq!(q.pop().unwrap().id, "AAZ003");
        // In flight: not queued again.
        assert_eq!(q.request([z("AAZ002")], true), 0);
        assert_eq!(q.pop().unwrap().id, "AAZ001");
        assert!(q.pop().is_none());
        assert_eq!(q.backlog(), 3);
    }

    #[test]
    fn failures_cool_down() {
        let q = ZoneQueue::new(Duration::ZERO);
        q.request([z("AAZ001"), z("AAZ002"), z("AAZ003")], false);
        let a = q.pop().unwrap();
        let b = q.pop().unwrap();
        let c = q.pop().unwrap();
        q.finish(
            &a,
            &Outcome::Failed("HTTP 503".into()),
            Duration::from_secs(1),
        );
        q.finish(&b, &Outcome::Unobtainable, Duration::from_secs(1));
        q.finish(&c, &Outcome::Stored, Duration::from_secs(1));
        assert_eq!(q.backlog(), 0);
        assert_eq!(
            q.request([a.clone(), b.clone(), c.clone()], true),
            1,
            "only the stored zone may be asked again"
        );
        assert!(q.given_up(&b.id), "404: not worth waiting for");
        assert!(
            !q.given_up(&a.id),
            "one failed round: still worth waiting for"
        );
        assert!(!q.given_up(&c.id));

        // With a zero base the cooldown is over at once.
        let q = ZoneQueue::new(Duration::ZERO);
        q.request([z("AAZ001")], false);
        let a = q.pop().unwrap();
        q.finish(&a, &Outcome::Failed("x".into()), Duration::ZERO);
        assert_eq!(q.request([a], false), 1);
    }

    #[test]
    fn a_zone_failing_round_after_round_is_given_up_until_it_arrives() {
        let q = ZoneQueue::new(Duration::ZERO);
        for round in 1..=GIVE_UP_AFTER_ROUNDS {
            assert_eq!(q.request([z("AAZ009")], true), 1);
            let a = q.pop().unwrap();
            q.finish(&a, &Outcome::Failed("timed out".into()), Duration::ZERO);
            assert_eq!(
                q.given_up("AAZ009"),
                round >= GIVE_UP_AFTER_ROUNDS,
                "round {round}"
            );
        }
        // Still asked for again, and forgiven once it arrives.
        assert_eq!(q.request([z("AAZ009")], true), 1);
        let a = q.pop().unwrap();
        q.finish(&a, &Outcome::Stored, Duration::ZERO);
        assert!(!q.given_up("AAZ009"));
    }

    #[tokio::test(start_paused = true)]
    async fn pacer_spaces_requests() {
        let q = ZoneQueue::new(Duration::from_millis(250));
        let start = tokio::time::Instant::now();
        for _ in 0..5 {
            q.pacer.turn().await;
        }
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(1000),
            "five starts, 250 ms apart"
        );
    }
}
