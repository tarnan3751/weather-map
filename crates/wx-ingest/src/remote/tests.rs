use super::memory::MemoryRemote;
use super::*;

const URL: &str =
    "https://noaa-hrrr-bdp-pds.s3.amazonaws.com/hrrr.20261106/conus/hrrr.t11z.wrfsfcf00.grib2";

fn remote(mem: &Arc<MemoryRemote>) -> Remote {
    Remote::with_transport(mem.clone(), RemoteConfig::default())
}

fn range(start: u64, end: u64) -> ByteRange {
    ByteRange {
        start,
        end: Some(end),
    }
}

#[tokio::test(start_paused = true)]
async fn whole_ranged_missing_and_broken() {
    let mem = MemoryRemote::new();
    mem.put(URL, &b"GRIBxxxx7777"[..]);
    let r = remote(&mem);
    assert_eq!(
        &r.get(URL).bytes().await.unwrap().unwrap()[..],
        b"GRIBxxxx7777"
    );
    assert_eq!(
        &r.get(URL)
            .range(range(1, 3))
            .bytes()
            .await
            .unwrap()
            .unwrap()[..],
        b"RI"
    );
    let open = ByteRange {
        start: 8,
        end: None,
    };
    assert_eq!(
        &r.get(URL).range(open).bytes().await.unwrap().unwrap()[..],
        b"7777"
    );
    assert_eq!(r.get(format!("{URL}.idx")).bytes().await.unwrap(), None);
    // A range past the end is the object's fault, and so is an empty one.
    let past = r.get(URL).range(range(20, 30)).bytes().await.unwrap_err();
    assert_eq!(past.failure, Failure::Object, "{past}");
    assert_eq!(
        r.get(URL)
            .range(range(3, 3))
            .bytes()
            .await
            .unwrap_err()
            .failure,
        Failure::Object
    );
    // So is a body over its limit.
    let big = r.get(URL).max_bytes(4).bytes().await.unwrap_err();
    assert_eq!(big.failure, Failure::Object, "{big}");
    // And a 403.
    mem.fail(URL, 403, 1);
    assert_eq!(
        r.get(URL).bytes().await.unwrap_err().failure,
        Failure::Object
    );
    assert_eq!(
        r.get("ftp://example.org/x")
            .bytes()
            .await
            .unwrap_err()
            .failure,
        Failure::Object
    );

    let t = &r.traffic()[0];
    assert_eq!(t.host, "noaa-hrrr-bdp-pds.s3.amazonaws.com");
    assert_eq!((t.get_requests, t.not_found, t.list_requests), (7, 1, 0));
    assert_eq!(t.bytes, 12 + 2 + 4);
}

#[tokio::test(start_paused = true)]
async fn a_short_ranged_reply_is_refused() {
    struct Short;
    #[async_trait::async_trait]
    impl Transport for Short {
        async fn get(&self, _: &Request) -> Result<Response, TransportError> {
            Ok(Response {
                status: 206,
                headers: Vec::new(),
                body: Bytes::from_static(b"GR"),
            })
        }
    }
    let r = Remote::with_transport(Arc::new(Short), RemoteConfig::default());
    let e = r.get(URL).range(range(0, 10)).bytes().await.unwrap_err();
    assert_eq!(e.failure, Failure::Object);
    assert!(e.to_string().contains("asked for 10 bytes, got 2"), "{e}");
}

#[tokio::test(start_paused = true)]
async fn blips_are_retried_and_outages_reported() {
    let mem = MemoryRemote::new();
    mem.put(URL, &b"GRIB"[..]);
    let r = remote(&mem);
    mem.fail(URL, 503, 2);
    mem.drop_connections(URL, 1);
    let started = tokio::time::Instant::now();
    assert_eq!(&r.get(URL).bytes().await.unwrap().unwrap()[..], b"GRIB");
    // 1 s, 2 s and 4 s of backoff.
    assert!(
        started.elapsed() >= Duration::from_secs(7),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(r.traffic()[0].retries, 3);
    assert_eq!(mem.requests().len(), 4);

    mem.fail(URL, 500, 10);
    let e = r.get(URL).bytes().await.unwrap_err();
    assert_eq!(e.failure, Failure::Service, "{e}");
    // Throttling says how long to wait.
    mem.clear_log();
    struct Throttle(std::sync::atomic::AtomicU32);
    #[async_trait::async_trait]
    impl Transport for Throttle {
        async fn get(&self, _: &Request) -> Result<Response, TransportError> {
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Response {
                status: if n == 0 { 429 } else { 200 },
                headers: vec![("retry-after".into(), "9".into())],
                body: Bytes::from_static(b"ok"),
            })
        }
    }
    let r = Remote::with_transport(Arc::new(Throttle(0.into())), RemoteConfig::default());
    let started = tokio::time::Instant::now();
    r.get(URL).bytes().await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(9));
}

#[tokio::test(start_paused = true)]
async fn conditional_requests() {
    let mem = MemoryRemote::new();
    let latest = "https://opendata.dwd.de/weather/radar/composite/hx/composite_hx_LATEST-hd5";
    mem.put(latest, &b"first"[..]);
    let r = remote(&mem);
    let first = r.get(latest).send().await.unwrap().unwrap();
    assert!(!first.not_modified && first.validators.etag.is_some());
    assert!(first.validators.last_modified.is_some());
    let again = r
        .get(latest)
        .if_changed(&first.validators)
        .send()
        .await
        .unwrap()
        .unwrap();
    assert!(again.not_modified && again.body.is_empty());
    mem.put(latest, &b"second"[..]);
    let changed = r
        .get(latest)
        .if_changed(&first.validators)
        .send()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&changed.body[..], b"second");
}

#[tokio::test(start_paused = true)]
async fn hosts_are_spaced_and_capped() {
    let mem = MemoryRemote::new();
    let slow = "https://opendata.dwd.de/a";
    mem.put(slow, &b"x"[..]);
    let config = RemoteConfig {
        hosts: vec![(
            "dwd.de".to_string(),
            HostPolicy {
                concurrency: 1,
                min_interval: Duration::from_secs(1),
            },
        )],
        ..RemoteConfig::default()
    };
    let r = Remote::with_transport(mem.clone(), config);
    assert_eq!(
        r.policy("opendata.dwd.de").min_interval,
        Duration::from_secs(1)
    );
    assert_eq!(r.policy("notdwd.de"), HostPolicy::GENTLE);
    assert_eq!(
        r.policy("noaa-gfs-bdp-pds.s3.amazonaws.com"),
        HostPolicy::BULK
    );
    let started = tokio::time::Instant::now();
    let gets: Vec<_> = (0..4).map(|_| r.get(slow).bytes()).collect();
    for g in futures::future::join_all(gets).await {
        g.unwrap();
    }
    assert!(
        started.elapsed() >= Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn s3_listing_pages_and_directories() {
    let mem = MemoryRemote::new();
    let bucket = S3Bucket::aws("noaa-mrms-pds");
    mem.serve_s3(&bucket, 2);
    for key in [
        "CONUS/P/20261106/MRMS_P_20261106-114838.grib2.gz",
        "CONUS/P/20261106/MRMS_P_20261106-115038.grib2.gz",
        "CONUS/P/20261106/MRMS_P_20261106-115238.grib2.gz",
        "CONUS/P/20261107/MRMS_P_20261107-000038.grib2.gz",
        "CONUS/Q/20261106/MRMS_Q_20261106-114800.grib2.gz",
        "ALASKA/P/20261106/a@b.grib2.gz",
    ] {
        mem.put_s3(&bucket, key, &b"x"[..]);
    }
    let r = remote(&mem);
    let keys = |v: Vec<Listed>| v.into_iter().map(|l| l.key).collect::<Vec<_>>();
    let all = r
        .list_s3("t", &bucket, "CONUS/P/2026110", None)
        .await
        .unwrap();
    assert_eq!(all.len(), 4);
    assert!(all.iter().all(|l| l.size == 1 && l.modified.is_some()));
    let after = r
        .list_s3(
            "t",
            &bucket,
            "CONUS/P/20261106/",
            Some("CONUS/P/20261106/MRMS_P_20261106-115038"),
        )
        .await
        .unwrap();
    assert_eq!(
        keys(after),
        [
            "CONUS/P/20261106/MRMS_P_20261106-115038.grib2.gz",
            "CONUS/P/20261106/MRMS_P_20261106-115238.grib2.gz"
        ]
    );
    assert_eq!(
        keys(r.list_s3("t", &bucket, "ALASKA/", None).await.unwrap()),
        ["ALASKA/P/20261106/a@b.grib2.gz"]
    );
    assert!(
        r.list_s3("t", &bucket, "nothing/", None)
            .await
            .unwrap()
            .is_empty()
    );
    // Pages of two: two pages for four keys, one for each other listing.
    let t = &r.traffic()[0];
    assert_eq!(t.list_requests, 2 + 1 + 1 + 1);
    let missing = r
        .list_s3("t", &S3Bucket::aws("nope"), "", None)
        .await
        .unwrap_err();
    assert_eq!(missing.failure, Failure::Object);
}

#[tokio::test(start_paused = true)]
async fn html_listing() {
    let mem = MemoryRemote::new();
    let page = "https://opendata.dwd.de/weather/nwp/icon-d2/grib/12/t_2m/";
    mem.put(
        page,
        &br#"<pre><a href="../">../</a>
<a href="f_000.grib2.bz2">f_000.grib2.bz2</a> 06-Nov-2026 12:45   1031234
</pre>"#[..],
    );
    let r = remote(&mem);
    let entries = r.list_html("t", page).await.unwrap().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].url, format!("{page}f_000.grib2.bz2"));
    assert_eq!(
        r.list_html("t", "https://opendata.dwd.de/none/")
            .await
            .unwrap(),
        None
    );
    assert_eq!(r.traffic()[0].list_requests, 2);
}
