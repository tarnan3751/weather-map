use super::*;
use crate::remote::memory::MemoryRemote;
use object_store::memory::InMemory;
use wx_core::{FeedState, Hub};

fn ingest(feeds: &[&str]) -> anyhow::Result<Ingest> {
    let store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
    let catalog = Arc::new(FrameCatalog::new(Hub::new()));
    let config = IngestConfig {
        feeds: feeds.iter().map(|s| s.to_string()).collect(),
        ..IngestConfig::default()
    };
    Ingest::new(store, catalog, config)
}

#[test]
fn new_does_no_io_and_names_tasks() {
    let ingest = ingest(&["hrrr", "mrms", "hrrr"]).unwrap();
    let names = ingest.task_names();
    // Sources in registry order; one task per MRMS product, named after the
    // product's first layer; one per HRRR region; the janitor last.
    assert_eq!(
        &names[..5],
        [
            "mrms.radar",
            "mrms.rain_1h",
            "mrms.rain_24h",
            "mrms.rain_72h",
            "mrms_ak.radar"
        ]
    );
    assert!(names.contains(&"mrms_guam.radar".to_string()));
    let hrrr = names.iter().position(|n| n == "hrrr").unwrap();
    assert_eq!(names[hrrr + 1], "hrrr_ak");
    assert_eq!(names.last().unwrap(), "janitor");
    assert!(ingest.traffic().is_empty());
}

#[test]
fn a_selection_names_families_or_sources() {
    let names = ingest(&["mrms_ak", "gfs", "rtma"]).unwrap().task_names();
    assert!(names.iter().all(|n| !n.starts_with("mrms.")), "{names:?}");
    assert!(names.contains(&"mrms_ak.rain_72h".to_string()));
    // RTMA's one product is one task per region, named after the source.
    for id in ["rtma", "rtma_ak", "rtma_hi", "rtma_pr", "rtma_gu", "gfs"] {
        assert!(names.contains(&id.to_string()), "{id} in {names:?}");
    }
    assert!(
        ingest(&["nexrad"])
            .unwrap_err()
            .to_string()
            .contains("unknown")
    );
    assert_eq!(ingest(&[]).unwrap().task_names(), ["janitor"]);
    assert!(
        IngestConfig::is_known("mrms_hi")
            && IngestConfig::is_known("icon")
            && !IngestConfig::is_known("alerts")
    );
}

/// Every enabled family with an adapter runs by default; the others are
/// refused when named.
#[test]
fn the_default_is_every_enabled_family_with_an_adapter() {
    let supported = IngestConfig::supported();
    assert_eq!(IngestConfig::default().feeds, supported);
    for f in ["mrms", "rtma", "hrrr", "gfs"] {
        assert!(supported.contains(&f.to_string()), "{f}");
    }
    let base = IngestConfig::base();
    for s in SOURCES {
        let name = s.family.as_str();
        let made = ingest(&[name]);
        match (s.family.enabled(), families::adapter(s, &base).is_some()) {
            (true, true) => assert!(made.is_ok(), "{name}: {:?}", made.err()),
            (false, _) => assert!(
                made.unwrap_err().to_string().contains("not enabled"),
                "{name}"
            ),
            (true, false) => assert!(
                made.unwrap_err().to_string().contains("no adapter"),
                "{name}"
            ),
        }
    }
    let all = ingest(&supported.iter().map(String::as_str).collect::<Vec<_>>()).unwrap();
    assert!(all.task_names().len() > 20);
}

#[tokio::test]
async fn spawn_registers_and_stops() {
    let mem = MemoryRemote::new();
    let store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
    let catalog = Arc::new(FrameCatalog::new(Hub::new()));
    let config = IngestConfig {
        feeds: vec!["gfs".into()],
        ..IngestConfig::default()
    };
    let ingest = Ingest::with_remote(store, catalog, config, testing::quick_remote(&mem)).unwrap();
    let health = HealthRegistry::new(Hub::new());
    let cancel = CancellationToken::new();
    let handles = ingest.spawn(&health, cancel.clone());
    assert_eq!(handles.len(), 2);
    assert!(health.get("gfs").is_some());
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(health.get("janitor").unwrap().state, FeedState::Healthy);
    cancel.cancel();
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(health.get("janitor").unwrap().state, FeedState::Stopped);
    assert_eq!(health.get("gfs").unwrap().state, FeedState::Stopped);
    // It looked for runs.
    assert!(
        mem.requests()
            .iter()
            .any(|r| r.contains("noaa-gfs-bdp-pds") && r.contains("list-type=2"))
    );
}
