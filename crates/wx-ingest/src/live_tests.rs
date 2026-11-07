//! Checks against NOAA's real buckets. They need the network, so they are
//! ignored by default: `cargo test -p wx-ingest -- --ignored live`.

use crate::driver::{Cx, Fetched, Model, ModelFile, Need, Observed, Payload};
use crate::families::{gfs, hrrr, mrms, rtma};
use crate::remote::S3Bucket;
use chrono::{DateTime, Duration, Utc};
use wx_core::{Family, LAYERS, SourceDef, source};

/// Every need of `source`'s inputs on `product` at `lead`.
fn needs(source: &SourceDef, product: &str, lead: u32) -> Vec<Need> {
    crate::driver::inputs_of(LAYERS, source)
        .into_iter()
        .filter(|(_, i)| i.product == product)
        .flat_map(|(_, i)| {
            i.fields.iter().map(move |f| Need {
                input: i,
                field: Some(crate::driver::fetched_as(*f, Default::default())),
                lead,
            })
        })
        .collect()
}

/// Each payload is one whole GRIB message. Returns how many there were.
fn check_messages(what: &str, fetched: Fetched, needs: &[Need]) -> usize {
    let Fetched::Got(payloads) = fetched else {
        panic!("{what}: missing");
    };
    let mut n = 0;
    for (p, need) in payloads.iter().zip(needs) {
        match p {
            Some(Payload::Grib(b)) => {
                assert!(
                    b.starts_with(b"GRIB") && b.ends_with(b"7777"),
                    "{what}: {:?} is not one message",
                    need.field
                );
                n += 1;
            }
            Some(other) => panic!("{what}: {other:?}"),
            None => eprintln!("{what}: no {:?}", need.field.map(|f| (f.var, f.level))),
        }
    }
    n
}

#[tokio::test]
#[ignore = "network"]
async fn live_mrms_every_domain_has_fresh_files() {
    let remote = crate::testing::live::remote();
    let now = Utc::now();
    let cx = Cx::new(&remote, "live", now);
    let m = mrms::Mrms;
    let product = "MergedReflectivityQCComposite_00.50";
    for s in wx_core::source::sources_of(Family::Mrms) {
        let files = m
            .list(&cx, s, product, now - Duration::hours(1))
            .await
            .unwrap();
        let newest = files
            .iter()
            .max_by_key(|f| f.valid)
            .unwrap_or_else(|| panic!("{}: nothing in an hour", s.id));
        let age = now - newest.valid;
        eprintln!(
            "{}: {} files, newest {} ({}s old)",
            s.id,
            files.len(),
            newest.valid,
            age.num_seconds()
        );
        assert!(age < Duration::minutes(15), "{} is {age} old", s.id);
        let head = cx
            .get(S3Bucket::aws(mrms::BUCKET).url(&newest.name))
            .range(crate::idx::ByteRange::new(0, 2))
            .bytes()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&head[..], &[0x1f, 0x8b], "{} is gzip", s.id);
    }
    eprintln!("{:?}", remote.traffic());
}

/// The newest of `runs` (newest first) whose listing has the file `file`
/// names, with its size.
async fn newest_with(
    cx: &Cx<'_>,
    model: &dyn Model,
    source: &SourceDef,
    products: &[&'static str],
    runs: &[DateTime<Utc>],
    file: impl Fn(DateTime<Utc>) -> String,
) -> (DateTime<Utc>, String, Option<u64>) {
    for &run in runs {
        let listing = model
            .list(cx, source, run, products)
            .await
            .unwrap()
            .unwrap();
        let name = file(run);
        if let Some(size) = listing.size(&name) {
            return (run, name, size);
        }
    }
    panic!("{}: no run has {}", source.id, file(runs[0]));
}

#[tokio::test]
#[ignore = "network"]
async fn live_hrrr_ranges_are_grib_messages() {
    let remote = crate::testing::live::remote();
    let now = Utc::now();
    let cx = Cx::new(&remote, "live", now);
    let h = hrrr::Hrrr::new(18);
    for id in ["hrrr", "hrrr_ak"] {
        let s = source::source(id).unwrap();
        let runs = h.cycle(s).runs(now, Duration::hours(6));
        for (product, lead) in [
            (wx_core::families::hrrr::SURFACE, 60),
            (wx_core::families::hrrr::SURFACE, 360),
            (hrrr::SUBHOURLY, 105),
        ] {
            let needs = needs(s, product, lead);
            let probe = needs[0];
            let (run, name, size) = newest_with(
                &cx,
                &h,
                s,
                &[wx_core::families::hrrr::SURFACE, hrrr::SUBHOURLY],
                &runs,
                |r| h.file(s, r, &probe),
            )
            .await;
            let file = ModelFile {
                name: name.clone(),
                size,
            };
            let fetched = h.fetch(&cx, s, run, &file, &needs).await.unwrap();
            let n = check_messages(&name, fetched, &needs);
            eprintln!("{name}: {n} of {} needs", needs.len());
            assert!(n > 0);
        }
    }
}

#[tokio::test]
#[ignore = "network"]
async fn live_rtma_ranges_are_grib_messages() {
    let remote = crate::testing::live::remote();
    let now = Utc::now();
    let cx = Cx::new(&remote, "live", now);
    let r = rtma::Rtma;
    let product = wx_core::families::rtma::PRODUCT;
    for s in wx_core::source::sources_of(Family::Rtma) {
        let files = r
            .list(&cx, s, product, now - Duration::hours(4))
            .await
            .unwrap();
        let newest = files
            .iter()
            .max_by_key(|f| f.valid)
            .unwrap_or_else(|| panic!("{}: nothing in 4 hours", s.id));
        let needs = needs(s, product, 0);
        let fetched = r.fetch(&cx, s, product, newest, &needs).await.unwrap();
        let n = check_messages(&newest.name, fetched, &needs);
        eprintln!(
            "{}: {} at {}, {n} of {} needs",
            s.id,
            newest.name,
            newest.valid,
            needs.len()
        );
        assert!(n > 0);
    }
}

#[tokio::test]
#[ignore = "network"]
async fn live_gfs_ranges_are_grib_messages() {
    let remote = crate::testing::live::remote();
    let now = Utc::now();
    let cx = Cx::new(&remote, "live", now);
    let g = gfs::Gfs;
    let s = crate::testing::source("gfs");
    let product = wx_core::families::gfs::PRODUCT;
    let runs = g.cycle(s).runs(now, Duration::hours(12));
    for lead in [0, 24 * 60, 123 * 60] {
        let needs = needs(s, product, lead);
        let probe = needs[0];
        let (run, name, size) =
            newest_with(&cx, &g, s, &[product], &runs, |r| g.file(s, r, &probe)).await;
        let file = ModelFile {
            name: name.clone(),
            size,
        };
        let fetched = g.fetch(&cx, s, run, &file, &needs).await.unwrap();
        let n = check_messages(&name, fetched, &needs);
        eprintln!("{name}: {n} of {} needs", needs.len());
        assert!(
            n + 5 >= needs.len(),
            "only f000 lacks fields (the rates and flags)"
        );
    }
    eprintln!("{:?}", remote.traffic());
}

#[tokio::test]
#[ignore = "network"]
async fn live_missing_files_are_none() {
    let remote = crate::testing::live::remote();
    let future = crate::clock::floor(Utc::now(), Duration::hours(1)) + Duration::hours(3);
    let key = hrrr::file_key(
        hrrr::layout("conus").unwrap(),
        future,
        wx_core::families::hrrr::SURFACE,
        0,
    );
    let url = S3Bucket::aws(hrrr::BUCKET).url(&format!("{key}.idx"));
    assert_eq!(remote.get(&url).bytes().await.unwrap(), None);
    assert_eq!(remote.traffic()[0].not_found, 1);
}
