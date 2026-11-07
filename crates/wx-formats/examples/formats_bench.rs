//! Decode time and a digest of the decoded values for the research samples,
//! one reader per run so that the peak memory of each can be read off:
//!
//! ```text
//! /usr/bin/time -l cargo run --release -p wx-formats --example formats_bench -- \
//!     /path/to/research gmgsi|goes|opera|dwd_tar|dwd_hdf|cmorph
//! ```

use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::Instant;

use wx_formats::{cmorph, dwd, gmgsi, goes, opera};

/// A reader: the bytes of a file to what it decodes to.
type Reader = fn(&[u8]) -> Vec<wx_formats::Decoded>;

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(dir), Some(which)) = (args.next(), args.next()) else {
        panic!("usage: formats_bench <research dir> <reader>");
    };
    let read = |rel: &str| std::fs::read(Path::new(&dir).join(rel)).unwrap();
    let (file, decode): (&str, Reader) = match which.as_str() {
        "gmgsi" => {
            ("satellite/gmgsi/GLOBCOMPLIR_v3r0_blend_s202611061700000_e202611061709599_c202611061733476.nc", |b| {
                vec![gmgsi::decode(b).unwrap()]
            })
        }
        "goes" => ("satellite/goes/CMIPF_C13_G19_s20263101810214.nc", |b| vec![goes::decode(b).unwrap()]),
        "opera" => ("radar/opera/opera_DBZH.h5", |b| vec![opera::decode(b).unwrap()]),
        "dwd_tar" => ("radar/dwd/composite_wn__LATEST.tar", |b| dwd::decode_tar(b).unwrap()),
        "dwd_hdf" => ("radar/dwd/composite_hx_LATEST-hd5", |b| vec![dwd::decode(b).unwrap()]),
        "cmorph" => {
            ("satellite/cmorph2/CMORPH2_0.25deg-30min_202611061700.RT.nc", |b| vec![cmorph::decode(b).unwrap()])
        }
        other => panic!("no reader {other}"),
    };
    let bytes = read(file);
    let t = Instant::now();
    let out = decode(&bytes);
    let first = t.elapsed().as_secs_f64() * 1e3;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for d in &out {
        let f = &d.field;
        (f.nx, f.ny, d.valid).hash(&mut h);
        for v in &f.values {
            v.to_bits().hash(&mut h);
        }
        format!("{:?}", f.proj).hash(&mut h);
    }
    drop(out);
    let best = (0..4)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(decode(&bytes));
            t.elapsed().as_secs_f64() * 1e3
        })
        .fold(f64::INFINITY, f64::min);
    println!("{which}: first {first:.1} ms, best of 4 {best:.1} ms, digest {:016x}", h.finish());
}
