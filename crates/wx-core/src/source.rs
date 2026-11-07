//! The source registry: every place data comes from is a row in [`SOURCES`].
//!
//! A source is one provider's product over one region: "MRMS over Alaska",
//! "ICON-EU", "HRRR". Sources that are fetched and decoded the same way share
//! a [`Family`]; the layer registry says what each family contributes to each
//! layer (`LayerSpec::inputs`). Everything about a family (its rows, its
//! inputs, where its data lives, its licence) is in its module under
//! [`crate::families`]; this file only assembles the rows.
//!
//! `rank` decides who is on top where sources overlap, and is unique. Roughly:
//! observations (radar 920-950) over analyses (800s) over radar nowcasts
//! (700s) over convection-permitting models (580-660) over regional models
//! (400s) over global models (100-200), with satellite imagery (60s-70s) and
//! satellite rainfall (50) at the bottom. Where two grids of similar quality
//! overlap, the module of the higher-ranked one says why it wins, and whether
//! its adapter masks part of its grid so the other shows. See `frame` for how
//! a time step is assembled from the ranked parts.

use crate::families::{
    arome, cmorph, cptec, cwa, dmi, dpc, dwd_radar, gdps, gfs, gmgsi, goes, hrdps, hrrr, icon, ifs,
    jma, mrms, nbm, opera, rtma,
};
use crate::layer::FrameKind;
use serde::Serialize;

/// Sources that share a fetcher and a file layout. Each has a module of the
/// same name under [`crate::families`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    /// NOAA Multi-Radar Multi-Sensor mosaics, 0.5-1 km, every 2 minutes.
    Mrms,
    /// NOAA Real-Time Mesoscale Analysis, 1.25-3 km, hourly.
    Rtma,
    /// NOAA High-Resolution Rapid Refresh, 3 km, CONUS and Alaska.
    Hrrr,
    /// NOAA Global Forecast System, 0.25 degree, four runs a day.
    Gfs,
    /// NOAA National Blend of Models, 1.25-3 km, hourly cycles.
    Nbm,
    /// ECCC Global Deterministic Prediction System, 15 km, hourly.
    Gdps,
    /// ECCC High Resolution Deterministic Prediction System, 2.5 km.
    Hrdps,
    /// DWD ICON-EU (6.5 km) and ICON-D2 (2.2 km).
    Icon,
    /// DMI HARMONIE DINI and IG, 2 km.
    Dmi,
    /// Météo-France AROME 0.025 degree, France and five overseas domains.
    Arome,
    /// CPTEC/INPE WRF over South America, 7 km.
    Cptec,
    /// ECMWF IFS open data, 0.1 degree, 3-hourly. Defined, not ingested yet.
    Ifs,
    /// NOAA CPC CMORPH2 satellite rainfall, 0.25 degree, every 30 minutes.
    Cmorph,
    /// EUMETNET OPERA European radar composites.
    Opera,
    /// Italian radar composite (Protezione Civile).
    Dpc,
    /// JMA precipitation nowcast tiles: observed and +60 minutes.
    Jma,
    /// Taiwan CWA composite reflectivity.
    Cwa,
    /// DWD radar composites: observed and +120 minutes.
    DwdRadar,
    /// NOAA/NESDIS global geostationary infrared mosaic, hourly.
    Gmgsi,
    /// GOES-19 and GOES-18 band 13 full disks, every 10 minutes.
    Goes,
}

impl Family {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Family::Mrms => "mrms",
            Family::Rtma => "rtma",
            Family::Hrrr => "hrrr",
            Family::Gfs => "gfs",
            Family::Nbm => "nbm",
            Family::Gdps => "gdps",
            Family::Hrdps => "hrdps",
            Family::Icon => "icon",
            Family::Dmi => "dmi",
            Family::Arome => "arome",
            Family::Cptec => "cptec",
            Family::Ifs => "ifs",
            Family::Cmorph => "cmorph",
            Family::Opera => "opera",
            Family::Dpc => "dpc",
            Family::Jma => "jma",
            Family::Cwa => "cwa",
            Family::DwdRadar => "dwd_radar",
            Family::Gmgsi => "gmgsi",
            Family::Goes => "goes",
        }
    }

    pub fn parse(s: &str) -> Option<Family> {
        SOURCES.iter().map(|d| d.family).find(|f| f.as_str() == s)
    }

    /// Whether the ingest side fetches this family yet. See
    /// [`crate::families::enabled`].
    pub const fn enabled(&self) -> bool {
        crate::families::enabled(*self)
    }
}

/// One row of the source registry.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SourceDef {
    /// Stable id, `[a-z0-9_]+`, at most 16 bytes. Part ids start with it.
    pub id: &'static str,
    pub family: Family,
    /// Family-specific name of the region or domain: the bucket directory,
    /// file-name token or grid name the fetcher needs (the family's module
    /// says which). Empty when the family has only one region.
    pub region: &'static str,
    /// Short name for people: "HRRR", "MRMS Alaska".
    pub title: &'static str,
    /// Who publishes it: "NOAA", "DWD".
    pub provider: &'static str,
    /// Credit line the map must show while this source is in view.
    pub attribution: &'static str,
    /// Where the credit links to: the provider, or the licence when the
    /// licence asks for a link to itself.
    pub attribution_url: &'static str,
    /// The licence the data is used under, by name ("CC BY 4.0"). The
    /// catalogue in `docs/sources/README.md` has the terms behind each.
    pub licence: &'static str,
    pub kind: FrameKind,
    /// Higher is drawn on top. Unique.
    pub rank: u16,
    /// Nominal ground resolution, kilometres.
    pub res_km: f32,
    /// Deepest zoom tiles are rendered for from this source; beyond it the
    /// client magnifies.
    pub max_zoom: u8,
}

/// What kind of instrument or system a source's values come from, for
/// wording a reading ("radar sees no echo" is not what a satellite estimate
/// or a model means by the same number).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Medium {
    /// Ground radar looking at the place.
    Radar,
    /// Estimated or imaged from orbit.
    Satellite,
    /// An analysis of observations onto a grid.
    Analysis,
    /// Radar echoes moved forward in time.
    Nowcast,
    /// A numerical weather model.
    Model,
}

impl SourceDef {
    pub fn medium(&self) -> Medium {
        match (self.family, self.kind) {
            (Family::Cmorph | Family::Gmgsi | Family::Goes, _) => Medium::Satellite,
            (Family::Rtma, _) => Medium::Analysis,
            (
                Family::Mrms
                | Family::Opera
                | Family::Dpc
                | Family::Jma
                | Family::Cwa
                | Family::DwdRadar,
                FrameKind::Observed,
            ) => Medium::Radar,
            (Family::Jma | Family::DwdRadar, FrameKind::Forecast) => Medium::Nowcast,
            _ => Medium::Model,
        }
    }
}

/// Every source. The first row stays MRMS CONUS: the frame tests build their
/// own rows from it.
pub static SOURCES: &[SourceDef] = &[
    // Observations.
    mrms::MRMS,
    mrms::MRMS_AK,
    mrms::MRMS_HI,
    mrms::MRMS_CARIB,
    mrms::MRMS_GUAM,
    opera::OPERA,
    dpc::DPC,
    jma::JMA_RADAR,
    cwa::CWA,
    dwd_radar::DWD_RADAR,
    cmorph::CMORPH,
    gmgsi::GMGSI,
    goes::GOES19,
    goes::GOES18,
    // Analyses.
    rtma::RTMA,
    rtma::RTMA_AK,
    rtma::RTMA_HI,
    rtma::RTMA_PR,
    rtma::RTMA_GU,
    // Radar nowcasts.
    dwd_radar::DWD_NOWCAST,
    jma::JMA_NOWCAST,
    // Convection-permitting models.
    hrrr::HRRR,
    hrrr::HRRR_AK,
    hrdps::HRDPS,
    icon::ICON_D2,
    dmi::DINI,
    dmi::IG,
    arome::AROME_FR,
    arome::AROME_ANTILLES,
    arome::AROME_GUYANE,
    arome::AROME_INDIEN,
    arome::AROME_NCALED,
    arome::AROME_POLYN,
    nbm::NBM_CO,
    nbm::NBM_AK,
    nbm::NBM_HI,
    nbm::NBM_PR,
    nbm::NBM_GU,
    // Regional models.
    icon::ICON_EU,
    cptec::CPTEC,
    // Global models.
    ifs::IFS,
    gdps::GDPS,
    gfs::GFS,
];

pub fn source(id: &str) -> Option<&'static SourceDef> {
    SOURCES.iter().find(|s| s.id == id)
}

/// Every source of a family.
pub fn sources_of(family: Family) -> impl Iterator<Item = &'static SourceDef> {
    SOURCES.iter().filter(move |s| s.family == family)
}

/// Ordering used everywhere parts are stacked: higher rank first.
pub fn rank_of(source_id: &str) -> u16 {
    source(source_id).map_or(0, |s| s.rank)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::families;
    use std::collections::HashSet;

    #[test]
    fn registry_is_consistent() {
        let mut ids = HashSet::new();
        let mut ranks = HashSet::new();
        for s in SOURCES {
            assert!(ids.insert(s.id), "duplicate source id {}", s.id);
            assert!(
                ranks.insert(s.rank),
                "{}: rank {} is used twice",
                s.id,
                s.rank
            );
            assert!(
                !s.id.is_empty()
                    && s.id.len() <= 16
                    && s.id
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "source id {} must be short and URL-safe",
                s.id
            );
            assert!(s.max_zoom <= crate::proj::MAX_ZOOM);
            assert!(s.res_km > 0.0, "{}: resolution", s.id);
            for (what, v) in [
                ("title", s.title),
                ("provider", s.provider),
                ("attribution", s.attribution),
                ("licence", s.licence),
            ] {
                assert!(!v.is_empty(), "{}: empty {what}", s.id);
            }
            assert!(
                s.attribution_url.starts_with("https://"),
                "{}: attribution URL",
                s.id
            );
            assert_eq!(Family::parse(s.family.as_str()), Some(s.family));
        }
        assert!(source("hrrr").is_some() && source("nope").is_none());
        assert_eq!(sources_of(Family::Mrms).count(), 5);
        assert_eq!(rank_of("nope"), 0);
    }

    #[test]
    fn family_names_are_unique_and_url_safe() {
        let mut names = HashSet::new();
        for (family, _) in families::ROWS {
            let n = family.as_str();
            assert!(names.insert(n), "family name {n} is used twice");
            assert!(
                n.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            );
        }
    }

    /// `SOURCES` holds exactly the rows the family modules list, each under
    /// its own family.
    #[test]
    fn every_family_module_row_is_listed() {
        let mut listed = 0;
        let mut seen = HashSet::new();
        for (family, rows) in families::ROWS {
            assert!(
                seen.insert(*family),
                "{} is in families::ROWS twice",
                family.as_str()
            );
            assert!(!rows.is_empty(), "{} has no rows", family.as_str());
            for row in *rows {
                assert_eq!(
                    row.family,
                    *family,
                    "{} is filed under {}",
                    row.id,
                    family.as_str()
                );
                assert_eq!(
                    source(row.id),
                    Some(row),
                    "{} is missing from SOURCES or differs",
                    row.id
                );
                listed += 1;
            }
        }
        assert_eq!(
            listed,
            SOURCES.len(),
            "SOURCES has rows no family module lists"
        );
    }

    /// The frame tests build their registry from `SOURCES[0]` and rely on the
    /// original four families.
    #[test]
    fn rows_the_frame_tests_rely_on() {
        assert_eq!(SOURCES[0].id, "mrms");
        for f in [Family::Mrms, Family::Rtma, Family::Hrrr, Family::Gfs] {
            assert!(sources_of(f).next().is_some());
        }
    }

    #[test]
    fn only_ifs_waits() {
        for (family, _) in families::ROWS {
            assert_eq!(
                family.enabled(),
                *family != Family::Ifs,
                "{}",
                family.as_str()
            );
        }
    }

    /// Stacking choices that the family modules explain.
    #[test]
    fn overlaps_resolve_as_documented() {
        let above = |a: &str, b: &str| assert!(rank_of(a) > rank_of(b), "{a} must rank above {b}");
        // The Alaska grids reach Washington and British Columbia.
        above("rtma", "rtma_ak");
        above("nbm_co", "nbm_ak");
        // DINI's domain contains ICON-D2's.
        above("icon_d2", "dini");
        above("icon_d2", "arome_fr");
        above("arome_fr", "dini");
        // HRDPS masks the US out of its grid (hrdps::MASK_OUT).
        above("hrdps", "hrrr");
        above("hrdps", "hrrr_ak");
        assert!(!hrdps::MASK_OUT.is_empty());
        // Nowcasts above every model, below every observation but CMORPH.
        for s in SOURCES {
            if s.kind == FrameKind::Forecast
                && s.family != Family::Jma
                && s.family != Family::DwdRadar
            {
                above("dwd_nowcast", s.id);
                above("jma_nowcast", s.id);
            }
        }
        // CMORPH under every model, so models cover it ahead of now.
        for s in SOURCES.iter().filter(|s| s.kind == FrameKind::Forecast) {
            above(s.id, "cmorph");
        }
    }
}
