//! The seams between the generic machinery and the families.
//!
//! A family is a file that implements [`Model`] (sources that publish model
//! runs: HRRR, GFS, ICON, a radar nowcast issued every five minutes) or
//! [`Observed`] (sources that publish one file per observation time: MRMS,
//! RTMA, a satellite product). It says what exists and how to get one file's
//! data; the drivers in [`model`] and [`observed`] do everything else:
//! deciding what the catalog wants, scheduling, listing and polling,
//! retrying, charging broken files, decoding on build threads under the
//! shared limits, step differencing, storing grids and inserting parts.
//!
//! # The unit of work
//!
//! A part is one input of one layer at one lead of one run (or one
//! observation time). It needs one [`Need`] per field of the input, each
//! found in some file the family names ([`Model::file`]). Needs that name
//! the same file are fetched together, in one call to the family's `fetch`,
//! so one file can feed many parts (every layer reading an HRRR file; the
//! four 15-minute steps in one sub-hourly file; 25 nowcast leads in one tar)
//! and one part can draw on several files (ICON's one variable per file).
//! A part is built as soon as every one of its needs has arrived.
//!
//! # Step accumulations
//!
//! A [`Stat::StepPerHour`] field is the amount accumulated over the step
//! ending at the valid time, per hour. A family says per product how its
//! files hold it ([`Steps`]). Most hold run totals only: the driver then asks
//! for the run total at the part's lead and at the previous lead the family
//! lists for the product ([`Model::leads`]), and hands `derive` their
//! difference divided by the hours between them (missing where either is
//! missing; small negative differences are passed on, and `derive` counts
//! them as nothing). At the first lead the previous total is zero by
//! definition and is not fetched; otherwise the newest total fetched of
//! each field is kept while the run is wanted, so a step whose end lands a
//! cycle after its start does not fetch the start again. Files that hold
//! the hour's own record
//! (NBM's `5-6 hour acc fcst`) are asked for it as it is. Either way a run's
//! lead 0 has no step, and no part.
//!
//! # Decoding
//!
//! A family returns each need as a [`Payload`]: GRIB2 bytes (optionally
//! gzip or bzip2 compressed) whose fields are named NOAA's way (NOAA, DMI,
//! CPTEC), which the build step decodes with `wx_pipeline::decode`; or bytes
//! and a [`Decoder`] of the family's own, which the build step runs on a
//! blocking thread under the shared build limit. [`grib_picking`] makes one
//! for GRIB2 named any other way (ICON, ECMWF, AROME, ECCC); decoders for
//! HDF5, GeoTIFF or NetCDF come from `wx-formats`. Payload bytes are
//! [`Bytes`]: a file holding many needs is shared, not copied.

pub mod model;
pub mod observed;

pub use model::{Cycle, Listing, Model, ModelFile, Steps};
pub use observed::{Cadence, ObsFile, Observed};

use crate::idx::{self, ByteRange, IdxEntry};
use crate::remote::{FetchError, Get, HtmlEntry, Listed, Remote, S3Bucket, codec};
use anyhow::{Context, ensure};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::{StreamExt, TryStreamExt};
use std::sync::Arc;
use wx_core::layer::{FieldSel, Stat};
use wx_core::{Field, InputSpec, LayerSpec, SourceDef};

/// Gaps up to this size between wanted messages are downloaded rather than
/// paying for another request.
pub const MERGE_GAP: u64 = 64 * 1024;
/// Ranged requests in flight for one file.
const RANGES_PER_FILE: usize = 4;

/// One field of one input at one lead: something to fetch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Need {
    pub input: &'static InputSpec,
    /// The field, `None` for inputs without selectors (the file is the
    /// field, as for MRMS). A [`Stat::StepPerHour`] selector arrives as
    /// [`Stat::RunTotal`] unless the family's [`Steps`] for the product is
    /// [`Steps::Windows`]; see "Step accumulations" in the module docs.
    pub field: Option<FieldSel>,
    /// Minutes after the run; 0 for observations.
    pub lead: u32,
}

impl Need {
    pub fn product(&self) -> &'static str {
        self.input.product
    }

    /// The same field of the same product at the same lead: the same bytes,
    /// whichever input asks.
    pub fn same_as(&self, other: &Need) -> bool {
        self.product() == other.product() && self.field == other.field && self.lead == other.lead
    }

    /// A run total asked for in place of a step accumulation, which the
    /// next lead's step starts at.
    pub(crate) fn is_step_total(&self) -> bool {
        self.field.is_some_and(|f| {
            f.stat == Stat::RunTotal
                && self
                    .input
                    .fields
                    .iter()
                    .any(|s| is_step(s.stat) && s.var == f.var && s.level == f.level)
        })
    }
}

/// Decodes one need's bytes into its field and, when the bytes say, the
/// time it is valid at. Runs on a build thread.
pub type Decoder = Arc<dyn Fn(&[u8], &Need) -> anyhow::Result<Decoded> + Send + Sync>;

/// A field decoded by a family [`Decoder`].
#[derive(Debug, Clone)]
pub struct Decoded {
    pub field: Field,
    pub valid: Option<DateTime<Utc>>,
}

/// One need's data, as the family fetched it.
#[derive(Clone)]
pub enum Payload {
    /// GRIB2 messages holding the field (others may be there too, as long as
    /// only one matches the need), plain, gzip or bzip2.
    Grib(Bytes),
    /// Bytes for a family decoder.
    Custom { bytes: Bytes, decode: Decoder },
}

impl Payload {
    /// A copy holding only its own bytes, not the buffer they may be a
    /// slice of, for keeping.
    pub(crate) fn detached(&self) -> Payload {
        match self {
            Payload::Grib(b) => Payload::Grib(Bytes::copy_from_slice(b)),
            Payload::Custom { bytes, decode } => Payload::Custom {
                bytes: Bytes::copy_from_slice(bytes),
                decode: decode.clone(),
            },
        }
    }
}

impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Payload::Grib(b) => write!(f, "Grib({} bytes)", b.len()),
            Payload::Custom { bytes, .. } => write!(f, "Custom({} bytes)", bytes.len()),
        }
    }
}

impl Payload {
    /// Wraps bytes for `decode`, as [`Cx::whole`] takes it.
    pub fn decoded_by(decode: Decoder) -> impl Fn(Bytes) -> Payload {
        move |bytes| Payload::Custom {
            bytes,
            decode: decode.clone(),
        }
    }
}

/// What came of fetching one file.
#[derive(Debug)]
pub enum Fetched {
    /// The file (or its index) is not there yet. Its parts wait.
    Missing,
    /// One entry per need, in order. `None` where the file does not hold the
    /// need: that part can never be made from this file and is given up.
    Got(Vec<Option<Payload>>),
}

impl Fetched {
    /// `payload` for every need.
    pub fn all(needs: &[Need], payload: Payload) -> Fetched {
        Fetched::Got(vec![Some(payload); needs.len()])
    }
}

/// The layers a source feeds in `layers` (a layer registry), with the input
/// that applies to it, in registry order.
pub fn inputs_of(
    layers: &'static [LayerSpec],
    source: &SourceDef,
) -> Vec<(&'static LayerSpec, &'static InputSpec)> {
    layers
        .iter()
        .filter_map(|l| l.input_for(source).map(|i| (l, i)))
        .collect()
}

/// The products those inputs read, each once, in registry order.
pub fn products_of(layers: &'static [LayerSpec], source: &SourceDef) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for (_, input) in inputs_of(layers, source) {
        if !out.contains(&input.product) {
            out.push(input.product);
        }
    }
    out
}

/// What a family's code works with. Requests made through it queue as the
/// task's (see `remote`), so its downloads take turns with other sources'.
pub struct Cx<'a> {
    remote: &'a Remote,
    lane: &'a str,
    /// The time this cycle runs at.
    pub now: DateTime<Utc>,
}

impl<'a> Cx<'a> {
    pub fn new(remote: &'a Remote, lane: &'a str, now: DateTime<Utc>) -> Self {
        Self { remote, lane, now }
    }

    /// Start a GET (see [`Remote::get`]).
    pub fn get(&self, url: impl Into<String>) -> Get<'a> {
        self.remote.get(url).lane(self.lane)
    }

    /// Start a POST (see [`Remote::post`]).
    pub fn post(
        &self,
        url: impl Into<String>,
        content_type: &str,
        body: impl Into<Bytes>,
    ) -> Get<'a> {
        self.remote.post(url, content_type, body).lane(self.lane)
    }

    /// See [`Remote::list_s3`].
    pub async fn list_s3(
        &self,
        bucket: &S3Bucket,
        prefix: &str,
        start_after: Option<&str>,
    ) -> Result<Vec<Listed>, FetchError> {
        self.remote
            .list_s3(self.lane, bucket, prefix, start_after)
            .await
    }

    /// See [`Remote::list_html`].
    pub async fn list_html(&self, url: &str) -> Result<Option<Vec<HtmlEntry>>, FetchError> {
        self.remote.list_html(self.lane, url).await
    }

    /// Run CPU-bound work (inflating, splitting an archive) on a blocking
    /// thread rather than the async ones.
    pub async fn cpu<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> anyhow::Result<T> {
        tokio::task::spawn_blocking(f)
            .await
            .map_err(|e| anyhow::anyhow!("blocking task failed: {e}"))
    }

    /// The whole file at `url`, as the payload of every need (`wrap` says
    /// how: [`Payload::Grib`] or [`Payload::decoded_by`]).
    pub async fn whole(
        &self,
        url: &str,
        needs: &[Need],
        max_bytes: u64,
        wrap: impl Fn(Bytes) -> Payload,
    ) -> Result<Fetched, FetchError> {
        Ok(match self.get(url).max_bytes(max_bytes).bytes().await? {
            None => Fetched::Missing,
            Some(b) => Fetched::all(needs, wrap(b)),
        })
    }

    /// [`Cx::grib_with_idx`] for an inventory named after its file and
    /// `sidecar` (`.idx`, `.index`).
    pub async fn grib_with_sidecar(
        &self,
        url: &str,
        sidecar: &str,
        size: Option<u64>,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        self.grib_with_idx(url, &format!("{url}{sidecar}"), size, needs)
            .await
    }

    /// The needs of a GRIB2 file with a NOAA-style `.idx` (or wgrib2-style
    /// `.index`) at `index_url`: read the inventory, then fetch each wanted
    /// message once. `size`, when known from a listing, lets an inventory
    /// that does not belong to the file be caught before any range is asked
    /// for. A missing inventory means the file is not ready.
    pub async fn grib_with_idx(
        &self,
        url: &str,
        index_url: &str,
        size: Option<u64>,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        self.grib_with_index(url, index_url, size, needs, idx::parse)
            .await
    }

    /// [`Cx::grib_with_idx`] with the inventory read by `parse`.
    pub async fn grib_with_index(
        &self,
        url: &str,
        index_url: &str,
        size: Option<u64>,
        needs: &[Need],
        parse: impl Fn(&str) -> Vec<IdxEntry>,
    ) -> Result<Fetched, FetchError> {
        let Some(text) = self
            .get(index_url)
            .max_bytes(16 * 1024 * 1024)
            .bytes()
            .await?
        else {
            return Ok(Fetched::Missing);
        };
        let inventory = parse(&String::from_utf8_lossy(&text));
        self.grib_by_inventory(url, &inventory, size, needs).await
    }

    /// The needs of a GRIB2 file whose inventory is already known (parsed
    /// from any index format into [`IdxEntry`] values). Each need's field is
    /// looked up at its lead in minutes ([`IdxEntry::matches`]); one not in
    /// the inventory gives `None`. Neighbouring messages share a request.
    /// Bytes that are not a GRIB message where the inventory says one starts
    /// mean the inventory does not describe the file: [`Failure::Object`].
    ///
    /// [`Failure::Object`]: crate::remote::Failure::Object
    pub async fn grib_by_inventory(
        &self,
        url: &str,
        inventory: &[IdxEntry],
        size: Option<u64>,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let ranges: Vec<Option<ByteRange>> = needs
            .iter()
            .map(|n| {
                let sel = n.field.as_ref()?;
                idx::find(inventory, sel, n.lead).map(|e| e.range.bounded(size))
            })
            .collect();
        let wanted: Vec<ByteRange> = ranges.iter().flatten().copied().collect();
        if wanted.is_empty() {
            return Ok(Fetched::Got(vec![None; needs.len()]));
        }
        // An inventory naming bytes past the end of the file does not belong
        // to it; asking would only earn a 416 or a short read.
        if let Some(s) = size
            && let Some(r) = wanted
                .iter()
                .find(|r| r.start >= s || r.end.is_some_and(|e| e > s))
        {
            return Err(FetchError::object(
                url,
                format!(
                    "the inventory names bytes {}..{:?} but the file has {s}",
                    r.start, r.end
                ),
            ));
        }
        let merged = idx::coalesce(&wanted, MERGE_GAP);
        let fetched: Vec<Option<(ByteRange, Bytes)>> = futures::stream::iter(merged)
            .map(|r| async move {
                Ok::<_, FetchError>(self.get(url).range(r).bytes().await?.map(|b| (r, b)))
            })
            .buffered(RANGES_PER_FILE)
            .try_collect()
            .await?;
        let Some(fetched) = fetched.into_iter().collect::<Option<Vec<_>>>() else {
            return Ok(Fetched::Missing);
        };
        let mut out = Vec::with_capacity(needs.len());
        for r in ranges {
            let Some(r) = r else {
                out.push(None);
                continue;
            };
            let part = idx::slice(&fetched, r).ok_or_else(|| {
                FetchError::object(
                    url,
                    format!("the response does not cover bytes {r:?} named by the inventory"),
                )
            })?;
            if !part.starts_with(b"GRIB") {
                return Err(FetchError::object(
                    url,
                    format!(
                        "bytes at {} are not a GRIB message: the inventory does not match the file",
                        r.start
                    ),
                ));
            }
            out.push(Some(Payload::Grib(part)));
        }
        Ok(Fetched::Got(out))
    }
}

/// A [`Decoder`] for GRIB2 whose fields are not named NOAA's way (ICON's
/// variable directories, ECMWF's params, AROME's short names): `pick` says
/// which field of [`wx_pipeline::scan`]'s list holds the need, from its
/// parameter numbers, level and time. The bytes may be gzip or bzip2. One
/// field must be picked; several with the same reference time, valid time
/// and grid count as one (the first is taken), as NOAA files repeat some.
pub fn grib_picking(
    pick: impl Fn(&wx_pipeline::MessageInfo, &Need) -> bool + Send + Sync + 'static,
) -> Decoder {
    Arc::new(move |bytes: &[u8], need: &Need| {
        let bytes = codec::inflate(bytes, codec::MAX_INFLATED)?;
        let infos = wx_pipeline::scan(&bytes).context("scanning the GRIB2 messages")?;
        let hits: Vec<usize> = (0..infos.len())
            .filter(|&k| pick(&infos[k], need))
            .collect();
        let what = || match need.field {
            Some(f) => format!("{}:{} at lead {}", f.var, f.level, need.lead),
            None => format!("the field at lead {}", need.lead),
        };
        let first = *hits
            .first()
            .with_context(|| format!("no field of {} is {}", infos.len(), what()))?;
        let same = |k: &usize| {
            let (a, b) = (&infos[*k], &infos[first]);
            a.reference_time == b.reference_time
                && a.valid_time() == b.valid_time()
                && a.grid == b.grid
        };
        ensure!(
            hits.iter().all(same),
            "{} fields are {} at different times or on different grids",
            hits.len(),
            what()
        );
        let mut decoded = wx_pipeline::decode_messages(&bytes, &[first])?;
        ensure!(
            decoded.len() == 1,
            "decoded {} fields for one",
            decoded.len()
        );
        let m = decoded.remove(0);
        Ok(Decoded {
            valid: Some(m.time.valid()),
            field: m.field,
        })
    })
}

/// Whether a field is accumulated over the step ending at the valid time.
pub(crate) fn is_step(stat: Stat) -> bool {
    stat == Stat::StepPerHour
}

/// The selector a family is asked for: see "Step accumulations".
pub(crate) fn fetched_as(sel: FieldSel, steps: Steps) -> FieldSel {
    if is_step(sel.stat) && steps == Steps::RunTotals {
        FieldSel {
            stat: Stat::RunTotal,
            ..sel
        }
    } else {
        sel
    }
}

#[cfg(test)]
mod tests;
