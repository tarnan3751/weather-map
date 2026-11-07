//! CPTEC: INPE's WRF at 7 km over South America, on `dataserver.cptec.inpe.br`
//! at `.../ams_07km/brutos/{YYYY}/{MM}/{DD}/{HH}/`, one GRIB2 file per valid
//! time, named `WRF_cpt_07KM_{run YYYYMMDDHH}_{valid YYYYMMDDHH}.grib2`. Each
//! holds every level (about 190 MB) and has a wgrib-style `.inv` beside it
//! (named without `.grib2`), so the few wanted messages are fetched by byte
//! range. The server lists directories as HTML, with sizes rounded (`190M`),
//! so listed sizes are not used.
//!
//! One run a day (00 UTC) with hourly files from f000; the registry's
//! horizon is the part ingested. A run's files land from 5 h 20 after the
//! run time (f000) at about 45 seconds an hour of lead (f120 at 6 h 50).
//!
//! Accumulations (`APCP`, `0-12 hour acc fcst`, `0-0 day acc fcst` at f000)
//! are run totals, so the driver differences them.

use crate::driver::{Cx, Cycle, Fetched, Listing, Model, ModelFile, Need};
use crate::remote::FetchError;
use chrono::{DateTime, Duration, Timelike, Utc};
use wx_core::SourceDef;
use wx_core::families::cptec as reg;

pub const BASE: &str = "https://dataserver.cptec.inpe.br/dataserver_modelos/wrf/ams_07km/brutos";

/// The region the registry names this grid by.
const REGION: &str = "ams_07km";

/// Minutes after the run when f000 appears, and the extra seconds per hour
/// of lead.
const FIRST_FILE_MINUTES: i64 = 320;
const SECONDS_PER_HOUR_OF_LEAD: i64 = 45;

/// Forecast hours ingested: hourly from the analysis.
pub fn hours() -> impl Iterator<Item = u32> {
    0..=reg::HORIZON_HOURS
}

/// The directory of a run.
pub fn run_dir(run: DateTime<Utc>) -> String {
    format!("{BASE}/{}/{:02}/", run.format("%Y/%m/%d"), run.hour())
}

/// The file of a run at a lead in minutes; the name the listing and `file`
/// use.
pub fn file_name(run: DateTime<Utc>, lead: u32) -> String {
    let valid = run + Duration::minutes(lead as i64);
    format!(
        "{}_{}_{}.grib2",
        reg::PRODUCT,
        run.format("%Y%m%d%H"),
        valid.format("%Y%m%d%H")
    )
}

/// The inventory of a file.
pub fn inventory_name(file: &str) -> String {
    format!("{}.inv", file.strip_suffix(".grib2").unwrap_or(file))
}

/// When file `hour` of a run usually appears.
pub fn arrival(run: DateTime<Utc>, hour: u32) -> DateTime<Utc> {
    run + Duration::minutes(FIRST_FILE_MINUTES)
        + Duration::seconds(hour as i64 * SECONDS_PER_HOUR_OF_LEAD)
}

#[derive(Debug, Default)]
pub struct Cptec;

#[async_trait::async_trait]
impl Model for Cptec {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        anyhow::ensure!(
            source.region == REGION,
            "no CPTEC layout for region {:?}",
            source.region
        );
        Ok(())
    }

    fn cycle(&self, _: &SourceDef) -> Cycle {
        Cycle {
            every: Duration::hours(24),
            spare: 1,
            relist: Duration::minutes(5),
            give_up: Duration::minutes(90),
        }
    }

    fn leads(&self, _: &SourceDef, _: &str, run: DateTime<Utc>) -> Vec<u32> {
        if !reg::RUN_HOURS.contains(&run.hour()) {
            return Vec::new();
        }
        hours().map(|h| h * 60).collect()
    }

    fn due(&self, _: &SourceDef, _: &str, run: DateTime<Utc>, lead: u32) -> DateTime<Utc> {
        arrival(run, lead / 60)
    }

    fn file(&self, _: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        file_name(run, need.lead)
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        run: DateTime<Utc>,
        _: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let mut listing = Listing::new();
        // The day's directory does not exist until the run starts to land.
        let Some(entries) = cx.list_html(&run_dir(run)).await? else {
            return Ok(Some(listing));
        };
        let names: std::collections::HashSet<&str> = entries
            .iter()
            .filter(|e| !e.is_dir)
            .map(|e| e.name.as_str())
            .collect();
        for name in &names {
            // The inventory is written with its file.
            if name.ends_with(".grib2") && names.contains(inventory_name(name).as_str()) {
                listing.add(*name, None);
            }
        }
        Ok(Some(listing))
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        run: DateTime<Utc>,
        file: &ModelFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let dir = run_dir(run);
        cx.grib_with_idx(
            &format!("{dir}{}", file.name),
            &format!("{dir}{}", inventory_name(&file.name)),
            None,
            needs,
        )
        .await
    }
}

#[cfg(test)]
mod tests;
