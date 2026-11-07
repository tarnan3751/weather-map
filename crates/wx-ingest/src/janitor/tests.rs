use super::*;
use crate::driver::Payload;
use crate::engine::{Moment, PartJob, Produced};
use crate::testing::{self, stores::Gated};
use object_store::PutPayload;
use object_store::memory::InMemory;
use wx_core::{Hub, Insert, LAYERS, LonLatBox, TileCoord, layer};

use crate::testing::at as t;

const BOX: LonLatBox = LonLatBox::new(-125.0, 24.0, -66.0, 50.0);

async fn put_part(store: &dyn ObjectStore, layer: &str, id: &str) {
    for (z, x, y) in [(0, 0, 0), (1, 0, 0), (5, 7, 12)] {
        let key = keys::tile(layer, &[id], TileCoord::new(z, x, y).unwrap());
        store
            .put(&Path::from(key), PutPayload::from_static(b"WXT1"))
            .await
            .unwrap();
    }
    // A tile stacked over another part is filed under the top one.
    let stacked = keys::tile(
        layer,
        &[id, "gfs-26110606-360"],
        TileCoord::new(2, 1, 1).unwrap(),
    );
    store
        .put(&Path::from(stacked), PutPayload::from_static(b"WXT1"))
        .await
        .unwrap();
    store
        .put(
            &Path::from(keys::grid(layer, id)),
            PutPayload::from_static(b"grid"),
        )
        .await
        .unwrap();
}

fn observed(at: DateTime<Utc>) -> Part {
    Part::observed("mrms", at, BOX, Some(BOX), 7)
}

fn forecast(source: &str, run: DateTime<Utc>, lead: u32) -> Part {
    Part::forecast(source, run, lead, BOX, None, 6)
}

#[tokio::test]
async fn dropped_parts_are_deleted_after_the_grace_period() {
    let store = InMemory::new();
    let catalog = FrameCatalog::new(Hub::new());
    let deletions = Arc::new(Deletions::default());
    let now = t(12, 0);
    let (old, fresh) = (observed(t(9, 0)), observed(t(11, 50)));
    put_part(&store, "radar", &old.id).await;
    put_part(&store, "radar", &fresh.id).await;
    // Taken while still in the window, then expired by the sweep.
    catalog.insert("radar", old.clone(), t(9, 1)).unwrap();
    catalog.insert("radar", fresh.clone(), t(11, 51)).unwrap();
    let (dropped, deleted) = pass(&store, &catalog, &deletions, now).await.unwrap();
    assert_eq!(
        (dropped, deleted),
        (1, 0),
        "inside the grace period nothing goes"
    );
    assert_eq!(deletions.pending(), 1);
    assert_eq!(testing::keys(&store).await.len(), 10);

    let (_, deleted) = pass(&store, &catalog, &deletions, now + DELETE_GRACE)
        .await
        .unwrap();
    assert_eq!(deleted, 1);
    let left = testing::keys(&store).await;
    assert_eq!(left.len(), 5);
    assert!(left.iter().all(|k| k.contains(&fresh.id)), "{left:?}");
    assert_eq!(deletions.pending(), 0);
}

#[tokio::test]
async fn replaced_parts_are_deleted_but_live_and_rebuilt_ones_are_not() {
    let store = InMemory::new();
    let catalog = FrameCatalog::new(Hub::new());
    let deletions = Arc::new(Deletions::default());
    let now = t(12, 0);
    // The 11Z run's part for 18Z replaces the 10Z run's.
    let older = forecast("hrrr", t(10, 0), 480);
    let newer = forecast("hrrr", t(11, 0), 420);
    put_part(&store, "temp", &older.id).await;
    put_part(&store, "temp", &newer.id).await;
    catalog.insert("temp", older.clone(), now).unwrap();
    deletions.mark_live("temp", &older.id, now);
    let Insert::Added { replaced: Some(r) } = catalog.insert("temp", newer.clone(), now).unwrap()
    else {
        panic!("the newer run replaces the older");
    };
    assert_eq!(r.id, older.id);
    deletions.mark_live("temp", &newer.id, now);
    // A part queued by mistake while it is live survives.
    deletions.schedule("temp", &newer.id, now);
    // So does one a task is rebuilding.
    let rebuilt = "hrrr-26110611-180";
    let guard = deletions.protect("temp", rebuilt).unwrap();
    deletions.schedule("temp", rebuilt, now);
    put_part(&store, "temp", rebuilt).await;

    let (dropped, _) = pass(&store, &catalog, &deletions, now).await.unwrap();
    assert_eq!(dropped, 1);
    let (_, deleted) = pass(&store, &catalog, &deletions, now + DELETE_GRACE)
        .await
        .unwrap();
    assert_eq!(deleted, 1);
    let left = testing::keys(&store).await;
    assert!(!left.iter().any(|k| k.contains(&older.id)), "{left:?}");
    assert!(left.iter().any(|k| k.contains(&newer.id)));
    assert!(left.iter().any(|k| k.contains(rebuilt)));
    assert_eq!(deletions.pending(), 1, "the rebuilt part's deletion waits");
    drop(guard);
    assert!(!deletions.is_protected("temp", rebuilt));
}

#[tokio::test]
async fn reconcile_deletes_old_orphans_only() {
    let store = testing::stores::Counting::default();
    let catalog = FrameCatalog::new(Hub::new());
    let deletions = Arc::new(Deletions::default());
    let live = observed(t(11, 58));
    catalog.insert("radar", live.clone(), t(11, 59)).unwrap();
    put_part(&store, "radar", &live.id).await;
    // Left by a crash: tiles only, no grid.
    for (z, x, y) in [(0, 0, 0), (3, 1, 2)] {
        let key = keys::tile(
            "radar",
            &["mrms-2611061000"],
            TileCoord::new(z, x, y).unwrap(),
        );
        store
            .put(&Path::from(key), PutPayload::from_static(b"x"))
            .await
            .unwrap();
    }
    // A part the catalog refused: a grid alone.
    store
        .put(
            &Path::from(keys::grid("temp", "gfs-26110600-1440")),
            PutPayload::from_static(b"g"),
        )
        .await
        .unwrap();
    // A part being built right now.
    let _guard = deletions.protect("wind", "hrrr-26110611-60").unwrap();
    put_part(&store, "wind", "hrrr-26110611-60").await;
    // Something that is not ours.
    store
        .put(
            &Path::from("tiles/radar/README.TXT/x"),
            PutPayload::from_static(b"x"),
        )
        .await
        .unwrap();

    // Objects were written "now" by the in-memory store: too young. Only
    // the tiles-only part's tiles are listed to tell: a grid shows its part
    // young, and the parts held or in flight are not looked at.
    let now = Utc::now();
    store.take_lists();
    assert!(
        reconcile(&store, &catalog, &deletions, LAYERS, now, DELETE_GRACE)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.take_lists(), 2 * LAYERS.len() + 1);

    let later = now + DELETE_GRACE + Duration::seconds(1);
    let mut removed = reconcile(&store, &catalog, &deletions, LAYERS, later, DELETE_GRACE)
        .await
        .unwrap();
    removed.sort();
    assert_eq!(
        removed,
        vec![
            key("radar", "mrms-2611061000"),
            key("temp", "gfs-26110600-1440")
        ]
    );
    // Again the tiles-only part's tiles, then each deletion's.
    assert_eq!(store.take_lists(), 2 * LAYERS.len() + 1 + 2);
    let left = testing::keys(&store).await;
    assert!(
        left.iter().any(|k| k.contains(&live.id)),
        "the catalog holds it"
    );
    assert!(
        left.iter().any(|k| k.contains("hrrr-26110611-60")),
        "being built"
    );
    assert!(left.iter().any(|k| k.contains("README")), "not ours");
    assert!(
        !left
            .iter()
            .any(|k| k.contains("2611061000") || k.contains("1440"))
    );
}

#[tokio::test]
async fn a_deletion_scheduled_while_protected_still_happens() {
    let store = InMemory::new();
    let deletions = Arc::new(Deletions::default());
    let id = "hrrr-26110611-180";
    put_part(&store, "temp", id).await;
    let guard = deletions.protect("temp", id).unwrap();
    let now = t(12, 0);
    deletions.schedule("temp", id, now);
    assert_eq!(delete_due(&store, &deletions, now).await.unwrap(), 0);
    assert_eq!(deletions.pending(), 1, "still queued, not forgotten");
    drop(guard);
    assert_eq!(delete_due(&store, &deletions, now).await.unwrap(), 1);
    assert!(testing::keys(&store).await.is_empty());
}

#[tokio::test]
async fn a_part_being_deleted_cannot_be_protected() {
    let deletions = Arc::new(Deletions::default());
    let held = HashSet::new();
    let id = "gfs-26110606-1620";
    let claim = deletions.claim_orphan(key("temp", id), &held).unwrap();
    assert!(deletions.is_deleting("temp", id));
    assert!(deletions.protect("temp", id).is_none());
    assert!(deletions.claim_orphan(key("temp", id), &held).is_none());
    drop(claim);
    let guard = deletions.protect("temp", id).unwrap();
    // And a part in flight cannot be claimed.
    assert!(deletions.claim_orphan(key("temp", id), &held).is_none());
    deletions.schedule("temp", id, t(0, 0));
    assert!(deletions.claim_due(t(12, 0)).is_none());
    drop(guard);
    assert!(deletions.claim_due(t(12, 0)).is_some());
    // A part the catalog holds is never an orphan.
    let held: Held = [("temp", "x")].into();
    assert!(deletions.claim_orphan(key("temp", "x"), &held).is_none());
}

#[tokio::test]
async fn deleting_a_missing_part_is_fine() {
    let store = InMemory::new();
    assert_eq!(
        delete_part(&store, "radar", "mrms-2611060000")
            .await
            .unwrap(),
        0
    );
}

#[test]
fn tiles_outlive_their_serving_window() {
    assert!(DELETE_GRACE.num_seconds() > SERVE_GRACE_SECONDS);
}

/// A rebuild of a part whose old objects are being deleted waits for the
/// delete, so it never loses its new objects to it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rebuild_never_overlaps_the_delete_of_its_old_objects() {
    let inner = Arc::new(InMemory::new());
    let gate = Arc::new(Gated::new(inner.clone()));
    let mem = testing::remote(&[]);
    let engine = testing::engine_with(&mem, gate.clone(), LAYERS, wx_core::SOURCES);
    let temp = layer::layer("temp").unwrap();
    let hrrr = crate::testing::source("hrrr");
    let run = crate::clock::floor(Utc::now(), Duration::hours(1));
    let job = PartJob::forecast(
        temp,
        temp.input_for(hrrr).unwrap(),
        hrrr,
        run,
        60,
        Some(0),
        Default::default(),
    )
    .unwrap();
    let id = job.id.clone();
    // A refused build left objects behind and queued their deletion.
    put_part(inner.as_ref(), "temp", &id).await;
    let now = Utc::now();
    engine.deletions.schedule("temp", &id, now);
    let janitor = {
        let (store, deletions) = (gate.clone(), engine.deletions.clone());
        tokio::spawn(async move { delete_due(store.as_ref(), &deletions, now).await })
    };
    // The delete has listed the old tiles and is about to remove them.
    gate.entered.notified().await;
    let tmp = Payload::Grib(testing::fixture("hrrr_t10z_f06__TMP_2mabove.grib2").into());
    let first = engine
        .produce(
            &job,
            vec![tmp.clone()],
            "test",
            Moment::starting(Utc::now()),
        )
        .await;
    gate.release.add_permits(1);
    assert_eq!(janitor.await.unwrap().unwrap(), 1);
    assert!(
        matches!(first, Ok(Produced::Busy)),
        "refused while its old objects go"
    );
    assert!(engine.wanted(&job, Utc::now()));
    let second = engine
        .produce(&job, vec![tmp], "test", Moment::starting(Utc::now()))
        .await;
    assert!(matches!(second, Ok(Produced::Inserted)), "{second:?}");
    assert!(
        testing::keys(inner.as_ref())
            .await
            .contains(&keys::grid("temp", &id))
    );
}
