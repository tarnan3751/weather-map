//! The `alerts` feed: one poll cycle of the NWS active-alerts feed, and
//! what both feeds share: expiry, publishing, and the safety-net sweep.

use crate::cache::ListQuery;
use crate::nws::{self, FeedAlert, ParsedFeed};
use crate::upstream::retry_after;
use crate::{Inner, notify, shapes};
use anyhow::Context;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Connection, Postgres, Transaction};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use wx_core::{FeedCtx, WsEvent};

/// The national feed is a few megabytes; anything far larger is broken.
const MAX_FEED_BYTES: usize = 64 << 20;
/// How often the safety-net sweep runs (full re-match, retention).
const SWEEP_EVERY: Duration = Duration::from_secs(120);

/// What one cycle changed, for the map and for notifications.
#[derive(Debug, Default)]
pub(crate) struct Changes {
    /// Newly drawn: active and with a shape for the first time.
    pub added: BTreeSet<String>,
    /// Still drawn, but different.
    pub updated: BTreeSet<String>,
    /// No longer drawn.
    pub removed: BTreeSet<String>,
    /// Alerts to match against saved places now.
    pub to_match: BTreeSet<String>,
    /// World alerts without a shape that started or stopped being listed.
    /// Not on the map, but in `/api/alerts`, so the revision must rise.
    pub listed: BTreeSet<String>,
}

impl Changes {
    /// Take in what another step changed.
    pub fn absorb(&mut self, other: Changes) {
        self.added.extend(other.added);
        self.updated.extend(other.updated);
        self.removed.extend(other.removed);
        self.to_match.extend(other.to_match);
        self.listed.extend(other.listed);
    }

    /// An alert that was drawn (or not) and now is (or is not).
    pub fn redrawn(&mut self, id: String, before: bool, after: bool) {
        match (before, after) {
            (false, true) => self.added.insert(id.clone()),
            (true, true) => self.updated.insert(id.clone()),
            (true, false) => self.removed.insert(id.clone()),
            (false, false) => false,
        };
        if after {
            self.to_match.insert(id);
        }
    }

    fn normalize(&mut self) {
        // Removed and drawn again within one cycle: a zone alert whose zone
        // list changed under the same id loses its shape on the upsert and
        // gets the new one from the shape step. It never left the map.
        let redrawn: Vec<String> = self.added.intersection(&self.removed).cloned().collect();
        for id in redrawn {
            self.added.remove(&id);
            self.removed.remove(&id);
            self.updated.insert(id.clone());
            self.to_match.insert(id);
        }
        for id in &self.removed {
            self.updated.remove(id);
            self.to_match.remove(id);
        }
        for id in &self.added {
            self.updated.remove(id);
        }
    }

    fn visible(&self) -> bool {
        !(self.added.is_empty()
            && self.updated.is_empty()
            && self.removed.is_empty()
            && self.listed.is_empty())
    }
}

/// Survives feed restarts, so a failed cycle does not throw away what is
/// known about the feed.
#[derive(Default)]
pub(crate) struct PollState {
    etag: Option<String>,
    last_modified: Option<String>,
    body_hash: Option<[u8; 32]>,
    newest_sent: Option<DateTime<Utc>>,
    /// Content hash per alert id as last written, so unchanged alerts are
    /// not rewritten every cycle.
    known: HashMap<String, [u8; 32]>,
    /// When the local part of a cycle (expiry, shapes, notifications,
    /// sweep) last ran, with or without a fetch.
    last_local: Option<Instant>,
}

/// The outcome of [`crate::Alerts::poll_once`].
#[derive(Debug, Clone, Default)]
pub struct PollReport {
    /// False when the feed was unchanged (304, or the same body as last time).
    pub feed_changed: bool,
    /// Alerts in the feed with `status: Actual` (when it changed).
    pub feed_alerts: usize,
    /// Features that could not be understood and were skipped.
    pub skipped: usize,
    /// Active alerts that have a shape (what `/api/alerts` serves).
    pub drawn: usize,
    /// Active alerts still waiting for zone boundaries.
    pub awaiting_zones: usize,
    /// Active alerts whose event ends in "Warning".
    pub warnings: usize,
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    /// Notifications created this cycle.
    pub notifications: usize,
    /// Zones queued or being fetched.
    pub zone_backlog: usize,
    /// Newest `sent` time in the feed.
    pub newest_sent: Option<DateTime<Utc>>,
}

impl PollReport {
    pub fn detail(&self) -> String {
        let mut s = format!(
            "{} alerts drawn ({} warnings), {} awaiting zones",
            self.drawn, self.warnings, self.awaiting_zones
        );
        if self.zone_backlog > 0 {
            s.push_str(&format!(", {} zones to fetch", self.zone_backlog));
        }
        if self.added + self.updated + self.removed > 0 {
            s.push_str(&format!(
                "; +{} ~{} -{}",
                self.added, self.updated, self.removed
            ));
        } else if !self.feed_changed {
            s.push_str("; feed unchanged");
        }
        if self.notifications > 0 {
            s.push_str(&format!("; {} notifications", self.notifications));
        }
        s
    }
}

struct FeedBody {
    bytes: Vec<u8>,
    hash: [u8; 32],
    etag: Option<String>,
    last_modified: Option<String>,
}

impl Inner {
    pub(crate) async fn run_feed(self: &Arc<Self>, ctx: FeedCtx) -> anyhow::Result<()> {
        let interval = self.config.poll_interval;
        loop {
            let started = Instant::now();
            let report = self.poll_once().await?;
            ctx.ok(report.newest_sent, report.detail());
            let wait = interval
                .saturating_sub(started.elapsed())
                .max(interval / 10);
            if !ctx.sleep(wait).await {
                return Ok(());
            }
        }
    }

    /// Fetch, then do the local part. When the fetch fails, or is skipped
    /// because api.weather.gov asked us to back off, the local part still
    /// runs (ended warnings leave the map, pending pushes are retried)
    /// before the error is returned to the supervisor.
    pub(crate) async fn poll_once(self: &Arc<Self>) -> anyhow::Result<PollReport> {
        let mut st = self.poll.lock().await;
        let fetched = match self.nws_limits.paused_for() {
            Some(left) => Err(anyhow::anyhow!(
                "alerts feed not fetched: api.weather.gov asked us to back off ({}s left)",
                left.as_secs().max(1)
            )),
            None => self.fetch_feed(&st).await,
        };
        match fetched {
            Ok(body) => self.cycle(&mut st, body).await,
            Err(fetch_error) => {
                if let Err(e) = self.cycle(&mut st, None).await {
                    tracing::warn!("alerts upkeep failed while the feed is unavailable: {e:#}");
                }
                Err(fetch_error)
            }
        }
    }

    /// Between failed fetches the supervisor backs off for up to minutes;
    /// the local part keeps its own pace meanwhile.
    pub(crate) async fn upkeep(self: Arc<Self>, cancel: CancellationToken) {
        let every = self.config.poll_interval;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(every) => {}
                _ = cancel.cancelled() => return,
            }
            // A cycle in progress does the local part itself.
            let Ok(mut st) = self.poll.try_lock() else {
                continue;
            };
            if st.last_local.is_some_and(|t| t.elapsed() < every * 2) {
                continue;
            }
            if let Err(e) = self.cycle(&mut st, None).await {
                tracing::warn!("alerts upkeep failed: {e:#}");
            }
        }
    }

    /// Everything but the fetch: apply the body (if there is a new one),
    /// expire, shape, notify and publish, sweep.
    async fn cycle(
        self: &Arc<Self>,
        st: &mut PollState,
        body: Option<FeedBody>,
    ) -> anyhow::Result<PollReport> {
        let mut report = PollReport::default();
        let mut ch = Changes::default();
        let outcome: anyhow::Result<()> = async {
            let _writes = self.writes.lock().await;
            if let Some(body) = body {
                let feed = nws::parse_feed(&body.bytes, Utc::now())?;
                if feed.skipped > 0 {
                    tracing::warn!(
                        skipped = feed.skipped,
                        "alerts feed had features that could not be read"
                    );
                }
                self.apply_feed(st, &feed, &mut ch).await?;
                // Only now: a cycle that failed half-way must not make the
                // same body look "already processed".
                st.etag = body.etag;
                st.last_modified = body.last_modified;
                st.body_hash = Some(body.hash);
                st.newest_sent = feed.newest_sent();
                report.feed_changed = true;
                report.feed_alerts = feed.alerts.len();
                report.skipped = feed.skipped;
            }
            self.expire_ended(&mut ch).await?;
            shapes::assign(self, &mut ch).await
        }
        .await;
        ch.normalize();
        report.added = ch.added.len();
        report.updated = ch.updated.len();
        report.removed = ch.removed.len();
        // Even when a later step failed: what is committed must reach maps
        // and phones, since the next cycle will not see it as new.
        report.notifications = self.publish(ch).await;
        outcome?;

        if self.sweep_due() {
            match self.sweep().await {
                Ok(n) => {
                    report.notifications += n;
                    self.sweep_done();
                }
                Err(e) => {
                    self.force_sweep.store(true, Ordering::Release);
                    tracing::warn!("alerts sweep failed: {e:#}");
                }
            }
        }
        st.last_local = Some(Instant::now());

        let (drawn, awaiting, warnings): (i64, i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE geom IS NOT NULL),
                    count(*) FILTER (WHERE geom IS NULL AND cardinality(ugc) > 0),
                    count(*) FILTER (WHERE event LIKE '%Warning')
             FROM alerts WHERE active AND origin = 'nws'",
        )
        .fetch_one(&self.pool)
        .await?;
        report.drawn = drawn as usize;
        report.awaiting_zones = awaiting as usize;
        report.warnings = warnings as usize;
        report.zone_backlog = self.zones.backlog();
        report.newest_sent = st.newest_sent;
        Ok(report)
    }

    /// `None` when the feed has not changed since the last processed body.
    async fn fetch_feed(&self, st: &PollState) -> anyhow::Result<Option<FeedBody>> {
        let mut req = self
            .nws
            .get(format!("{}/alerts/active", self.api_base))
            .header(reqwest::header::ACCEPT, "application/geo+json")
            .timeout(Duration::from_secs(30));
        // Only worth asking once the previous body was fully processed.
        if st.body_hash.is_some() {
            if let Some(etag) = &st.etag {
                req = req.header(reqwest::header::IF_NONE_MATCH, etag);
            }
            if let Some(lm) = &st.last_modified {
                req = req.header(reqwest::header::IF_MODIFIED_SINCE, lm);
            }
        }
        let resp = req
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("alerts feed: {}", crate::push::transport_error(&e)))?;
        let status = resp.status();
        if status == StatusCode::FORBIDDEN {
            let pause = self.nws_limits.refused(retry_after(&resp));
            anyhow::bail!(
                "alerts feed: HTTP 403, refused by api.weather.gov; all requests to it paused for {}s",
                pause.as_secs()
            );
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            let pause = self.nws_limits.rate_limited(retry_after(&resp));
            anyhow::bail!(
                "alerts feed: HTTP 429; all requests to api.weather.gov paused for {}s",
                pause.as_secs()
            );
        }
        if status == StatusCode::NOT_MODIFIED {
            self.nws_limits.ok();
            return Ok(None);
        }
        anyhow::ensure!(
            status.is_success(),
            "alerts feed answered HTTP {}",
            status.as_u16()
        );
        self.nws_limits.ok();
        let header = |name| {
            resp.headers()
                .get(name)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
                .map(str::to_string)
        };
        let etag = header(reqwest::header::ETAG);
        let last_modified = header(reqwest::header::LAST_MODIFIED);
        let bytes = read_limited(resp, MAX_FEED_BYTES)
            .await
            .context("alerts feed")?;
        // api.weather.gov ignores If-None-Match but serves byte-identical
        // bodies while nothing changes, so the body hash is the real check.
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        if st.body_hash == Some(hash) {
            return Ok(None);
        }
        Ok(Some(FeedBody {
            bytes,
            hash,
            etag,
            last_modified,
        }))
    }

    /// The event key for each alert in the feed, given what is `stored`
    /// about the feed's messages and the messages they reference.
    ///
    /// A message continues the hazard of the messages it references, so it
    /// takes the key of a stored (or earlier, same-response) message it
    /// references, provided that one is about the same event: same event
    /// name, and for VTEC alerts the same office, phenomenon, significance
    /// and event number. That keeps the key stable across the new year and
    /// across updates of warnings without VTEC, whose own id changes with
    /// every update. A VTEC continuation that references nothing we have
    /// (the API often lists no references for CON and EXT) takes the key of
    /// the newest stored message about the same event number. Only failing
    /// both is the key derived from the message alone. A stored message
    /// keeps the key it was stored with.
    async fn resolve_keys(
        &self,
        feed: &ParsedFeed,
        stored: &HashMap<String, StoredAlert>,
    ) -> Result<HashMap<String, String>, sqlx::Error> {
        let mut known: HashMap<&str, (String, &str)> = stored
            .iter()
            .map(|(id, s)| (id.as_str(), (s.event_key.clone(), s.event.as_str())))
            .collect();
        let patterns: Vec<String> = feed
            .alerts
            .iter()
            .filter(|a| a.continues)
            .filter_map(|a| a.vtec_prefix.as_ref().map(|p| format!("{p}%")))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let by_number: Vec<(String, String)> = if patterns.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as(
                "SELECT event_key, event FROM alerts
                 WHERE event_key LIKE ANY($1) AND last_seen > now() - interval '30 days'
                 ORDER BY first_seen DESC",
            )
            .bind(&patterns)
            .fetch_all(&self.pool)
            .await?
        };

        let same_event = |a: &FeedAlert, key: &str, event: &str| {
            event == a.event && a.vtec_prefix.as_deref().is_none_or(|p| key.starts_with(p))
        };
        // Referenced messages were sent before the ones referencing them.
        let mut order: Vec<&FeedAlert> = feed.alerts.iter().collect();
        order.sort_by_key(|a| a.sent);
        let mut keys = HashMap::with_capacity(order.len());
        for a in order {
            let key = if let Some((key, _)) = known.get(a.id.as_str()) {
                key.clone()
            } else if let Some(key) = a.references.iter().find_map(|r| {
                known
                    .get(r.as_str())
                    .filter(|(key, event)| same_event(a, key, event))
                    .map(|(key, _)| key.clone())
            }) {
                key
            } else if let Some((key, _)) = a
                .vtec_prefix
                .as_deref()
                .filter(|_| a.continues)
                .and_then(|p| {
                    by_number
                        .iter()
                        .find(|(key, event)| key.starts_with(p) && *event == a.event)
                })
            {
                key.clone()
            } else {
                a.event_key.clone()
            };
            known.insert(&a.id, (key.clone(), &a.event));
            keys.insert(a.id.clone(), key);
        }
        Ok(keys)
    }

    /// Changes are added to `out` only once the transaction has committed.
    async fn apply_feed(
        &self,
        st: &mut PollState,
        feed: &ParsedFeed,
        out: &mut Changes,
    ) -> anyhow::Result<()> {
        let ids: Vec<String> = feed.alerts.iter().map(|a| a.id.clone()).collect();
        // A message replaces the ones it references (a CON its NEW, a CAN
        // what it cancels). The API drops replaced messages, but not always
        // in the same response, and a cancelled warning must not linger.
        let superseded: HashSet<&str> = feed
            .alerts
            .iter()
            .flat_map(|a| a.references.iter().map(String::as_str))
            .collect();
        let lookup: Vec<&str> = ids
            .iter()
            .map(String::as_str)
            .chain(superseded.iter().copied())
            .collect();
        let stored: HashMap<String, StoredAlert> = sqlx::query_as::<_, (String, String, String, bool)>(
            "SELECT id, event_key, event, active AND geom IS NOT NULL FROM alerts WHERE id = ANY($1)",
        )
        .bind(&lookup)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(id, event_key, event, drawn)| (id, StoredAlert { event_key, event, drawn }))
        .collect();
        let keys = self.resolve_keys(feed, &stored).await?;
        let ended = |a: &FeedAlert| a.ended || superseded.contains(a.id.as_str());
        let changed: Vec<(&FeedAlert, Value)> = feed
            .alerts
            .iter()
            .filter(|a| !(stored.contains_key(&a.id) && st.known.get(&a.id) == Some(&a.hash)))
            .map(|a| (a, row(a, keys.get(&a.id).unwrap_or(&a.event_key), ended(a))))
            .collect();

        let mut ch = Changes::default();
        let mut tx = self.pool.begin().await?;
        let (written, rows) = upsert_all(&mut tx, &changed).await?;
        for (id, active, has_geom) in rows {
            let before = stored.get(&id).is_some_and(|s| s.drawn);
            ch.redrawn(id, before, active && has_geom);
        }
        sqlx::query("UPDATE alerts SET last_seen = now() WHERE id = ANY($1)")
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
        // Back in the feed after dropping out of it (the API is not always
        // consistent between its servers).
        let in_effect: Vec<&str> = feed
            .alerts
            .iter()
            .filter(|a| !ended(a))
            .map(|a| a.id.as_str())
            .collect();
        let back: Vec<(String, bool)> = sqlx::query_as(
            "UPDATE alerts SET active = true, ended_at = NULL
             WHERE id = ANY($1) AND NOT active
             RETURNING id, geom IS NOT NULL",
        )
        .bind(&in_effect)
        .fetch_all(&mut *tx)
        .await?;
        for (id, has_geom) in back {
            ch.redrawn(id, false, has_geom);
        }
        // Out of the feed, or replaced by a message that is in it. Only NWS
        // alerts: the world's come and go with their own feeds.
        let superseded: Vec<&str> = superseded.into_iter().collect();
        let gone: Vec<(String, bool)> = sqlx::query_as(
            "UPDATE alerts SET active = false, ended_at = now()
             WHERE active AND origin = 'nws' AND (NOT (id = ANY($1)) OR id = ANY($2))
             RETURNING id, geom IS NOT NULL",
        )
        .bind(&ids)
        .bind(&superseded)
        .fetch_all(&mut *tx)
        .await?;
        for (id, had_geom) in gone {
            ch.redrawn(id, had_geom, false);
        }
        tx.commit().await?;

        out.absorb(ch);
        st.known
            .extend(written.into_iter().map(|a| (a.id.clone(), a.hash)));
        let current: HashSet<&String> = ids.iter().collect();
        st.known.retain(|id, _| current.contains(id));
        Ok(())
    }

    /// End alerts whose time is up. A world alert whose sender gave no
    /// expiry is taken to be in effect for a day after it starts.
    pub(crate) async fn expire_ended(&self, ch: &mut Changes) -> anyhow::Result<()> {
        let ended: Vec<(String, bool, bool)> = sqlx::query_as(
            "UPDATE alerts SET active = false, ended_at = now()
             WHERE active
               AND (coalesce(ends, expires) <= now()
                    OR (origin = 'cap' AND expires IS NULL
                        AND coalesce(onset, effective, sent) + interval '24 hours' <= now()))
             RETURNING id, geom IS NOT NULL, origin = 'cap'",
        )
        .fetch_all(&self.pool)
        .await?;
        for (id, had_geom, world) in ended {
            if had_geom {
                ch.removed.insert(id);
            } else if world {
                ch.listed.insert(id);
            }
        }
        Ok(())
    }

    /// After zone boundaries arrive.
    pub(crate) async fn apply_new_zones(self: &Arc<Self>) -> anyhow::Result<()> {
        let mut ch = Changes::default();
        let outcome = {
            let _writes = self.writes.lock().await;
            shapes::assign(self, &mut ch).await
        };
        // Shapes assigned before a failure are committed: publish them.
        self.publish(ch).await;
        outcome
    }

    /// Notify, raise the revision and warm the default list, then tell
    /// browsers: what they fetch next must already be current. Returns
    /// notifications created.
    pub(crate) async fn publish(self: &Arc<Self>, mut ch: Changes) -> usize {
        ch.normalize();
        let mut created = 0;
        if !ch.to_match.is_empty() {
            let ids: Vec<String> = ch.to_match.iter().cloned().collect();
            match notify::match_places(self, Some(&ids), None).await {
                Ok(rows) => {
                    created = rows.len();
                    notify::dispatch(self, rows);
                }
                Err(e) => {
                    // The sweep re-runs the match on the next cycle.
                    self.force_sweep.store(true, Ordering::Release);
                    tracing::warn!("matching alerts to places failed: {e}");
                }
            }
        }
        if ch.visible() {
            self.cache.invalidate(&ch.updated);
            let revision = self.revision.bump(&self.pool).await;
            if let Err(e) = self
                .cache
                .list(&self.pool, revision, &ListQuery::default())
                .await
            {
                tracing::warn!("building /api/alerts failed: {e:#}");
            }
            self.hub.send(WsEvent::Alerts {
                revision,
                added: ch.added.into_iter().collect(),
                updated: ch.updated.into_iter().collect(),
                removed: ch.removed.into_iter().collect(),
            });
        }
        created
    }

    /// The sweep has not run for `SWEEP_EVERY`, or a failure asked for it.
    pub(crate) fn sweep_due(&self) -> bool {
        let forced = self.force_sweep.swap(false, Ordering::AcqRel);
        forced || crate::lock(&self.last_sweep).is_none_or(|t| t.elapsed() >= SWEEP_EVERY)
    }

    pub(crate) fn sweep_done(&self) {
        *crate::lock(&self.last_sweep) = Some(Instant::now());
    }

    /// The safety net: anything a failed cycle, a restart or a crash left
    /// undone is picked up here.
    pub(crate) async fn sweep(self: &Arc<Self>) -> anyhow::Result<usize> {
        let rows = notify::match_places(self, None, None).await?;
        let created = rows.len();
        notify::dispatch(self, rows);
        notify::redeliver_orphans(self).await?;
        // Devices that had nowhere to receive a warning and now do (one
        // that subscribed while its delivery was being decided, say).
        notify::requeue_waiting(self, None).await?;

        sqlx::query("DELETE FROM alerts WHERE NOT active AND ended_at < now() - interval '1 day'")
            .execute(&self.pool)
            .await?;
        // Keep a hazard's notification rows while any message about it is
        // still stored, so a long-lived event never notifies twice.
        sqlx::query(
            "DELETE FROM notifications n
             WHERE n.created_at < now() - interval '7 days'
               AND NOT EXISTS (SELECT 1 FROM alerts a WHERE a.event_key = n.event_key)",
        )
        .execute(&self.pool)
        .await?;
        // Zone boundaries are revised a couple of times a year. The attempt
        // is recorded up front, whatever comes of it: a retired zone answers
        // 404 for ever and must not keep its place at the head of the line.
        let stale: Vec<(String, String)> = sqlx::query_as(
            "WITH due AS (
                 SELECT id FROM zones
                 WHERE fetched_at < now() - interval '90 days'
                   AND (refresh_attempted_at IS NULL OR refresh_attempted_at < now() - interval '7 days')
                 ORDER BY coalesce(refresh_attempted_at, fetched_at)
                 LIMIT 50)
             UPDATE zones z SET refresh_attempted_at = now()
             FROM due WHERE z.id = due.id
             RETURNING z.id, z.kind",
        )
        .fetch_all(&self.pool)
        .await?;
        self.zones.request(
            stale
                .into_iter()
                .map(|(id, kind)| nws::ZoneRef { kind, id }),
            false,
        );
        crate::world::store::prune(&self.pool).await?;
        Ok(created)
    }
}

/// What is stored about an alert the feed names.
struct StoredAlert {
    event_key: String,
    event: String,
    /// Active and with a shape.
    drawn: bool,
}

/// The alert as a row of `alerts` (see [`FeedAlert`]).
fn row(a: &FeedAlert, event_key: &str, ended: bool) -> Value {
    let mut row = serde_json::to_value(a).expect("an alert is plain data");
    row["event_key"] = event_key.into();
    row["active"] = (!ended).into();
    row
}

/// Write the rows of `alerts`, all in one statement; if the database
/// refuses that, one by one, each in a savepoint, so that an alert it
/// refuses is skipped instead of failing the feed for ever. Returns the
/// alerts written, and `(id, active, has a shape)` of those whose content
/// was new.
async fn upsert_all<'a>(
    tx: &mut Transaction<'_, Postgres>,
    alerts: &[(&'a FeedAlert, Value)],
) -> Result<(Vec<&'a FeedAlert>, Vec<(String, bool, bool)>), sqlx::Error> {
    if alerts.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let all = Value::Array(alerts.iter().map(|(_, row)| row.clone()).collect());
    let mut sp = tx.begin().await?;
    match sqlx::query_as(UPSERT_SQL)
        .bind(&all)
        .fetch_all(&mut *sp)
        .await
    {
        Ok(rows) => {
            sp.commit().await?;
            return Ok((alerts.iter().map(|(a, _)| *a).collect(), rows));
        }
        Err(sqlx::Error::Database(_)) => sp.rollback().await?,
        Err(e) => return Err(e),
    }
    let (mut written, mut changed) = (Vec::new(), Vec::new());
    for (a, row) in alerts {
        let mut sp = tx.begin().await?;
        match sqlx::query_as(UPSERT_SQL)
            .bind(Value::Array(vec![row.clone()]))
            .fetch_all(&mut *sp)
            .await
        {
            Ok(rows) => {
                sp.commit().await?;
                written.push(*a);
                changed.extend(rows);
            }
            Err(sqlx::Error::Database(e)) => {
                sp.rollback().await?;
                tracing::warn!(alert = %a.id, "could not store alert: {e}");
            }
            Err(e) => return Err(e),
        }
    }
    Ok((written, changed))
}

macro_rules! nws_columns {
    () => {
        "id, event_key, event, severity, urgency, certainty, status, message_type, headline, description,
         instruction, area_desc, sender_name, sent, effective, onset, expires, ends, ugc, raw, active"
    };
}

/// `$1`: a JSON array of rows from [`row`]. Returns the rows whose content
/// changed. On conflict the stored `event_key` stays: notifications are
/// keyed on it.
const UPSERT_SQL: &str = concat!(
    "INSERT INTO alerts AS a (",
    nws_columns!(),
    ", ended_at, geom, geom_source)
     SELECT ",
    nws_columns!(),
    ", CASE WHEN active THEN NULL ELSE now() END,
            s.g, CASE WHEN s.g IS NULL THEN 'none' ELSE 'polygon' END
     FROM jsonb_array_elements($1) AS j(doc)
     CROSS JOIN LATERAL jsonb_populate_record(NULL::alerts, j.doc) AS r
     CROSS JOIN LATERAL (
         SELECT CASE WHEN g IS NULL OR ST_IsEmpty(g) THEN NULL ELSE g END AS g
         FROM (SELECT ST_Multi(ST_CollectionExtract(
                   ST_MakeValid(ST_SetSRID(ST_GeomFromGeoJSON(j.doc->>'polygon'), 4326)), 3)) AS g) p) s
     ON CONFLICT (id) DO UPDATE SET
         event = EXCLUDED.event,
         severity = EXCLUDED.severity, urgency = EXCLUDED.urgency,
         certainty = EXCLUDED.certainty, status = EXCLUDED.status,
         message_type = EXCLUDED.message_type, headline = EXCLUDED.headline,
         description = EXCLUDED.description, instruction = EXCLUDED.instruction,
         area_desc = EXCLUDED.area_desc, sender_name = EXCLUDED.sender_name,
         sent = EXCLUDED.sent, effective = EXCLUDED.effective, onset = EXCLUDED.onset,
         expires = EXCLUDED.expires, ends = EXCLUDED.ends, ugc = EXCLUDED.ugc,
         geom = CASE WHEN EXCLUDED.geom IS NOT NULL THEN EXCLUDED.geom
                     WHEN a.geom_source = 'zones' AND a.ugc = EXCLUDED.ugc THEN a.geom END,
         geom_source = CASE WHEN EXCLUDED.geom IS NOT NULL THEN 'polygon'
                            WHEN a.geom_source = 'zones' AND a.ugc = EXCLUDED.ugc THEN 'zones'
                            ELSE 'none' END,
         zones_missing = CASE WHEN EXCLUDED.geom IS NULL AND a.geom_source = 'zones' AND a.ugc = EXCLUDED.ugc
                              THEN a.zones_missing ELSE '{}' END,
         active = EXCLUDED.active,
         ended_at = CASE WHEN EXCLUDED.active THEN NULL ELSE coalesce(a.ended_at, now()) END,
         last_seen = now(),
         raw = EXCLUDED.raw
     WHERE a.raw IS DISTINCT FROM EXCLUDED.raw
     RETURNING a.id, a.active, a.geom IS NOT NULL"
);

pub(crate) async fn read_limited(
    mut resp: reqwest::Response,
    limit: usize,
) -> anyhow::Result<Vec<u8>> {
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        anyhow::bail!("response is larger than {limit} bytes");
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("reading response: {}", crate::push::transport_error(&e)))?
    {
        anyhow::ensure!(
            buf.len() + chunk.len() <= limit,
            "response is larger than {limit} bytes"
        );
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn removed_and_drawn_again_in_one_cycle_is_an_update() {
        let mut ch = Changes {
            added: set(&["z", "new"]),
            updated: set(&["kept"]),
            removed: set(&["z", "gone"]),
            to_match: set(&["new"]),
            listed: set(&[]),
        };
        ch.normalize();
        assert_eq!(ch.added, set(&["new"]));
        assert_eq!(ch.updated, set(&["kept", "z"]));
        assert_eq!(ch.removed, set(&["gone"]));
        assert_eq!(
            ch.to_match,
            set(&["new", "z"]),
            "its new shape is matched against places"
        );
    }
}
