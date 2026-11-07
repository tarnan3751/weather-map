//! The families this crate can fetch. Each module implements
//! [`Model`](crate::driver::Model) or [`Observed`](crate::driver::Observed);
//! [`adapter`] is the one place each is registered.

pub mod common;
pub mod gfs;
pub mod hrrr;
pub mod mrms;
pub mod rtma;

// Families behind cargo features (see Cargo.toml). Each `pub mod` line and
// each arm in `adapter` carries its feature's `cfg`.

#[cfg(feature = "arome")]
pub mod arome;
#[cfg(feature = "satellite")]
pub mod cmorph;
#[cfg(feature = "cptec")]
pub mod cptec;
#[cfg(feature = "radar")]
pub mod cwa;
#[cfg(feature = "dmi")]
pub mod dmi;
#[cfg(feature = "radar")]
pub mod dpc;
#[cfg(feature = "radar")]
pub mod dwd_radar;
#[cfg(feature = "eccc")]
pub mod eccc;
#[cfg(feature = "eccc")]
pub mod gdps;
#[cfg(feature = "satellite")]
pub mod gmgsi;
#[cfg(feature = "satellite")]
pub mod goes;
#[cfg(feature = "eccc")]
pub mod hrdps;
#[cfg(feature = "icon")]
pub mod icon;
#[cfg(feature = "radar")]
pub mod jma;
#[cfg(all(test, any(feature = "icon", feature = "dmi")))]
mod live_check;
#[cfg(feature = "nbm")]
pub mod nbm;
#[cfg(feature = "radar")]
pub mod opera;

use crate::IngestConfig;
use crate::driver::{Model, Observed};
use std::sync::Arc;
#[cfg(feature = "radar")]
use wx_core::FrameKind;
use wx_core::{Family, SourceDef};

/// What follows a source.
#[derive(Clone)]
pub enum Adapter {
    Model(Arc<dyn Model>),
    Observed(Arc<dyn Observed>),
}

/// The adapter for `source`, or `None` while nobody has written its
/// family's. A family with observed and nowcast sources (DWD radar, JMA)
/// has one of each, chosen by the source's kind.
#[allow(unreachable_patterns)]
pub fn adapter(source: &SourceDef, config: &IngestConfig) -> Option<Adapter> {
    Some(match source.family {
        Family::Mrms => Adapter::Observed(Arc::new(mrms::Mrms)),
        Family::Rtma => Adapter::Observed(Arc::new(rtma::Rtma)),
        Family::Hrrr => Adapter::Model(Arc::new(hrrr::Hrrr::new(config.hrrr_max_hour))),
        Family::Gfs => Adapter::Model(Arc::new(gfs::Gfs)),
        #[cfg(feature = "nbm")]
        Family::Nbm => Adapter::Model(Arc::new(nbm::Nbm)),
        #[cfg(feature = "cptec")]
        Family::Cptec => Adapter::Model(Arc::new(cptec::Cptec)),
        #[cfg(feature = "eccc")]
        Family::Gdps => Adapter::Model(Arc::new(gdps::Gdps::default())),
        #[cfg(feature = "eccc")]
        Family::Hrdps => Adapter::Model(Arc::new(hrdps::Hrdps::default())),
        #[cfg(feature = "satellite")]
        Family::Cmorph => Adapter::Observed(Arc::new(cmorph::Cmorph::default())),
        #[cfg(feature = "satellite")]
        Family::Gmgsi => Adapter::Observed(Arc::new(gmgsi::Gmgsi)),
        #[cfg(feature = "satellite")]
        Family::Goes => Adapter::Observed(Arc::new(goes::Goes)),
        #[cfg(feature = "icon")]
        Family::Icon => Adapter::Model(Arc::new(icon::Icon)),
        #[cfg(feature = "dmi")]
        Family::Dmi => Adapter::Model(Arc::new(dmi::Dmi)),
        #[cfg(feature = "arome")]
        Family::Arome => Adapter::Model(Arc::new(arome::Arome::default())),
        #[cfg(feature = "radar")]
        Family::Opera => Adapter::Observed(Arc::new(opera::Opera::default())),
        #[cfg(feature = "radar")]
        Family::DwdRadar if source.kind == FrameKind::Observed => {
            Adapter::Observed(Arc::new(dwd_radar::DwdRadar::default()))
        }
        #[cfg(feature = "radar")]
        Family::DwdRadar => Adapter::Model(Arc::new(dwd_radar::DwdNowcast::default())),
        #[cfg(feature = "radar")]
        Family::Dpc => Adapter::Observed(Arc::new(dpc::Dpc::default())),
        #[cfg(feature = "radar")]
        Family::Jma if source.kind == FrameKind::Observed => {
            Adapter::Observed(Arc::new(jma::JmaRadar::default()))
        }
        #[cfg(feature = "radar")]
        Family::Jma => Adapter::Model(Arc::new(jma::JmaNowcast::default())),
        #[cfg(feature = "radar")]
        Family::Cwa => Adapter::Observed(Arc::new(cwa::Cwa::default())),
        _ => return None,
    })
}
