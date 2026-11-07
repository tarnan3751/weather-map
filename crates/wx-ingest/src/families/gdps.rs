//! GDPS: ECCC's 15 km global model and the map's main global base, on the
//! Datamart ([`super::eccc`]) under `model_gdps/15km`, one file per field
//! and step.
//!
//! Runs start at 00 and 12 UTC and are ingested hourly to the registry's
//! horizon (`wx_core::families::gdps`, which also states the volumes): the
//! model's hourly part, since its steps are three-hourly after 84 h. A
//! run's step files appear from about 3 h 50 after the run time (the first
//! dozen steps together, the run's first step with them) to 4 h 40 for the
//! 120th, and up to an hour and a half later on a slow day.
//!
//! The grid is global, from the South Pole northward and from Greenwich
//! eastward, with earth-relative winds. Precipitation is read as each hour's
//! own amounts (`Precip-Accum1h`, `Snow-Accum1h`), so the product's steps
//! are [`Steps::Windows`]; snowfall is a run total.

use super::eccc::{self, Datamart, Layout};
use crate::driver::{Cx, Cycle, Fetched, Listing, Model, ModelFile, Need, Steps};
use crate::remote::FetchError;
use chrono::{DateTime, Duration, Timelike, Utc};
use wx_core::families::gdps as reg;
use wx_core::{Family, SourceDef};

pub const LAYOUT: Layout = Layout {
    product: reg::PRODUCT,
    model: "GDPS",
    grid: "LatLon0.15",
};

/// Steps ingested, in hours.
pub fn hours() -> std::ops::RangeInclusive<u32> {
    0..=reg::HORIZON_HOURS
}

/// When step `hour` of a run usually appears: 3 h 50 after the run for the
/// first steps, 4 h 40 for the 120th (the medians of a week's runs, which
/// were up to an hour later at worst).
pub fn arrival(run: DateTime<Utc>, hour: u32) -> DateTime<Utc> {
    run + Duration::seconds(230 * 60 + i64::from(hour) * 25)
}

pub struct Gdps {
    mart: Datamart,
}

impl Default for Gdps {
    fn default() -> Self {
        Self::with_host(eccc::HOST)
    }
}

impl Gdps {
    /// Reading from `host` (the Datamart, or its mirror).
    pub fn with_host(host: &str) -> Self {
        Self {
            mart: Datamart::new(host, LAYOUT, eccc::registry_levels(Family::Gdps)),
        }
    }

    #[cfg(test)]
    pub fn datamart(&self) -> &Datamart {
        &self.mart
    }
}

#[async_trait::async_trait]
impl Model for Gdps {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        super::common::one_region(source, "GDPS")
    }

    fn cycle(&self, _: &SourceDef) -> Cycle {
        Cycle {
            every: Duration::hours(12),
            spare: 1,
            // A run takes about 45 minutes to land.
            relist: Duration::minutes(5),
            // Slow runs land an hour or more behind the usual times.
            give_up: Duration::hours(3),
        }
    }

    fn steps(&self, _: &SourceDef, _: &str) -> Steps {
        Steps::Windows
    }

    fn leads(&self, _: &SourceDef, _: &str, run: DateTime<Utc>) -> Vec<u32> {
        if !reg::RUN_HOURS.contains(&run.hour()) || run.minute() != 0 {
            return Vec::new();
        }
        hours().map(|h| h * 60).collect()
    }

    fn due(&self, _: &SourceDef, _: &str, run: DateTime<Utc>, lead: u32) -> DateTime<Utc> {
        arrival(run, lead / 60)
    }

    fn file(&self, _: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        self.mart.file(run, need)
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        run: DateTime<Utc>,
        _: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let hours: Vec<u32> = self
            .leads(source, reg::PRODUCT, run)
            .iter()
            .map(|l| l / 60)
            .collect();
        Ok(Some(self.mart.list(cx, run, &hours).await?))
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        run: DateTime<Utc>,
        file: &ModelFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        self.mart
            .fetch(cx, run, file, needs, eccc::decoder(run))
            .await
    }
}

#[cfg(test)]
mod tests;
