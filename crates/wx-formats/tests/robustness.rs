//! Third-party input: damaged files are errors, never panics; absurd sizes
//! are refused before allocation; decoders run side by side on many threads.

mod common;

use common::*;
use wx_formats::{cmorph, cwa, dpc, dwd, gmgsi, goes, jma, opera};

type Decoder = fn(&[u8]) -> bool;

fn decoders() -> Vec<(&'static str, Decoder)> {
    vec![
        ("opera_20261106T1800_RATE.h5", |b| opera::decode(b).is_ok()),
        ("dwd_raa01-ry_20261106_1815-hdf5", |b| dwd::decode(b).is_ok()),
        ("dwd_composite_wn_20261106_1815_cut4of25.tar", |b| dwd::decode_tar(b).is_ok()),
        ("CMORPH2_0.25deg-30min_202611061700.RT.nc", |b| cmorph::decode(b).is_ok()),
        ("gmgsi_GLOBCOMPLIR_20261106T1700_rows700-1500_cols900-1700.nc", |b| gmgsi::decode(b).is_ok()),
        ("goes19_CMIPF_C13_s20263101810214_rows2600-2800_cols0-300_limb.nc", |b| goes::decode(b).is_ok()),
        ("dpc_VMI_20261106_1820.tif", |b| dpc::decode(b).is_ok()),
        ("cwa_O-A0059-001_20261106T1820_cols420-660_rows0-300.json", |b| cwa::decode(b).is_ok()),
        ("jma_hrpns_20261106T1820_6_56_25.png", |b| jma::decode_tiles(6, 56, 25, 1, 1, &[Some(b)]).is_ok()),
    ]
}

/// A small deterministic generator, so failures reproduce.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

#[test]
fn truncated_files_are_errors() {
    for (name, decode) in decoders() {
        let bytes = fixture(name);
        assert!(decode(&bytes), "{name} must decode whole");
        let n = bytes.len();
        for cut in [0, 1, 8, 64, 512, 4096, n / 10, n / 3, n / 2, n - n / 10, n - 1000, n - 1] {
            if cut < n {
                // Some formats tolerate losing trailing bytes; what matters
                // is that nothing panics.
                let _ = decode(&bytes[..cut]);
            }
        }
        assert!(!decode(&bytes[..n / 2]), "{name}: half a file decoded");
    }
}

#[test]
fn corrupted_files_do_not_panic() {
    // WX_FORMATS_FUZZ_ROUNDS raises the count for a longer run.
    let rounds: usize = std::env::var("WX_FORMATS_FUZZ_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let mut rng = Lcg(20_261_106);
    for (name, decode) in decoders() {
        let bytes = fixture(name);
        for round in 0..rounds {
            let mut b = bytes.clone();
            // Early rounds hit headers and metadata, later ones anywhere.
            let span = if round % 2 == 0 { b.len().min(4096) } else { b.len() };
            for _ in 0..1 + round % 4 {
                let at = rng.next() as usize % span;
                b[at] ^= 1 << (rng.next() % 8);
            }
            let _ = decode(&b);
        }
    }
}

#[test]
fn absurd_sizes_are_refused_before_allocation() {
    let err = dwd::decode(&fixture("synthetic_odim_claims_200000x200000.h5")).unwrap_err();
    assert!(format!("{err:#}").contains("point limit"), "{err:#}");
    let json = r#"{"cwaopendata":{"dataset":{"datasetInfo":{"parameterSet":{
        "StartPointLongitude":"115.0","StartPointLatitude":"18.0","GridResolution":"0.0125",
        "DateTime":"2026-11-07T02:20:00+08:00","GridDimensionX":"100000","GridDimensionY":"100000"}},
        "contents":{"content":"-99"}}}}"#;
    let err = cwa::decode(json.as_bytes()).unwrap_err();
    assert!(format!("{err:#}").contains("point limit"), "{err:#}");
    let err = jma::decode_tiles(14, 0, 0, 1000, 1000, &[]).unwrap_err();
    assert!(format!("{err:#}").contains("tiles for") || format!("{err:#}").contains("limit"), "{err:#}");
    let short = r#"{"cwaopendata":{"dataset":{"datasetInfo":{"parameterSet":{
        "StartPointLongitude":"115.0","StartPointLatitude":"18.0","GridResolution":"0.0125",
        "DateTime":"2026-11-07T02:20:00+08:00","GridDimensionX":"3","GridDimensionY":"2"}},
        "contents":{"content":"-99,-999,10.5"}}}}"#;
    assert!(cwa::decode(short.as_bytes()).is_err());
}

#[test]
fn wrong_product_is_an_error() {
    let opera = fixture("opera_20261106T1800_RATE.h5");
    assert!(dwd::decode(&opera).is_err(), "OPERA is not DWD");
    assert!(goes::decode(&opera).is_err());
    assert!(cmorph::decode(&opera).is_err());
    let dwd_file = fixture("dwd_raa01-ry_20261106_1815-hdf5");
    assert!(opera::decode(&dwd_file).is_err(), "DWD's stereographic grid is not OPERA");
    assert!(gmgsi::decode(&fixture("CMORPH2_0.25deg-30min_202611061700.RT.nc")).is_err());
    assert!(opera::decode(&fixture("dpc_SRI_20261106_1820.tif")).is_err());
    assert!(dpc::decode(&fixture("opera_20261106T1800_RATE.h5")).is_err());
}

#[test]
fn decoders_run_concurrently() {
    let files: Vec<(&str, Vec<u8>)> = decoders().iter().map(|(n, _)| (*n, fixture(n))).collect();
    let reference: Vec<Vec<f32>> = files.iter().map(|(n, b)| values(n, b)).collect();
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..4)
            .flat_map(|_| files.iter().enumerate())
            .map(|(k, (n, b))| s.spawn(move || (k, values(n, b))))
            .collect();
        for h in handles {
            let (k, v) = h.join().unwrap();
            assert!(
                v.len() == reference[k].len() && v.iter().zip(&reference[k]).all(|(a, b)| a.to_bits() == b.to_bits()),
                "{} differs when decoded concurrently",
                files[k].0
            );
        }
    });
}

fn values(name: &str, b: &[u8]) -> Vec<f32> {
    if name.ends_with(".tar") {
        return dwd::decode_tar(b).unwrap().into_iter().flat_map(|d| d.field.values).collect();
    }
    if name.ends_with(".png") {
        return jma::decode_tiles(6, 56, 25, 1, 1, &[Some(b)]).unwrap().values;
    }
    let d = match name {
        n if n.starts_with("opera") => opera::decode(b),
        n if n.starts_with("dwd") => dwd::decode(b),
        n if n.starts_with("CMORPH") => cmorph::decode(b),
        n if n.starts_with("gmgsi") => gmgsi::decode(b),
        n if n.starts_with("goes") => goes::decode(b),
        n if n.starts_with("dpc") => dpc::decode(b),
        n if n.starts_with("cwa") => cwa::decode(b),
        n => panic!("{n}"),
    };
    d.unwrap().field.values
}
