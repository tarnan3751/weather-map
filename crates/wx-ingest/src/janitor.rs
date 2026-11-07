//! Cleaning up after parts. A part owns its grid (`grids/{layer}/{part}.wxg`)
//! and every tile filed under it (`tiles/{layer}/{part}/`, written by the
//! tile server on demand). When the catalog drops a part (expired, replaced
//! by a newer run, left behind by an anchor) its objects are deleted
//! [`DELETE_GRACE`] later; anything in the store that belongs to no part the
//! catalog holds (left by a crash, or by a build the catalog refused) is
//! found and deleted too.
//!
//! Deciding that a part's objects may go and claiming them happen in one
//! step under the [`Deletions`] lock, and a claimed part cannot be protected
//! (rebuilt) until its delete has finished, so a part never lands on the
//! catalog with objects a delete was still removing.

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use futures::{StreamExt, TryStreamExt};
use object_store::path::Path;
use object_store::{ObjectStore, ObjectStoreExt};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use wx_core::frame::SERVE_GRACE_SECONDS;
use wx_core::{FeedCtx, FrameCatalog, LayerSpec, Part, keys};

/// How long a dropped part's objects outlive it, so a browser that was
/// animating through it does not hit missing tiles. The tile server keeps
/// serving a part for `SERVE_GRACE_SECONDS` after it leaves its timeline;
/// this must stay longer than that.
pub const DELETE_GRACE: Duration = Duration::minutes(5);
const _: () = assert!(DELETE_GRACE.num_seconds() > SERVE_GRACE_SECONDS);

const SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(60);
const RECONCILE_EVERY: Duration = Duration::minutes(10);

/// `(layer, part id)`.
pub type PartKey = (String, String);

fn key(layer: &str, id: &str) -> PartKey {
    (layer.to_string(), id.to_string())
}

/// The catalog's parts as `(layer, part id)`, taken once per sweep.
type Held<'a> = HashSet<(&'a str, &'a str)>;

#[derive(Default)]
struct State {
    /// Parts waiting to be deleted, and when.
    queue: BTreeMap<PartKey, DateTime<Utc>>,
    /// Parts being built: their grid may be in the store before they are on
    /// the catalog.
    protected: HashMap<PartKey, usize>,
    /// Parts whose objects are being deleted right now. They cannot be
    /// protected until the delete finishes.
    deleting: HashSet<PartKey>,
    /// Parts this process put on the catalog that it has not seen dropped,
    /// and when.
    live: HashMap<PartKey, DateTime<Utc>>,
}

impl State {
    /// Held by the catalog (in `held`, a snapshot of it: a part put on the
    /// catalog since has objects too young to be judged anyway), live, in
    /// flight, queued or being deleted.
    fn busy(&self, key: &PartKey, held: &Held) -> bool {
        held.contains(&(key.0.as_str(), key.1.as_str()))
            || self.live.contains_key(key)
            || self.protected.contains_key(key)
            || self.queue.contains_key(key)
            || self.deleting.contains(key)
    }
}

/// Shared by the tasks (which protect parts in flight and mark the ones
/// they insert) and the janitor (which carries out deletions).
#[derive(Default)]
pub struct Deletions {
    state: Mutex<State>,
}

/// Keeps a part's objects safe from the janitor while it lives.
pub struct Protect {
    owner: Arc<Deletions>,
    key: PartKey,
}

impl Drop for Protect {
    fn drop(&mut self) {
        let mut s = self.owner.lock();
        if let Some(n) = s.protected.get_mut(&self.key) {
            *n -= 1;
            if *n == 0 {
                s.protected.remove(&self.key);
            }
        }
    }
}

/// Held while a part's objects are being deleted.
pub struct Claim {
    owner: Arc<Deletions>,
    key: PartKey,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.owner.lock().deleting.remove(&self.key);
    }
}

impl Deletions {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // The state stays consistent even if a holder panicked mid-update.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Delete the part's objects at `due` (or later, if it is already queued
    /// for later).
    pub fn schedule(&self, layer: &str, id: &str, due: DateTime<Utc>) {
        let mut s = self.lock();
        let at = s.queue.entry(key(layer, id)).or_insert(due);
        *at = (*at).max(due);
    }

    /// The catalog dropped this part: it is no longer live, and its objects
    /// go at `due`.
    pub fn dropped(&self, layer: &str, id: &str, due: DateTime<Utc>) {
        self.lock().live.remove(&key(layer, id));
        self.schedule(layer, id, due);
    }

    /// Forget live marks older than `before` for parts the catalog no longer
    /// holds (their drop went unseen), so they cannot keep a deletion back.
    fn forget_stale(&self, held: &Held, before: DateTime<Utc>) {
        self.lock()
            .live
            .retain(|k, at| *at >= before || held.contains(&(k.0.as_str(), k.1.as_str())));
    }

    /// Mark a part as being built. `None` while its objects are being
    /// deleted: the caller skips it this time.
    pub fn protect(self: &Arc<Self>, layer: &str, id: &str) -> Option<Protect> {
        let key = key(layer, id);
        let mut s = self.lock();
        if s.deleting.contains(&key) {
            return None;
        }
        *s.protected.entry(key.clone()).or_default() += 1;
        Some(Protect {
            owner: self.clone(),
            key,
        })
    }

    pub fn mark_live(&self, layer: &str, id: &str, at: DateTime<Utc>) {
        self.lock().live.insert(key(layer, id), at);
    }

    pub fn unmark_live(&self, layer: &str, id: &str) {
        self.lock().live.remove(&key(layer, id));
    }

    pub fn is_deleting(&self, layer: &str, id: &str) -> bool {
        let s = self.lock();
        !s.deleting.is_empty() && s.deleting.contains(&key(layer, id))
    }

    #[cfg(test)]
    pub fn is_protected(&self, layer: &str, id: &str) -> bool {
        self.lock().protected.contains_key(&key(layer, id))
    }

    /// Claim the next due deletion that is safe to carry out, checking and
    /// claiming in one step. A part being rebuilt stays queued for a later
    /// pass (if the rebuild fails its objects must still go); one that is
    /// live again leaves the queue.
    fn claim_due(self: &Arc<Self>, now: DateTime<Utc>) -> Option<Claim> {
        let mut s = self.lock();
        let due: Vec<PartKey> = s
            .queue
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(k, _)| k.clone())
            .collect();
        for key in due {
            if s.protected.contains_key(&key) || s.deleting.contains(&key) {
                continue;
            }
            s.queue.remove(&key);
            if s.live.contains_key(&key) {
                continue;
            }
            s.deleting.insert(key.clone());
            return Some(Claim {
                owner: self.clone(),
                key,
            });
        }
        None
    }

    /// Whether the orphan sweep must leave this part alone.
    fn in_use(&self, key: &PartKey, held: &Held) -> bool {
        self.lock().busy(key, held)
    }

    /// Claim an orphan for deletion, re-checking [`Deletions::in_use`]
    /// under the lock it claims with.
    fn claim_orphan(self: &Arc<Self>, key: PartKey, held: &Held) -> Option<Claim> {
        let mut s = self.lock();
        if s.busy(&key, held) {
            return None;
        }
        s.deleting.insert(key.clone());
        Some(Claim {
            owner: self.clone(),
            key,
        })
    }

    pub fn pending(&self) -> usize {
        self.lock().queue.len()
    }
}

/// Remove a part's tiles and grid. Missing objects are fine. Returns how
/// many tiles there were.
pub async fn delete_part(store: &dyn ObjectStore, layer: &str, id: &str) -> anyhow::Result<usize> {
    let prefix = Path::from(keys::part_tile_prefix(layer, id));
    let tiles: Vec<Path> = store
        .list(Some(&prefix))
        .map_ok(|m| m.location)
        .try_collect()
        .await
        .with_context(|| format!("listing {prefix}"))?;
    let count = tiles.len();
    let mut results = store.delete_stream(futures::stream::iter(tiles.into_iter().map(Ok)).boxed());
    while let Some(r) = results.next().await {
        match r {
            Ok(_) | Err(object_store::Error::NotFound { .. }) => {}
            Err(e) => {
                return Err(
                    anyhow::Error::new(e).context(format!("deleting tiles of {layer}/{id}"))
                );
            }
        }
    }
    let grid = Path::from(keys::grid(layer, id));
    match store.delete(&grid).await {
        Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(count),
        Err(e) => Err(anyhow::Error::new(e).context(format!("deleting {grid}"))),
    }
}

/// Carry out due deletions. A part that is live again is left alone; one
/// being rebuilt waits for a later pass. Failed deletions are retried a
/// minute later. Returns how many parts' objects went.
pub async fn delete_due(
    store: &dyn ObjectStore,
    deletions: &Arc<Deletions>,
    now: DateTime<Utc>,
) -> anyhow::Result<usize> {
    let mut deleted = 0;
    let mut first_error = None;
    while let Some(claim) = deletions.claim_due(now) {
        let (layer, id) = &claim.key;
        match delete_part(store, layer, id).await {
            Ok(_) => deleted += 1,
            Err(e) => {
                deletions.schedule(layer, id, now + Duration::minutes(1));
                first_error.get_or_insert(e);
            }
        }
        drop(claim);
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(deleted),
    }
}

/// Find parts that have objects in the store but are not held by the
/// catalog, not live, not in flight and not already queued, whose newest
/// object is older than `grace`, and delete them. Looks at `grids/{layer}/`
/// and the first level of `tiles/{layer}/` of every layer in `layers`; a
/// part's tiles are listed only when its grid does not already show it
/// young and it has a tile directory. Returns `(layer, part id)` of each.
pub async fn reconcile(
    store: &dyn ObjectStore,
    catalog: &FrameCatalog,
    deletions: &Arc<Deletions>,
    layers: &[LayerSpec],
    now: DateTime<Utc>,
    grace: Duration,
) -> anyhow::Result<Vec<PartKey>> {
    // Taken before listing: a part inserted after it has a grid younger than
    // `grace`, or is live.
    let parts = catalog.parts();
    let held: Held = parts
        .iter()
        .map(|(layer, p)| (*layer, p.id.as_str()))
        .collect();
    deletions.forget_stale(&held, now - grace);
    let young = |t: DateTime<Utc>| t > now - grace;
    let newer =
        |t: &mut Option<DateTime<Utc>>, u: DateTime<Utc>| *t = Some(t.map_or(u, |t| t.max(u)));
    let mut removed = Vec::new();
    for layer in layers {
        let tiles = store
            .list_with_delimiter(Some(&Path::from(keys::layer_tile_prefix(layer.id))))
            .await
            .with_context(|| format!("listing tiles of {}", layer.id))?;
        let grids = store
            .list_with_delimiter(Some(&Path::from(keys::layer_grid_prefix(layer.id))))
            .await
            .with_context(|| format!("listing grids of {}", layer.id))?;
        // Each part with objects here: its grid's time, and whether it has
        // tiles.
        let mut found: BTreeMap<&str, (Option<DateTime<Utc>>, bool)> = BTreeMap::new();
        for p in &tiles.common_prefixes {
            if let Some(id) = p.filename() {
                found.entry(id).or_default().1 = true;
            }
        }
        for g in &grids.objects {
            if let Some(id) = g.location.filename().and_then(|f| f.strip_suffix(".wxg")) {
                newer(&mut found.entry(id).or_default().0, g.last_modified);
            }
        }
        for (id, (mut latest, has_tiles)) in found {
            // Only touch what this crate names; anything else is not ours.
            if !Part::is_valid_id(id) || held.contains(&(layer.id, id)) || latest.is_some_and(young)
            {
                continue;
            }
            let k = key(layer.id, id);
            if deletions.in_use(&k, &held) {
                continue;
            }
            if has_tiles {
                let prefix = Path::from(keys::part_tile_prefix(layer.id, id));
                let mut listed = store.list(Some(&prefix));
                while let Some(m) = listed
                    .try_next()
                    .await
                    .with_context(|| format!("listing {prefix}"))?
                {
                    newer(&mut latest, m.last_modified);
                }
            }
            let Some(latest) = latest else {
                continue; // an empty directory, nothing to delete
            };
            if young(latest) {
                continue;
            }
            // A task may have picked the part up while we listed; the claim
            // re-checks, and keeps it from doing so while the delete runs.
            let Some(claim) = deletions.claim_orphan(k.clone(), &held) else {
                continue;
            };
            delete_part(store, layer.id, id).await?;
            drop(claim);
            removed.push(k);
        }
    }
    Ok(removed)
}

/// One janitor pass: sweep the catalog, queue what it dropped, delete what
/// is due. Returns `(dropped, deleted)`.
pub async fn pass(
    store: &dyn ObjectStore,
    catalog: &FrameCatalog,
    deletions: &Arc<Deletions>,
    now: DateTime<Utc>,
) -> anyhow::Result<(usize, usize)> {
    catalog.sweep(now);
    let dropped = catalog.take_dropped();
    for d in &dropped {
        deletions.dropped(d.layer, &d.part.id, d.at + DELETE_GRACE);
    }
    let deleted = delete_due(store, deletions, now).await?;
    Ok((dropped.len(), deleted))
}

/// The supervised `janitor` task.
pub async fn run(
    store: Arc<dyn ObjectStore>,
    catalog: Arc<FrameCatalog>,
    deletions: Arc<Deletions>,
    layers: &'static [LayerSpec],
    ctx: FeedCtx,
) -> anyhow::Result<()> {
    // Let restored catalogs and parts in flight settle before judging
    // anything an orphan.
    let mut next_reconcile = Utc::now() + DELETE_GRACE;
    loop {
        let now = Utc::now();
        let (dropped, deleted) = pass(store.as_ref(), &catalog, &deletions, now).await?;
        let mut orphans = 0;
        if now >= next_reconcile {
            let found = reconcile(
                store.as_ref(),
                &catalog,
                &deletions,
                layers,
                now,
                DELETE_GRACE,
            )
            .await?;
            for (layer, id) in &found {
                tracing::info!(layer, part = id, "deleted an orphaned part");
            }
            orphans = found.len();
            next_reconcile = now + RECONCILE_EVERY;
        }
        if dropped > 0 || deleted > 0 || orphans > 0 {
            tracing::debug!(dropped, deleted, orphans, "janitor pass");
        }
        ctx.ok(
            None,
            format!(
                "{dropped} dropped, {deleted} deleted, {orphans} orphans, {} waiting",
                deletions.pending()
            ),
        );
        crate::clock::pause(&ctx, SWEEP_EVERY).await;
    }
}

#[cfg(test)]
mod tests;
