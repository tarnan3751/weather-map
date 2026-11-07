//! Compressed files as providers serve them: gzip (MRMS `.grib2.gz`) and
//! bzip2 (DWD ICON `.grib2.bz2`). Output is capped so a corrupt or hostile
//! stream cannot exhaust memory. CPU-bound: call from a blocking thread, or
//! hand the compressed bytes to the build step, which inflates them there
//! (see `driver::Payload`).

use anyhow::{Context, ensure};
use std::io::Read;

/// Far beyond any single file a source serves whole.
pub const MAX_INFLATED: u64 = 2 << 30;

pub const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
pub const BZIP2_MAGIC: [u8; 3] = *b"BZh";

pub fn is_gzip(b: &[u8]) -> bool {
    b.starts_with(&GZIP_MAGIC)
}

pub fn is_bzip2(b: &[u8]) -> bool {
    b.starts_with(&BZIP2_MAGIC)
}

/// Inflate gzip (several members are read back to back).
pub fn gunzip(gz: &[u8], max: u64) -> anyhow::Result<Vec<u8>> {
    read_capped(flate2::read::MultiGzDecoder::new(gz), max, gz.len()).context("inflating gzip")
}

/// Inflate bzip2 (several streams are read back to back).
pub fn bunzip2(bz: &[u8], max: u64) -> anyhow::Result<Vec<u8>> {
    read_capped(bzip2::read::MultiBzDecoder::new(bz), max, bz.len()).context("inflating bzip2")
}

/// Inflate whatever compression the magic bytes announce; anything else is
/// returned as it is.
pub fn inflate(b: &[u8], max: u64) -> anyhow::Result<std::borrow::Cow<'_, [u8]>> {
    if is_gzip(b) {
        Ok(gunzip(b, max)?.into())
    } else if is_bzip2(b) {
        Ok(bunzip2(b, max)?.into())
    } else {
        Ok(b.into())
    }
}

fn read_capped(r: impl Read, max: u64, hint: usize) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(hint.saturating_mul(3).min(max as usize));
    r.take(max + 1).read_to_end(&mut out)?;
    ensure!(out.len() as u64 <= max, "inflates beyond {max} bytes");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn round_trips_and_caps() {
        let data = b"GRIB....7777".repeat(1000);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&data).unwrap();
        let gz = gz.finish().unwrap();
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        bz.write_all(&data).unwrap();
        let bz = bz.finish().unwrap();
        assert!(is_gzip(&gz) && is_bzip2(&bz) && !is_gzip(&data));
        assert_eq!(gunzip(&gz, MAX_INFLATED).unwrap(), data);
        assert_eq!(bunzip2(&bz, MAX_INFLATED).unwrap(), data);
        assert_eq!(&*inflate(&bz, MAX_INFLATED).unwrap(), &data[..]);
        assert_eq!(
            &*inflate(&data, 10).unwrap(),
            &data[..],
            "plain bytes pass through"
        );
        assert!(gunzip(&gz, 100).is_err(), "over the cap");
        assert!(bunzip2(b"BZh9 not really", MAX_INFLATED).is_err());
    }

    #[test]
    fn the_mrms_fixture_inflates() {
        let path = format!(
            "{}/../../fixtures/MRMS_PrecipFlag_00.00_20261106-114800.grib2.gz",
            env!("CARGO_MANIFEST_DIR")
        );
        let gz = std::fs::read(path).unwrap();
        assert!(gunzip(&gz, MAX_INFLATED).unwrap().starts_with(b"GRIB"));
    }
}
