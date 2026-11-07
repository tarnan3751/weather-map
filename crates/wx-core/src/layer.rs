//! The layer registry: every map layer is a row in [`LAYERS`].
//!
//! A layer is defined by what each family of sources contributes to it
//! (`inputs`), how decoded fields become stored values (`Derive` + `bands`),
//! and how the browser should draw it (`render` + `palette`). Adding a layer
//! means adding a row here and a palette in the web client; the pipeline, the
//! tile server and the client's renderer are all generic over this table.
//! The inputs themselves live in the family modules ([`crate::families`]);
//! a row here only names them.
//!
//! One layer has one timeline. Each time step is a mosaic: every source that
//! has data for that moment contributes a part, and where parts overlap the
//! higher-ranked source is on top (see `source` for ranks and `frame` for how
//! a time step is assembled).

use crate::families::{
    arome, cmorph, cptec, cwa, dmi, dpc, dwd_radar, gdps, gfs, gmgsi, goes, hrdps, hrrr, icon, ifs,
    jma, mrms, nbm, opera, rtma,
};
use crate::source::{Family, SourceDef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    Observed,
    Forecast,
}

/// Menu section the client files the layer under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerGroup {
    Precipitation,
    Temperature,
    Wind,
    Sky,
}

/// What a band measures. The client picks unit conversions from this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantity {
    /// Stored in degrees Celsius.
    Temperature,
    /// Stored in metres per second.
    Speed,
    /// Stored in dBZ.
    Reflectivity,
    /// Liquid-equivalent rate, stored in millimetres per hour.
    PrecipRate,
    /// Liquid depth, stored in millimetres.
    PrecipDepth,
    /// Depth of fallen snow, stored in millimetres.
    SnowDepth,
    /// Stored as 0..100.
    Percent,
    /// Integer codes, see the band's layer for their meaning.
    Category,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BandKind {
    /// Interpolate between samples.
    Continuous,
    /// Codes: always nearest-neighbour, never averaged.
    Categorical,
}

/// How to shrink a band when one output sample covers many source samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Aggregate {
    Mean,
    /// Keeps storm cores visible when zoomed out.
    Max,
    /// Categorical bands: take the nearest sample.
    Nearest,
}

/// One stored band and its quantization: `value = offset + (q - 1) * scale`
/// for a stored sample `q >= 1`; `q == 0` means no data.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct BandSpec {
    pub name: &'static str,
    pub quantity: Quantity,
    /// Unit of the stored (dequantized) value.
    pub unit: &'static str,
    pub scale: f32,
    pub offset: f32,
    pub kind: BandKind,
    pub aggregate: Aggregate,
}

impl BandSpec {
    /// Physical value to stored sample. NaN becomes no-data; out-of-range
    /// values clamp to the ends of the representable range.
    pub fn quantize(&self, v: f32) -> u16 {
        // NaN stays NaN through `clamp` and casts to 0, which is no-data;
        // without a branch, loops over a grid vectorize.
        const _: () = assert!(wx_tile::NODATA == 0);
        let q = ((v - self.offset) / self.scale).round() + 1.0;
        q.clamp(1.0, 65535.0) as u16
    }

    pub fn dequantize(&self, q: u16) -> Option<f32> {
        (q != wx_tile::NODATA).then(|| self.offset + (q - 1) as f32 * self.scale)
    }

    pub fn min_value(&self) -> f32 {
        self.offset
    }

    pub fn max_value(&self) -> f32 {
        self.offset + 65534.0 * self.scale
    }
}

/// How the client draws the layer. Numbers are band indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Render {
    /// Colour one band through the palette.
    Scalar { band: u8 },
    /// Colour by wind speed `hypot(u, v)` or by the gust band, with direction
    /// available for arrows and particles.
    Wind { u: u8, v: u8, gust: Option<u8> },
    /// Colour by rate, with one palette ramp per precipitation type code.
    PrecipType { rate: u8, ptype: u8 },
}

/// Which of a variable's time treatments is wanted. Model files often carry
/// the same variable and level several times (instantaneous, averaged over
/// the last hours, accumulated, a maximum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stat {
    /// The value at the valid time. In `.idx` terms the last column reads
    /// `anl`, `6 hour fcst` or `45 min fcst`.
    Instant,
    /// Accumulated from the start of the run to the valid time:
    /// `0-6 hour acc fcst`.
    RunTotal,
    /// The amount accumulated over the step that ends at the valid time,
    /// divided by the step's length in hours: in the field's own unit per
    /// hour (kg m-2, which is mm, becomes mm per hour; ECMWF's metres become
    /// metres per hour). `derive` receives it as an ordinary field, one per
    /// selector, and knows nothing of how it was made.
    ///
    /// The adapter makes it in one of two ways. Where the file holds a record
    /// for exactly that window, it takes that record (NBM's
    /// `APCP:surface:5-6 hour acc fcst` at step 6) and divides by the
    /// window's hours. Where the file holds only accumulations from the start
    /// of the run, it fetches the run total at the valid time and at the
    /// previous ingested step and takes `(total(t) - total(t - step)) /
    /// step hours`. The step is the family's ingest step: an hour everywhere
    /// in this registry. A cell missing in either total is missing; small
    /// negative differences (packing noise) are passed on as they are and the
    /// derive rules count negative amounts as 0. There is no value at step 0,
    /// so an input using it has no part at a run's step 0.
    StepPerHour,
    /// The maximum over the output interval that ends at the valid time, as
    /// the file defines the interval (the last hour for every input using it
    /// here). In `.idx` terms `2-3 hour max fcst`.
    StepMax,
}

/// One field to pull out of a source's files. `var` and `level` are in the
/// provider's own vocabulary, which each family module documents (see
/// [`crate::families`]): for NOAA files the `.idx` inventory's `:{var}:{level}:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FieldSel {
    pub var: &'static str,
    pub level: &'static str,
    pub stat: Stat,
}

/// Floor used for "radar is looking here and sees nothing". It is a real
/// value (not no-data) so the client can tell "no echo" from "no coverage".
pub const NO_ECHO_DBZ: f32 = -30.0;

/// Below this liquid-equivalent rate there is no precipitation type: the
/// precipitation-type rules give [`ptype::NONE`].
pub const TYPED_RATE_MIN_MMH: f32 = 0.01;

/// [`Derive::ReflectivityFromRate`]: below this rate the value is
/// [`NO_ECHO_DBZ`].
pub const ECHO_RATE_MIN_MMH: f32 = 0.1;
/// Marshall-Palmer `Z = A R^B`, Z in mm^6 m^-3 and R in mm/h. OPERA's rain
/// rates use the same relation (`zr_a` 200, `zr_b` 1.6 in its files).
pub const ZR_A: f32 = 200.0;
pub const ZR_B: f32 = 1.6;

/// [`Derive::PrecipTypeTemp`] thresholds on the 2 m temperature, Celsius (a
/// judgement: the 50 % snow line of surface observations lies around
/// 1 degree).
pub const SNOW_AT_OR_BELOW_C: f32 = 0.5;
pub const RAIN_AT_OR_ABOVE_C: f32 = 2.0;

/// [`Derive::PrecipPhases`] thresholds on the solid share of the total.
pub const RAIN_BELOW_SOLID_SHARE: f32 = 0.25;
pub const SNOW_ABOVE_SOLID_SHARE: f32 = 0.75;

/// Brightness temperatures outside this range, kelvin, are not real.
pub const BRIGHTNESS_MIN_K: f32 = 150.0;
pub const BRIGHTNESS_MAX_K: f32 = 350.0;

/// Precipitation type codes stored in the `ptype` band of the `precip` layer.
pub mod ptype {
    pub const NONE: f32 = 0.0;
    pub const RAIN: f32 = 1.0;
    pub const SNOW: f32 = 2.0;
    pub const SLEET: f32 = 3.0;
    pub const FREEZING_RAIN: f32 = 4.0;
    /// More than one type at once (for example rain and snow).
    pub const MIX: f32 = 5.0;
}

/// A provider's precipitation type codes, for [`Derive::PrecipTypeCode`].
/// [`PtypeTable::ptype`] is the mapping to [`ptype`] codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PtypeTable {
    /// ECCC's instantaneous precipitation type: `PTYPE_Sfc` (HRDPS) and
    /// `PrecipType-Instant_Sfc` (GDPS, RDPS). The files point at WMO table
    /// 4.201, but the values follow ECCC's own table, from the MSC open data
    /// variable lists (`HRDPS_Variables-List_en.csv` and
    /// `GDPS_Variables-List_en.csv` under
    /// `https://eccc-msc.github.io/open-data/assets/csv/`, "Instantaneous
    /// precipitation type"):
    ///
    /// | code | ECCC's name | ptype |
    /// |---|---|---|
    /// | 1 | rain | RAIN |
    /// | 2 | rain/snow | MIX |
    /// | 3 | freezing rain | FREEZING_RAIN |
    /// | 4 | ice pellets | SLEET |
    /// | 5 | snow | SNOW |
    /// | 6 | none | NONE |
    /// | 7 | drizzle | RAIN |
    /// | 8 | freezing drizzle | FREEZING_RAIN |
    /// | 9 | freezing rain/ice pellets | MIX |
    ///
    /// The 2026-11-06 12 UTC +3 h files agree with it: where it is wet, the
    /// median 2 m temperature is 8 C for code 1, 1.3 C for 2, -0.9 C for 3,
    /// 0.5 C for 4, -3.6 C for 5, 9.7 C for 7, -2.0 C for 8 and -0.2 C for 9;
    /// the dominant phase of HRDPS's own amounts over the hour before
    /// (`WEARN`, `WEASN`, `WEAFR`, `WEAPE`) is rain at 99 % of code-1 cells,
    /// snow at 98 % of code-5 cells and freezing rain at 74 % of code-3
    /// cells, and code 2 splits 40/56 between rain and snow.
    Eccc,
    /// WMO code table 4677, present weather `ww` (ICON's `ww`, 0..99). Codes
    /// 0-49 carry no precipitation at the time: NONE. Drizzle and rain
    /// (50-55, 58-65, 80-82) RAIN; freezing drizzle and freezing rain (56-57,
    /// 66-67) FREEZING_RAIN; rain or drizzle with snow (68-69) MIX; snow,
    /// snow grains, ice prisms and snow crystals (70-78) SNOW; ice pellets
    /// (79) SLEET; rain and snow showers (83-84) MIX; snow showers (85-86)
    /// SNOW; snow pellets or small hail (87-88) MIX; hail showers (89-90)
    /// RAIN; rain with thunder in the past hour (91-92) RAIN; snow, mixed or
    /// hail with thunder in the past hour (93-94) MIX; thunderstorms (95-97,
    /// 99) RAIN; thunderstorm with dust or sand (98) NONE.
    ///
    /// ICON produces a subset. Checked on 2026-11-06 (ICON-D2 18 UTC and
    /// ICON-EU 15 UTC, +3 h and +6 h) against the 2 m temperature and the
    /// hour's precipitation and its snow share: 0-3 and 45/48 (fog) carry
    /// none; 51-55, 61-65, 80-81 and 95-96 are rain (snow share 0, median
    /// 2 m T 2.9 to 18.8 C); 56-57 and 66 freezing drizzle and freezing rain
    /// (median 2 m T -0.6 to 2.3 C); 71-77 and 85-86 snow (snow share 0.59 to
    /// 1.0, median 2 m T -2.1 to 4.6 C), with ICON-D2's own snow rate well
    /// above its rain rate there. 68-69, 79, 83-84 and 87-94 did not occur
    /// and keep their WMO meaning.
    WmoWw,
    /// ECMWF's `ptype` (WMO code table 4.201 as the IFS uses it): 0 NONE,
    /// 1 rain RAIN, 3 freezing rain FREEZING_RAIN, 5 snow SNOW, 6 wet snow
    /// SNOW, 7 rain and snow MIX, 8 ice pellets SLEET, 11 drizzle RAIN,
    /// 12 freezing drizzle FREEZING_RAIN. Codes of the WMO table the IFS
    /// does not produce map by their meaning: 2 thunderstorm RAIN, 4 mixed or
    /// ice MIX, 9 graupel SNOW, 10 hail SLEET. The names are from the WMO
    /// table (UNVERIFIED against the files' own metadata); the sample held 0,
    /// 1, 3 and others up to 12.
    Ecmwf,
}

impl PtypeTable {
    /// The [`ptype`] code for provider code `code`; NONE for a code the
    /// table does not list.
    pub const fn ptype(self, code: i32) -> f32 {
        use ptype::*;
        match self {
            PtypeTable::Eccc => match code {
                1 | 7 => RAIN,
                2 | 9 => MIX,
                3 | 8 => FREEZING_RAIN,
                4 => SLEET,
                5 => SNOW,
                _ => NONE,
            },
            PtypeTable::WmoWw => match code {
                50..=55 | 58..=65 | 80..=82 | 89..=92 | 95..=97 | 99 => RAIN,
                56 | 57 | 66 | 67 => FREEZING_RAIN,
                68 | 69 | 83 | 84 | 87 | 88 | 93 | 94 => MIX,
                70..=78 | 85 | 86 => SNOW,
                79 => SLEET,
                _ => NONE,
            },
            PtypeTable::Ecmwf => match code {
                1 | 2 | 11 => RAIN,
                3 | 12 => FREEZING_RAIN,
                5 | 6 | 9 => SNOW,
                4 | 7 => MIX,
                8 | 10 => SLEET,
                _ => NONE,
            },
        }
    }
}

/// How a source's decoded fields (in `InputSpec::fields` order) become the
/// layer's bands. Decoded values arrive in the file's native units (for HDF5
/// and NetCDF files, after the reader applies `scale_factor`, `add_offset`
/// and the fill value). NaN is missing throughout: a rule that needs a field
/// at a cell gives missing there when that field is missing, unless the rule
/// says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Derive {
    /// One field to one band: `value * mul + add`.
    Linear { mul: f32, add: f32 },
    /// Radar reflectivity in dBZ to one band. Missing stays missing; MRMS's
    /// "no coverage" sentinel (-999; treat values <= -900 as such) becomes
    /// missing. "No echo" becomes `NO_ECHO_DBZ`: that is MRMS's -99, HRRR's
    /// -10 and GFS's -20, so any value at or below -10 dBZ is treated as no
    /// echo. Real echoes weaker than that are not meteorologically useful.
    /// (Adapters of radar composites map their own "covered, nothing seen"
    /// value to -99 and "outside coverage" to NaN before this rule.)
    Reflectivity,
    /// Accumulated liquid precipitation in millimetres (kg/m^2) to one band.
    /// Negative values (MRMS uses -3 for "no coverage") become missing.
    PrecipDepth,
    /// Fields `[UGRD, VGRD, GUST]` in m/s to bands `[u, v, gust]`. `u`/`v`
    /// are rotated to earth-relative when the grid flags them grid-relative.
    Wind,
    /// Fields `[TMP K, DPT K, UGRD, VGRD]` to one band of apparent temperature
    /// in Celsius: NWS wind chill at or below 50 F with wind above 3 mph;
    /// otherwise the NWS heat index procedure (the simple formula, switching
    /// to the Rothfusz regression with its dry and humid adjustments once the
    /// result reaches 80 F) where that exceeds the air temperature; otherwise
    /// the air temperature.
    FeelsLike,
    /// Fields `[PRATE kg/m^2/s, CRAIN, CSNOW, CICEP, CFRZR]` to bands
    /// `[rate mm/h, ptype]`. The type is NONE where the rate is under
    /// 0.01 mm/h or no flag is set; otherwise the single flag that is set, or
    /// MIX when several are. See [`ptype`].
    PrecipTypeRate,
    /// Fields `[speed m/s, direction degrees, gust m/s]` to bands
    /// `[u, v, gust]`. `direction` is where the wind blows from, clockwise
    /// from north: `u = -speed * sin(direction)`, `v = -speed *
    /// cos(direction)`. North is true north, unless the grid flags its winds
    /// grid-relative, in which case direction is measured from the grid's +y
    /// axis and `u`/`v` are then rotated to earth-relative exactly as in
    /// `Wind`. A missing or negative speed, or a missing direction, makes `u`
    /// and `v` missing; the gust band is the third field as it is.
    WindSpeedDir,
    /// Fields `[TMP K, DPT K, wind speed m/s]` to one band: `FeelsLike`, with
    /// the wind speed taken from the third field instead of `hypot(u, v)`.
    FeelsLikeSpeed,
    /// Fields `[TMP K, RH %, UGRD, VGRD]` to one band: `FeelsLike`, with the
    /// relative humidity the heat index needs taken from the second field
    /// (clamped to 0..100) instead of computed from a dew point.
    FeelsLikeRh,
    /// Fields `[TMP K, RH %]` to one band of dew point in Celsius, by the
    /// Magnus form with Bolton's constants (the inverse of the relative
    /// humidity the heat index uses): with `t` the temperature in Celsius
    /// and `rh` clamped to 1..100, `g = ln(rh / 100) + 17.67 t / (t +
    /// 243.5)` and `td = 243.5 g / (17.67 - g)`.
    DewPointFromRh,
    /// Field `[rate]` to one band of reflectivity in dBZ. `R = rate *
    /// to_mmh` is the liquid-equivalent rate in mm/h (`to_mmh` is 3600 for
    /// kg m-2 s-1 and 1 for mm/h or for a `StepPerHour` amount in kg m-2);
    /// negative R counts as 0. Below [`ECHO_RATE_MIN_MMH`] (0.1 mm/h) the
    /// value is [`NO_ECHO_DBZ`]; otherwise `Z = ZR_A * R^ZR_B`
    /// (Marshall-Palmer, 200 and 1.6) and `dBZ = 10 log10(Z)`, which is
    /// 23.01 + 16 log10(R): 7.0 dBZ at 0.1 mm/h, 23 at 1, 39 at 10, 55 at
    /// 100. A rate averaged over an hour is smoother than an instantaneous
    /// one, so storm cores come out weaker than a model's own reflectivity
    /// would show; that is accepted.
    ReflectivityFromRate { to_mmh: f32 },
    /// Fields `[rate, TMP 2 m K]` to bands `[rate mm/h, ptype]`. The rate band
    /// is `max(rate * to_mmh, 0)`. The type is NONE below
    /// [`TYPED_RATE_MIN_MMH`]; otherwise, with `t` the 2 m temperature in
    /// Celsius, SNOW where `t <=` [`SNOW_AT_OR_BELOW_C`] (0.5), RAIN where
    /// `t >=` [`RAIN_AT_OR_ABOVE_C`] (2.0), and MIX between. A missing rate
    /// makes both bands missing; a missing temperature makes the type missing
    /// where the rate is at or above the threshold. Freezing rain and sleet
    /// depend on the air aloft, so this rule never gives them.
    PrecipTypeTemp { to_mmh: f32 },
    /// Fields `[rate, code]` to bands `[rate mm/h, ptype]`. The rate band as
    /// in `PrecipTypeTemp`. The type is NONE below [`TYPED_RATE_MIN_MMH`];
    /// otherwise `table.ptype(code)` with the code rounded to the nearest
    /// integer (a code the table does not list gives NONE, as the flags rule
    /// does when no flag is set). A missing rate makes both bands missing; a
    /// missing code makes the type missing where the rate is at or above the
    /// threshold. The code is instantaneous at the valid time even when the
    /// rate is a step amount, so an hour whose rain stopped before the valid
    /// time shows a rate with type NONE.
    PrecipTypeCode { table: PtypeTable, to_mmh: f32 },
    /// Fields `[total, solid]`, amounts or rates of all precipitation and of
    /// its solid part, in the same unit, to bands `[rate mm/h, ptype]`. The
    /// rate band is `max(total * to_mmh, 0)`. The type is NONE below
    /// [`TYPED_RATE_MIN_MMH`]; otherwise, with `s = clamp(solid / total, 0,
    /// 1)` the solid share, RAIN where `s <` [`RAIN_BELOW_SOLID_SHARE`]
    /// (0.25), SNOW where `s >` [`SNOW_ABOVE_SOLID_SHARE`] (0.75), and MIX
    /// between. A missing total makes both bands missing; a missing solid part
    /// makes the type missing where the rate is at or above the threshold.
    PrecipPhases { to_mmh: f32 },
    /// One or more fields in the same unit to one band:
    /// `(f0 + f1 + ...) * mul + add`.
    Sum { mul: f32, add: f32 },
    /// Field `[brightness temperature K]` to one band in Celsius, `K -
    /// 273.15`. Values outside [`BRIGHTNESS_MIN_K`]..=[`BRIGHTNESS_MAX_K`]
    /// (150-350 K) are missing.
    BrightnessTemp,
    /// Field `[count]`, an infrared image in McIDAS brightness counts 0..255
    /// (cold is high), to one band of brightness temperature in Celsius. With
    /// `b` the count as it is (not rounded): `T = 330 - b / 2` kelvin for
    /// `b <= 176` and `T = 418 - b` for `b > 176`, so 0 is 330 K, 176 is
    /// 242 K and 255 is 163 K; then `T - 273.15`. Counts outside 0..=255 are
    /// missing.
    McidasIrCount,
}

impl Derive {
    /// How many fields the rule takes, or `None` for `Sum` (one or more).
    /// Single-message products may list no fields at all (the decoder then
    /// returns the file's one field).
    pub const fn field_count(&self) -> Option<usize> {
        Some(match self {
            Derive::Linear { .. }
            | Derive::Reflectivity
            | Derive::PrecipDepth
            | Derive::ReflectivityFromRate { .. }
            | Derive::BrightnessTemp
            | Derive::McidasIrCount => 1,
            Derive::DewPointFromRh
            | Derive::PrecipTypeTemp { .. }
            | Derive::PrecipTypeCode { .. }
            | Derive::PrecipPhases { .. } => 2,
            Derive::Wind | Derive::WindSpeedDir | Derive::FeelsLikeSpeed => 3,
            Derive::FeelsLike | Derive::FeelsLikeRh => 4,
            Derive::PrecipTypeRate => 5,
            Derive::Sum { .. } => return None,
        })
    }

    /// How many bands the rule makes.
    pub const fn band_count(&self) -> usize {
        match self {
            Derive::Wind | Derive::WindSpeedDir => 3,
            Derive::PrecipTypeRate
            | Derive::PrecipTypeTemp { .. }
            | Derive::PrecipTypeCode { .. }
            | Derive::PrecipPhases { .. } => 2,
            _ => 1,
        }
    }
}

/// What one family of sources (or some of its sources) contributes to a
/// layer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct InputSpec {
    pub family: Family,
    /// The ids of the sources of `family` this input applies to; empty means
    /// all of them. A family whose sources differ in what they carry (one
    /// region lacks a field, an observation and its nowcast use different
    /// products) lists one input per group on the same layer, with disjoint
    /// `sources`; at most one input of a layer applies to any source.
    pub sources: &'static [&'static str],
    /// Family-specific product name: the MRMS product directory, the model
    /// output family (`wrfsfc`, `wrfsubh`, `2dvaranl_ndfd`, `pgrb2.0p25`),
    /// the radar product, the file package. The family's module says which.
    pub product: &'static str,
    /// Fields to decode, in the order `derive` expects them. Empty for
    /// products that are one field per file (MRMS, radar composites).
    pub fields: &'static [FieldSel],
    pub derive: Derive,
    /// How long one part stands in for later time steps, in minutes: at least
    /// the source's own step. A part valid at `v` can appear in the frames
    /// for `v <= t < v + hold`.
    pub hold_minutes: u32,
    /// Forecast inputs only. True when the model's recent hours are the best
    /// "current conditions" there are outside the areas observations cover,
    /// so its parts stay on the past part of the timeline (temperature). False
    /// when past forecasts would only confuse (simulated radar next to real
    /// radar).
    pub fill_past: bool,
}

impl InputSpec {
    /// This input, limited to the sources named.
    pub const fn only(self, sources: &'static [&'static str]) -> Self {
        InputSpec { sources, ..self }
    }

    /// Whether this input is what `source` contributes (when it is on the
    /// layer at all).
    pub fn applies_to(&self, source: &SourceDef) -> bool {
        self.family == source.family
            && (self.sources.is_empty() || self.sources.contains(&source.id))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LayerSpec {
    /// Stable, URL-safe id.
    pub id: &'static str,
    pub title: &'static str,
    pub group: LayerGroup,
    pub description: &'static str,
    pub bands: &'static [BandSpec],
    pub render: Render,
    /// Default palette id; palettes themselves live in the web client.
    pub palette: &'static str,
    /// What each family (or group of a family's sources) feeds this layer.
    pub inputs: &'static [InputSpec],
    /// How far back the timeline reaches (the loop window), in minutes.
    /// Observed parts, and forecast parts of `fill_past` inputs, older than
    /// this are dropped.
    pub retention_minutes: u32,
    /// Observed parts land on time steps that are multiples of this many
    /// minutes (a part valid at 12:05 on a 2-minute layer first shows at
    /// 12:06). Forecast parts make a time step at exactly their valid time.
    pub observed_tick_minutes: u32,
    /// Accumulations only make sense from a common start, so every part on
    /// the timeline comes from model runs that started at the same moment
    /// (the anchor), and the whole timeline moves on together when the next
    /// set of runs is in. See `frame`.
    pub anchored: bool,
}

impl LayerSpec {
    /// The first input of `family` on this layer. When the family's inputs
    /// are limited to particular sources, [`LayerSpec::input_for`] is the one
    /// that answers for a given source.
    pub fn input(&self, family: Family) -> Option<&'static InputSpec> {
        self.inputs.iter().find(|s| s.family == family)
    }

    /// What `source` contributes to this layer, if anything.
    pub fn input_for(&self, source: &SourceDef) -> Option<&'static InputSpec> {
        self.inputs.iter().find(|s| s.applies_to(source))
    }

    pub fn has_observations(&self) -> bool {
        crate::source::SOURCES
            .iter()
            .any(|s| s.kind == FrameKind::Observed && self.input_for(s).is_some())
    }
}

pub fn layer(id: &str) -> Option<&'static LayerSpec> {
    LAYERS.iter().find(|l| l.id == id)
}

/// Every input of `family`, with its layer. A layer appears once per input,
/// so more than once when the family's inputs there are limited to
/// particular sources.
pub fn layers_for(
    family: Family,
) -> impl Iterator<Item = (&'static LayerSpec, &'static InputSpec)> {
    LAYERS.iter().flat_map(move |l| {
        l.inputs
            .iter()
            .filter(move |s| s.family == family)
            .map(move |s| (l, s))
    })
}

// ---------------------------------------------------------------------------
// Bands
// ---------------------------------------------------------------------------

const fn continuous(
    name: &'static str,
    quantity: Quantity,
    unit: &'static str,
    scale: f32,
    offset: f32,
) -> BandSpec {
    BandSpec {
        name,
        quantity,
        unit,
        scale,
        offset,
        kind: BandKind::Continuous,
        aggregate: Aggregate::Mean,
    }
}

const DBZ: BandSpec = BandSpec {
    aggregate: Aggregate::Max,
    ..continuous("dbz", Quantity::Reflectivity, "dBZ", 0.1, -40.0)
};
const TEMP_C: BandSpec = continuous("t", Quantity::Temperature, "degC", 0.01, -100.0);
/// Infrared brightness temperature, with the temperature quantization:
/// cloud tops colder than -100 C clamp to -100.
const TB_C: BandSpec = continuous("tb", Quantity::Temperature, "degC", 0.01, -100.0);
const WIND_U: BandSpec = continuous("u", Quantity::Speed, "m/s", 0.01, -150.0);
const WIND_V: BandSpec = continuous("v", Quantity::Speed, "m/s", 0.01, -150.0);
const WIND_GUST: BandSpec = continuous("gust", Quantity::Speed, "m/s", 0.01, 0.0);
const RATE_MMH: BandSpec = continuous("rate", Quantity::PrecipRate, "mm/h", 0.01, 0.0);
const DEPTH_MM: BandSpec = continuous("depth", Quantity::PrecipDepth, "mm", 0.05, 0.0);
const SNOW_MM: BandSpec = continuous("depth", Quantity::SnowDepth, "mm", 0.1, 0.0);
const CLOUD_PCT: BandSpec = continuous("cover", Quantity::Percent, "%", 0.01, 0.0);
const PTYPE: BandSpec = BandSpec {
    name: "ptype",
    quantity: Quantity::Category,
    unit: "",
    scale: 1.0,
    offset: 0.0,
    kind: BandKind::Categorical,
    aggregate: Aggregate::Nearest,
};

/// Two hours of radar, per the product brief.
const RADAR_LOOP_MIN: u32 = 120;
/// How much history the hourly layers keep.
const HOURLY_LOOP_MIN: u32 = 12 * 60;
/// Three hours of satellite imagery.
const SATELLITE_LOOP_MIN: u32 = 180;

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

/// Every layer, in menu order. The first layer's first input stays MRMS's
/// unrestricted radar input: the frame tests build their own inputs from it.
pub static LAYERS: &[LayerSpec] = &[
    LayerSpec {
        id: "radar",
        title: "Radar",
        group: LayerGroup::Precipitation,
        description: "Live composite radar, continuing into simulated future radar",
        bands: &[DBZ],
        render: Render::Scalar { band: 0 },
        palette: "reflectivity",
        inputs: &[
            mrms::RADAR,
            opera::RADAR,
            dpc::RADAR,
            jma::RADAR,
            cwa::RADAR,
            dwd_radar::RADAR,
            cmorph::RADAR,
            dwd_radar::NOWCAST_RADAR,
            jma::NOWCAST_RADAR,
            hrrr::RADAR,
            hrdps::RADAR,
            icon::RADAR_D2,
            icon::RADAR_EU,
            dmi::RADAR,
            arome::RADAR,
            nbm::RADAR,
            cptec::RADAR,
            gfs::RADAR,
        ],
        retention_minutes: RADAR_LOOP_MIN,
        observed_tick_minutes: 2,
        anchored: false,
    },
    LayerSpec {
        id: "precip",
        title: "Precipitation type",
        group: LayerGroup::Precipitation,
        description: "Forecast rain, snow, sleet and freezing rain, with intensity",
        bands: &[RATE_MMH, PTYPE],
        render: Render::PrecipType { rate: 0, ptype: 1 },
        palette: "precip_type",
        inputs: &[
            hrrr::PRECIP,
            hrdps::PRECIP,
            icon::PRECIP,
            dmi::PRECIP,
            arome::PRECIP,
            nbm::PRECIP,
            cptec::PRECIP,
            ifs::PRECIP,
            gdps::PRECIP,
            gfs::PRECIP,
        ],
        retention_minutes: 0,
        observed_tick_minutes: 60,
        anchored: false,
    },
    LayerSpec {
        id: "rain_1h",
        title: "Rain, past hour",
        group: LayerGroup::Precipitation,
        description: "Estimated rainfall over the past hour",
        bands: &[DEPTH_MM],
        render: Render::Scalar { band: 0 },
        palette: "rainfall",
        inputs: &[mrms::RAIN_1H, opera::RAIN_1H, cmorph::RAIN_1H],
        retention_minutes: 180,
        observed_tick_minutes: 10,
        anchored: false,
    },
    LayerSpec {
        id: "rain_24h",
        title: "Rain, past 24 hours",
        group: LayerGroup::Precipitation,
        description: "Estimated rainfall over the past 24 hours",
        bands: &[DEPTH_MM],
        render: Render::Scalar { band: 0 },
        palette: "rainfall",
        inputs: &[mrms::RAIN_24H, cmorph::RAIN_24H],
        retention_minutes: 180,
        observed_tick_minutes: 10,
        anchored: false,
    },
    LayerSpec {
        id: "rain_72h",
        title: "Rain, past 72 hours",
        group: LayerGroup::Precipitation,
        description: "Estimated rainfall over the past 72 hours",
        bands: &[DEPTH_MM],
        render: Render::Scalar { band: 0 },
        palette: "rainfall",
        inputs: &[mrms::RAIN_72H, cmorph::RAIN_72H],
        retention_minutes: 180,
        observed_tick_minutes: 10,
        anchored: false,
    },
    LayerSpec {
        id: "snow",
        title: "Snowfall",
        group: LayerGroup::Precipitation,
        description: "Forecast snowfall, accumulating from the start of the latest model runs",
        bands: &[SNOW_MM],
        render: Render::Scalar { band: 0 },
        palette: "snowfall",
        inputs: &[
            hrrr::SNOW,
            hrdps::SNOW,
            icon::SNOW_D2,
            icon::SNOW_EU,
            dmi::SNOW,
            arome::SNOW,
            ifs::SNOW,
            gdps::SNOW,
        ],
        retention_minutes: 0,
        observed_tick_minutes: 60,
        anchored: true,
    },
    LayerSpec {
        id: "temp",
        title: "Temperature",
        group: LayerGroup::Temperature,
        description: "Air temperature two metres above the ground",
        bands: &[TEMP_C],
        render: Render::Scalar { band: 0 },
        palette: "temperature",
        inputs: &[
            rtma::TEMP,
            hrrr::TEMP,
            hrdps::TEMP,
            icon::TEMP,
            dmi::TEMP,
            arome::TEMP,
            nbm::TEMP,
            cptec::TEMP,
            ifs::TEMP,
            gdps::TEMP,
            gfs::TEMP,
        ],
        retention_minutes: HOURLY_LOOP_MIN,
        observed_tick_minutes: 60,
        anchored: false,
    },
    LayerSpec {
        id: "feels_like",
        title: "Feels like",
        group: LayerGroup::Temperature,
        description: "Wind chill when it is cold, heat index when it is hot",
        bands: &[TEMP_C],
        render: Render::Scalar { band: 0 },
        palette: "temperature",
        inputs: &[
            rtma::FEELS_LIKE,
            hrrr::FEELS_LIKE,
            hrdps::FEELS_LIKE,
            icon::FEELS_LIKE,
            dmi::FEELS_LIKE,
            arome::FEELS_LIKE,
            nbm::FEELS_LIKE,
            cptec::FEELS_LIKE,
            ifs::FEELS_LIKE,
            gdps::FEELS_LIKE,
            gfs::FEELS_LIKE,
        ],
        retention_minutes: HOURLY_LOOP_MIN,
        observed_tick_minutes: 60,
        anchored: false,
    },
    LayerSpec {
        id: "dew_point",
        title: "Dew point",
        group: LayerGroup::Temperature,
        description: "How humid the air is: the temperature at which dew forms",
        bands: &[TEMP_C],
        render: Render::Scalar { band: 0 },
        palette: "dew_point",
        inputs: &[
            rtma::DEW_POINT,
            hrrr::DEW_POINT,
            hrdps::DEW_POINT,
            icon::DEW_POINT,
            dmi::DEW_POINT,
            arome::DEW_POINT,
            nbm::DEW_POINT,
            cptec::DEW_POINT,
            ifs::DEW_POINT,
            gdps::DEW_POINT,
            gfs::DEW_POINT,
        ],
        retention_minutes: HOURLY_LOOP_MIN,
        observed_tick_minutes: 60,
        anchored: false,
    },
    LayerSpec {
        id: "wind",
        title: "Wind and gusts",
        group: LayerGroup::Wind,
        description: "Wind ten metres above the ground, and peak gusts",
        bands: &[WIND_U, WIND_V, WIND_GUST],
        render: Render::Wind {
            u: 0,
            v: 1,
            gust: Some(2),
        },
        palette: "wind",
        inputs: &[
            rtma::WIND,
            hrrr::WIND,
            hrdps::WIND,
            icon::WIND,
            dmi::WIND,
            arome::WIND,
            nbm::WIND,
            cptec::WIND,
            ifs::WIND,
            gdps::WIND,
            gfs::WIND,
        ],
        retention_minutes: HOURLY_LOOP_MIN,
        observed_tick_minutes: 60,
        anchored: false,
    },
    LayerSpec {
        id: "clouds",
        title: "Cloud cover",
        group: LayerGroup::Sky,
        description: "Forecast share of the sky covered by cloud",
        bands: &[CLOUD_PCT],
        render: Render::Scalar { band: 0 },
        palette: "clouds",
        inputs: &[
            hrrr::CLOUDS,
            hrdps::CLOUDS,
            icon::CLOUDS,
            dmi::CLOUDS,
            arome::CLOUDS,
            nbm::CLOUDS,
            cptec::CLOUDS,
            ifs::CLOUDS,
            gdps::CLOUDS,
            gfs::CLOUDS,
        ],
        retention_minutes: 0,
        observed_tick_minutes: 60,
        anchored: false,
    },
    LayerSpec {
        id: "satellite",
        title: "Satellite",
        group: LayerGroup::Sky,
        description: "Infrared satellite imagery: the colder, the higher the cloud tops",
        bands: &[TB_C],
        render: Render::Scalar { band: 0 },
        palette: "infrared",
        inputs: &[goes::SATELLITE, gmgsi::SATELLITE],
        retention_minutes: SATELLITE_LOOP_MIN,
        observed_tick_minutes: 10,
        anchored: false,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::families;
    use crate::source::{SOURCES, sources_of};
    use std::collections::HashSet;

    #[test]
    fn quantization_round_trips_within_half_a_step() {
        for band in [TEMP_C, TB_C, WIND_U, DBZ, DEPTH_MM, CLOUD_PCT] {
            for v in [band.min_value(), -12.34, 0.0, 0.5, 37.77, 99.99] {
                let v = v.clamp(band.min_value(), band.max_value());
                let q = band.quantize(v);
                assert_ne!(q, wx_tile::NODATA);
                let back = band.dequantize(q).unwrap();
                assert!(
                    (back - v).abs() <= band.scale * 0.51,
                    "{}: {v} -> {q} -> {back}",
                    band.name
                );
            }
            assert_eq!(band.quantize(f32::NAN), wx_tile::NODATA);
            assert_eq!(band.dequantize(wx_tile::NODATA), None);
            assert_eq!(band.quantize(f32::MAX), 65535);
            assert_eq!(band.quantize(f32::MIN), 1);
        }
    }

    #[test]
    fn ptype_codes_survive_quantization() {
        for code in [
            ptype::NONE,
            ptype::RAIN,
            ptype::SNOW,
            ptype::SLEET,
            ptype::FREEZING_RAIN,
            ptype::MIX,
        ] {
            assert_eq!(PTYPE.dequantize(PTYPE.quantize(code)), Some(code));
        }
    }

    #[test]
    fn code_tables() {
        use PtypeTable::*;
        let valid = [
            ptype::NONE,
            ptype::RAIN,
            ptype::SNOW,
            ptype::SLEET,
            ptype::FREEZING_RAIN,
            ptype::MIX,
        ];
        for table in [Eccc, WmoWw, Ecmwf] {
            for code in -1..=300 {
                assert!(valid.contains(&table.ptype(code)), "{table:?} {code}");
            }
        }
        let cases = [
            (Eccc, 1, ptype::RAIN),
            (Eccc, 3, ptype::FREEZING_RAIN),
            (Eccc, 5, ptype::SNOW),
            (Eccc, 6, ptype::NONE),
            (WmoWw, 0, ptype::NONE),
            (WmoWw, 45, ptype::NONE),
            (WmoWw, 53, ptype::RAIN),
            (WmoWw, 56, ptype::FREEZING_RAIN),
            (WmoWw, 63, ptype::RAIN),
            (WmoWw, 67, ptype::FREEZING_RAIN),
            (WmoWw, 73, ptype::SNOW),
            (WmoWw, 79, ptype::SLEET),
            (WmoWw, 81, ptype::RAIN),
            (WmoWw, 86, ptype::SNOW),
            (WmoWw, 95, ptype::RAIN),
            (Ecmwf, 0, ptype::NONE),
            (Ecmwf, 1, ptype::RAIN),
            (Ecmwf, 3, ptype::FREEZING_RAIN),
            (Ecmwf, 5, ptype::SNOW),
            (Ecmwf, 6, ptype::SNOW),
            (Ecmwf, 7, ptype::MIX),
            (Ecmwf, 8, ptype::SLEET),
            (Ecmwf, 12, ptype::FREEZING_RAIN),
        ];
        for (table, code, want) in cases {
            assert_eq!(table.ptype(code), want, "{table:?} {code}");
        }
    }

    /// Sources a layer's input applies to.
    fn sources_on(input: &InputSpec) -> Vec<&'static SourceDef> {
        SOURCES.iter().filter(|s| input.applies_to(s)).collect()
    }

    #[test]
    fn registry_is_consistent() {
        let mut ids = HashSet::new();
        for l in LAYERS {
            assert!(ids.insert(l.id), "duplicate layer id {}", l.id);
            assert!(
                l.id.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "layer id {} is not URL-safe",
                l.id
            );
            assert!(!l.inputs.is_empty() && !l.bands.is_empty());
            assert!(l.observed_tick_minutes > 0);
            let bands = l.bands.len() as u8;
            match l.render {
                Render::Scalar { band } => assert!(band < bands),
                Render::Wind { u, v, gust } => {
                    assert!(u < bands && v < bands && gust.is_none_or(|g| g < bands))
                }
                Render::PrecipType { rate, ptype } => assert!(rate < bands && ptype < bands),
            }
            for s in l.inputs {
                let what = format!("{} ({} {:?})", l.id, s.family.as_str(), s.sources);
                assert!(s.hold_minutes > 0, "{what}: an input needs a hold");
                assert!(!s.product.is_empty(), "{what}: no product");
                assert_eq!(l.bands.len(), s.derive.band_count(), "{what}: band count");
                match s.derive.field_count() {
                    Some(n) => assert!(
                        s.fields.is_empty() && n == 1 || s.fields.len() == n,
                        "{what}: field count"
                    ),
                    None => assert!(!s.fields.is_empty(), "{what}: field count"),
                }
                // Every input reaches at least one source, and names only
                // sources of its own family.
                assert!(
                    sources_of(s.family).next().is_some(),
                    "{what}: family has no source"
                );
                for id in s.sources {
                    let d = crate::source::source(id);
                    assert!(
                        d.is_some_and(|d| d.family == s.family),
                        "{what}: {id} is not of the family"
                    );
                }
                let on = sources_on(s);
                assert!(!on.is_empty(), "{what}: applies to no source");
                let observed = on.iter().any(|d| d.kind == FrameKind::Observed);
                let forecast = on.iter().any(|d| d.kind == FrameKind::Forecast);
                assert!(
                    !(observed && s.fill_past),
                    "{what}: fill_past is for forecasts"
                );
                assert!(
                    !(l.anchored && observed),
                    "{what}: anchored layers take model runs only"
                );
                if l.anchored {
                    assert!(
                        s.fields.iter().all(|f| f.stat == Stat::RunTotal),
                        "{what}: anchored inputs are accumulations from the run start"
                    );
                }
                // Model inputs fill the past on the temperature and wind
                // layers and nowhere else.
                let fills = matches!(l.id, "temp" | "feels_like" | "dew_point" | "wind");
                if forecast && !observed && s.family != Family::Jma && s.family != Family::DwdRadar
                {
                    assert_eq!(s.fill_past, fills, "{what}: fill_past");
                }
                // Statistics that need a run (a previous step, an output
                // interval) belong to models.
                if observed {
                    assert!(
                        s.fields.iter().all(|f| matches!(f.stat, Stat::Instant)),
                        "{what}: observations take instantaneous fields"
                    );
                }
            }
            // At most one input of a layer applies to any source.
            for src in SOURCES {
                let n = l.inputs.iter().filter(|s| s.applies_to(src)).count();
                assert!(n <= 1, "{}: {} inputs apply to {}", l.id, n, src.id);
            }
        }
        assert!(layer("radar").is_some() && layer("nope").is_none());
        assert_eq!(layers_for(Family::Mrms).count(), 4);
        assert!(layer("temp").unwrap().has_observations());
        assert!(layer("satellite").unwrap().has_observations());
        assert!(!layer("clouds").unwrap().has_observations());
        assert!(!layer("precip").unwrap().has_observations());
    }

    /// Every source feeds at least one layer.
    #[test]
    fn every_source_has_an_input() {
        for s in SOURCES {
            assert!(
                LAYERS.iter().any(|l| l.input_for(s).is_some()),
                "{} feeds no layer",
                s.id
            );
        }
    }

    /// Limited inputs pick the right source, and the first layer's first
    /// input (which the frame tests copy) applies to a whole family.
    #[test]
    fn inputs_limited_to_sources() {
        let radar = layer("radar").unwrap();
        assert!(LAYERS[0].inputs[0].sources.is_empty());
        assert_eq!(LAYERS[0].inputs[0].family, Family::Mrms);
        let product = |id: &str| {
            radar
                .input_for(crate::source::source(id).unwrap())
                .map(|i| i.product)
        };
        assert_eq!(product("dwd_radar"), Some("hx"));
        assert_eq!(product("dwd_nowcast"), Some("wn"));
        assert_eq!(product("icon_d2"), Some(icon::RADAR_D2.product));
        assert_eq!(
            radar.input_for(&icon::ICON_EU).unwrap().derive,
            icon::RADAR_EU.derive
        );
        assert_eq!(radar.input_for(&jma::JMA_NOWCAST).unwrap().hold_minutes, 5);
        assert!(layer("snow").unwrap().input_for(&gfs::GFS).is_none());
        assert!(radar.input_for(&gdps::GDPS).is_none());
        assert_eq!(
            layers_for(Family::Icon)
                .filter(|(l, _)| l.id == "radar")
                .count(),
            2
        );
    }

    /// Holds follow the rules: a model's largest ingested step; an
    /// observation's cadence twice over plus its latency, at least.
    #[test]
    fn holds() {
        for l in LAYERS {
            for s in l.inputs {
                for d in sources_on(s) {
                    let min = match (d.family, l.id) {
                        (Family::Gfs | Family::Ifs, _) => 180,
                        (Family::Hrrr, "radar") | (Family::Icon, "radar") if d.id != "icon_eu" => {
                            15
                        }
                        (Family::Jma | Family::DwdRadar, _) if d.kind == FrameKind::Forecast => 5,
                        (Family::Cmorph, _) => 150,
                        (Family::Gmgsi, _) => 160,
                        (Family::Rtma, _) => 160,
                        (_, _) if d.kind == FrameKind::Forecast => 60,
                        _ => 1,
                    };
                    assert!(
                        s.hold_minutes >= min,
                        "{} {}: hold {} < {min}",
                        l.id,
                        d.id,
                        s.hold_minutes
                    );
                }
            }
        }
    }

    #[test]
    fn disabled_families_still_have_complete_rows() {
        for (family, _) in families::ROWS.iter().filter(|(f, _)| !f.enabled()) {
            let layers: Vec<_> = layers_for(*family).map(|(l, _)| l.id).collect();
            for id in [
                "precip",
                "snow",
                "temp",
                "feels_like",
                "dew_point",
                "wind",
                "clouds",
            ] {
                assert!(layers.contains(&id), "{}: no {id} input", family.as_str());
            }
        }
    }

    #[test]
    fn satellite_layer() {
        let l = layer("satellite").unwrap();
        assert_eq!(l.group, LayerGroup::Sky);
        assert_eq!(l.palette, "infrared");
        assert_eq!((l.observed_tick_minutes, l.retention_minutes), (10, 180));
        assert_eq!(l.bands[0].quantity, Quantity::Temperature);
        assert_eq!(
            (l.bands[0].scale, l.bands[0].offset),
            (TEMP_C.scale, TEMP_C.offset)
        );
    }
}
