//! ICON: DWD's regional models ICON-EU and ICON-D2 on `opendata.dwd.de`, at
//! `weather/nwp/{model}/grib/{HH}/{var}/`, one bzip2-compressed GRIB2 file
//! per variable and step:
//! `{model}_{domain}_regular-lat-lon_single-level_{YYYYMMDDHH}_{step:03}_{token}.grib2.bz2`.
//!
//! The selectors' `var` is the variable directory; the file token is that
//! upper-cased for ICON-EU (`T_2M`) and `2d_` before it for ICON-D2
//! (`2d_t_2m`). A run is listed through the HTML indexes of the directories
//! its layers read. A directory holds only the latest run of its hour, so
//! names are matched by the run time they carry and yesterday's files are
//! never taken for today's.
//!
//! Files are fetched whole (bzip2 forbids ranges), inflated once and split
//! into one message per need on a blocking thread: ICON-D2's `tot_prec` and
//! `dbz_cmax` files hold four 15-minute messages (:00 to :45 of the file's
//! hour), so one download feeds four radar parts. Each file holds one
//! variable, so a need's message is found by its time and statistic.
//!
//! Regions ([`layout`]), horizons and cadences are the registry's
//! (`wx_core::families::icon`): ICON-EU hourly to 78 h on its 00/06/12/18 UTC
//! runs and to 30 h on the others, ICON-D2 hourly to 48 h with its radar
//! every 15 minutes ([`SUBHOURLY`]). Accumulations are run totals.

use crate::driver::{self, Cx, Cycle, Decoder, Fetched, Listing, Model, ModelFile, Need, Payload};
use crate::remote::{FetchError, codec};
use anyhow::{Context, bail, ensure};
use bytes::Bytes;
use chrono::{DateTime, Duration, Timelike, Utc};
use std::sync::LazyLock;
use wx_core::SourceDef;
use wx_core::families::icon as reg;
use wx_core::layer::Stat;
use wx_pipeline::{MessageInfo, Timing};

pub use wx_core::families::icon::{PRODUCT, SUBHOURLY};

pub const BASE: &str = "https://opendata.dwd.de/weather/nwp/";
/// Far beyond any file the inputs read (ICON-D2's `dbz_cmax` is 2.8 MB).
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Code table 4.10.
const ACCUMULATION: u8 = 1;
const MAXIMUM: u8 = 2;

/// The two models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Eu,
    D2,
}

pub fn layout(region: &str) -> Option<Layout> {
    Some(match region {
        "icon-eu" => Layout::Eu,
        "icon-d2" => Layout::D2,
        _ => return None,
    })
}

impl Layout {
    /// The model's directory, which also starts its file names.
    pub fn dir(self) -> &'static str {
        match self {
            Layout::Eu => "icon-eu",
            Layout::D2 => "icon-d2",
        }
    }

    fn domain(self) -> &'static str {
        match self {
            Layout::Eu => "europe",
            Layout::D2 => "germany",
        }
    }

    /// The variable's token in file names.
    pub fn token(self, var: &str) -> String {
        match self {
            Layout::Eu => var.to_ascii_uppercase(),
            Layout::D2 => format!("2d_{var}"),
        }
    }

    /// The last hourly step of `run` that is ingested.
    pub fn last_hour(self, run: DateTime<Utc>) -> u32 {
        match self {
            Layout::Eu if reg::EU_LONG_RUN_HOURS.contains(&run.hour()) => reg::EU_HORIZON_HOURS,
            Layout::Eu => reg::EU_SHORT_RUN_HORIZON_HOURS,
            Layout::D2 => reg::D2_HORIZON_HOURS,
        }
    }

    /// When step `hour` of a run is usually complete (its last variable
    /// listed), from the 2026-11-06 runs: ICON-D2 44 minutes after the run
    /// for step 0 and 45 s more per step (1 h 20 for step 48); ICON-EU
    /// 2 h 39 and 35 s more per step (3 h 25 for step 78, a few minutes
    /// early for the long runs and late for the short ones).
    pub fn arrival(self, run: DateTime<Utc>, hour: u32) -> DateTime<Utc> {
        let h = i64::from(hour);
        let secs = match self {
            Layout::D2 => 44 * 60 + h * 45,
            Layout::Eu => 159 * 60 + h * 35,
        };
        run + Duration::seconds(secs)
    }
}

/// The directory of `var` for `run`, below [`BASE`].
pub fn var_dir(l: Layout, run: DateTime<Utc>, var: &str) -> String {
    format!("{}/grib/{:02}/{var}/", l.dir(), run.hour())
}

/// The file of `var` at step `hour` of `run`, below [`BASE`].
pub fn file_key(l: Layout, run: DateTime<Utc>, var: &str, hour: u32) -> String {
    format!(
        "{}{}_{}_{PRODUCT}_{}_{hour:03}_{}.grib2.bz2",
        var_dir(l, run, var),
        l.dir(),
        l.domain(),
        run.format("%Y%m%d%H"),
        l.token(var)
    )
}

/// The step whose file holds a lead: ICON-D2's file `h` holds h:00, h:15,
/// h:30 and h:45.
pub fn file_hour(lead: u32) -> u32 {
    lead / 60
}

/// The variable directories that `source`'s inputs on `products` read, in
/// registry order.
fn variables(source: &SourceDef, products: &[&str]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (_, input) in driver::inputs_of(wx_core::LAYERS, source) {
        if !products.contains(&input.product) {
            continue;
        }
        for f in input.fields {
            if !out.contains(&f.var) {
                out.push(f.var);
            }
        }
    }
    out
}

/// Whether message `m` is `need`'s field. A file holds one variable, so its
/// time says which message: the valid time for an instantaneous value, an
/// accumulation from the run's start or a maximum ending at the lead.
pub fn holds(m: &MessageInfo, need: &Need) -> bool {
    let Some(sel) = need.field else {
        return false;
    };
    let at = m.reference_time + Duration::minutes(i64::from(need.lead));
    match (sel.stat, m.time) {
        (Stat::Instant, Some(Timing::Instant(t))) => t == at,
        (
            Stat::RunTotal,
            Some(Timing::Interval {
                start,
                end,
                process,
            }),
        ) => start == m.reference_time && end == at && process == Some(ACCUMULATION),
        (Stat::StepMax, Some(Timing::Interval { end, process, .. })) => {
            end == at && process == Some(MAXIMUM)
        }
        _ => false,
    }
}

/// Decodes the one message a need was given (see [`split`]).
static DECODER: LazyLock<Decoder> = LazyLock::new(|| driver::grib_picking(holds));

/// Inflate an ICON file of `run` and give each need its message, or `None`
/// where the file has none for it (the last ICON-D2 file holds only its
/// :00 message). The messages share the inflated buffer.
pub fn split(
    file: &[u8],
    run: DateTime<Utc>,
    needs: &[Need],
) -> anyhow::Result<Vec<Option<Payload>>> {
    let grib = Bytes::from(codec::inflate(file, codec::MAX_INFLATED)?.into_owned());
    let infos = wx_pipeline::scan(&grib).context("scanning the GRIB2 messages")?;
    ensure!(!infos.is_empty(), "no GRIB2 message in the file");
    if let Some(m) = infos.iter().find(|m| m.reference_time != run) {
        bail!(
            "a message of the {} run in a file of the {run} run",
            m.reference_time
        );
    }
    Ok(needs
        .iter()
        .map(|n| {
            infos.iter().find(|m| holds(m, n)).map(|m| Payload::Custom {
                bytes: grib.slice(m.offset..m.offset + m.length),
                decode: DECODER.clone(),
            })
        })
        .collect())
}

#[derive(Debug, Default)]
pub struct Icon;

impl Icon {
    fn layout(source: &SourceDef) -> Result<Layout, FetchError> {
        layout(source.region).ok_or_else(|| {
            FetchError::object(
                source.id,
                format!("no ICON layout for region {:?}", source.region),
            )
        })
    }
}

#[async_trait::async_trait]
impl Model for Icon {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        Self::layout(source)?;
        Ok(())
    }

    fn cycle(&self, _: &SourceDef) -> Cycle {
        Cycle {
            every: Duration::hours(i64::from(reg::RUN_EVERY_HOURS)),
            spare: 2,
            // A run lands over 36 (ICON-D2) to 50 (ICON-EU) minutes, a step
            // every minute or so; each look lists about ten directories.
            relist: Duration::minutes(2),
            give_up: Duration::minutes(45),
        }
    }

    fn leads(&self, source: &SourceDef, product: &str, run: DateTime<Utc>) -> Vec<u32> {
        let Some(l) = layout(source.region) else {
            return Vec::new();
        };
        if !run.hour().is_multiple_of(reg::RUN_EVERY_HOURS) {
            return Vec::new();
        }
        let last = l.last_hour(run);
        match (product, l) {
            (PRODUCT, _) => (0..=last).map(|h| h * 60).collect(),
            (SUBHOURLY, Layout::D2) => (0..=last * 60)
                .step_by(reg::D2_SUBHOURLY_STEP_MINUTES as usize)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn due(&self, source: &SourceDef, _: &str, run: DateTime<Utc>, lead: u32) -> DateTime<Utc> {
        layout(source.region)
            .unwrap_or(Layout::Eu)
            .arrival(run, file_hour(lead))
    }

    fn file(&self, source: &SourceDef, run: DateTime<Utc>, need: &Need) -> String {
        let l = layout(source.region).unwrap_or(Layout::Eu);
        let var = need.field.map_or("", |f| f.var);
        file_key(l, run, var, file_hour(need.lead))
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        source: &SourceDef,
        run: DateTime<Utc>,
        products: &[&'static str],
    ) -> Result<Option<Listing>, FetchError> {
        let l = Self::layout(source)?;
        let stamp = format!("_{PRODUCT}_{}_", run.format("%Y%m%d%H"));
        let mut listing = Listing::new();
        for var in variables(source, products) {
            let dir = var_dir(l, run, var);
            // A directory that does not exist holds nothing of the run.
            let Some(entries) = cx.list_html(&format!("{BASE}{dir}")).await? else {
                continue;
            };
            for e in entries
                .into_iter()
                .filter(|e| !e.is_dir && e.name.contains(&stamp))
            {
                listing.add(format!("{dir}{}", e.name), e.size);
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
        let url = format!("{BASE}{}", file.name);
        let Some(body) = cx.get(&url).max_bytes(MAX_FILE_BYTES).bytes().await? else {
            return Ok(Fetched::Missing);
        };
        let needs = needs.to_vec();
        let split = cx
            .cpu(move || split(&body, run, &needs))
            .await
            .map_err(|e| FetchError::object(&url, format!("{e:#}")))?;
        split
            .map(Fetched::Got)
            .map_err(|e| FetchError::object(&url, format!("{e:#}")))
    }
}

#[cfg(test)]
mod tests;
