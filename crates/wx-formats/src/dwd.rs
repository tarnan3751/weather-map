//! Deutscher Wetterdienst radar composites (ODIM HDF5, open data).
//!
//! | Product | Quantity | Grid | Notes |
//! |---|---|---|---|
//! | `composite/hx` | column-max reflectivity, dBZ | 4400 x 4800 x 250 m | uint16 |
//! | `composite/wn` | max reflectivity, dBZ, +0 to +120 min | 1100 x 1200 x 1 km | tar of 25 steps |
//! | `composite/rv` | precipitation per 5 min, mm, +0 to +120 min | 1100 x 1200 x 1 km | tar, uint32 |
//! | `radolan/ry` | rain rate, mm/h | 1100 x 1200 x 1 km | uint16 |
//!
//! Values are `raw * gain + offset` (reflectivity: 0.0029298 and -64.0029);
//! `nodata` (65535, or 4294967295 for uint32) is outside the composite and
//! becomes `NaN`, `undetect` (0) is covered and dry and becomes
//! [`NO_ECHO_DBZ`](crate::NO_ECHO_DBZ) or 0.
//!
//! The grids are polar stereographic (`lat_0 = 90, lat_ts = 60, lon_0 =
//! 10`, WGS84). `x_0`/`y_0` put the centre of the upper-left cell at
//! projected (0, 0) and `UL_lon, UL_lat` are its outer corner, which
//! projects to (-xscale/2, +yscale/2): the 250 m and 1 km grids share their
//! outer corners exactly.
//!
//! In a nowcast tar each member is one step, named
//! `composite_wn_YYYYMMDD_HHMM_NNN-hd5` with `NNN` the lead in minutes. A
//! member's `what/date, time` is the observation it starts from and its
//! `dataset1/what` end time the step's valid time; steps after the first
//! carry `how/simulated = True`. The lead is taken from the file and must
//! match the name.

use std::io::Read;

use anyhow::{Context, Result, bail, ensure};
use chrono::TimeDelta;
use rayon::prelude::*;

use crate::Decoded;
use crate::odim;

/// Largest tar member read: a 250 m composite is about 2 MB compressed.
const MAX_MEMBER_BYTES: u64 = 64 << 20;
/// Most members read from one tar; DWD's have 25.
const MAX_MEMBERS: usize = 100;

/// Decode one DWD composite file (`hx`, `ry`, `yw`, `rw`, `dmax`, or one
/// member of a nowcast tar).
pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    let c = odim::decode(bytes, &["DBZH", "RATE", "ACRR"])?;
    ensure!(c.source.contains("ORG:78"), "what/source is {:?}, not DWD (ORG:78)", c.source);
    ensure!(c.projdef.contains("+proj=stere"), "DWD composite with projdef {:?}", c.projdef);
    Ok(c.decoded)
}

/// Decode every step of a DWD nowcast tar (`composite/wn`, `rv`, `rs`),
/// sorted by lead time. All steps must start from the same observation.
/// Members are read in order and decoded in parallel; a failure is reported
/// for the first member that fails.
pub fn decode_tar(bytes: &[u8]) -> Result<Vec<Decoded>> {
    let mut archive = tar::Archive::new(bytes);
    let mut members: Vec<(String, Vec<u8>)> = Vec::new();
    for entry in archive.entries().context("not a tar archive")? {
        let mut entry = entry.context("damaged tar archive")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry.path().context("tar member name")?.to_string_lossy().into_owned();
        ensure!(members.len() < MAX_MEMBERS, "more than {MAX_MEMBERS} members in the tar");
        let size = entry.size();
        ensure!(size <= MAX_MEMBER_BYTES, "tar member {name} is {size} bytes");
        let mut buf = Vec::with_capacity(size as usize);
        entry.read_to_end(&mut buf).with_context(|| format!("reading tar member {name}"))?;
        ensure!(buf.len() as u64 == size, "tar member {name} is truncated");
        members.push((name, buf));
    }
    let decoded: Vec<Result<Decoded>> = members
        .par_iter()
        .map(|(name, buf)| {
            let step = decode(buf).with_context(|| format!("tar member {name}"))?;
            if let Some(lead) = lead_from_name(name) {
                ensure!(
                    step.lead == TimeDelta::minutes(lead),
                    "tar member {name} has lead {} min inside",
                    step.lead.num_minutes()
                );
            }
            Ok(step)
        })
        .collect();
    let mut steps = decoded.into_iter().collect::<Result<Vec<Decoded>>>()?;
    if steps.is_empty() {
        bail!("tar archive holds no composite");
    }
    steps.sort_by_key(|s| s.lead);
    let run = steps[0].run.unwrap_or(steps[0].valid);
    for s in &steps {
        let r = s.run.unwrap_or(s.valid);
        ensure!(r == run, "steps start from different observations ({run} and {r})");
    }
    for w in steps.windows(2) {
        ensure!(w[0].lead != w[1].lead, "two steps with lead {} min", w[0].lead.num_minutes());
    }
    // The first step is the observation itself; give every step the same run.
    for s in &mut steps {
        if s.run.is_none() {
            s.run = Some(run);
        }
    }
    Ok(steps)
}

/// `NNN` from `..._NNN-hd5`, the lead in minutes.
fn lead_from_name(name: &str) -> Option<i64> {
    let base = name.rsplit('/').next()?;
    let stem = base.strip_suffix("-hd5").or_else(|| base.strip_suffix(".h5"))?;
    let digits = stem.rsplit('_').next()?;
    (digits.len() == 3 && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lead_from_member_names() {
        assert_eq!(lead_from_name("composite_wn_20261106_1815_000-hd5"), Some(0));
        assert_eq!(lead_from_name("x/composite_rv_20261106_1810_120-hd5"), Some(120));
        assert_eq!(lead_from_name("composite_hx_LATEST-hd5"), None);
        assert_eq!(lead_from_name("README"), None);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(decode_tar(b"definitely not a tar").is_err());
        assert!(decode_tar(&[0u8; 1024]).is_err());
    }
}
