//! Shared contracts for the weather map.
//!
//! * [`source`]: the source registry. One row per provider product and region.
//! * [`layer`]: the layer registry. One row per map layer.
//! * [`families`]: one module per family of sources, holding its rows, its
//!   inputs and everything known about where its data lives.
//! * [`proj`]: Web-Mercator tiles and model grid projections.
//! * [`grid`]: decoded fields and rasters.
//! * [`frame`]: parts, frames and the catalog that assembles each timeline.
//! * [`events`]: live-update events and the hub that fans them out.
//! * [`feed`]: supervised background feeds and their health.
//! * [`keys`]: object-store key layout.
//! * [`upstream`]: a shared back-off switch for a third-party host.

pub mod events;
pub mod families;
pub mod feed;
pub mod frame;
pub mod grid;
pub mod keys;
pub mod layer;
pub mod proj;
pub mod source;
pub mod upstream;

pub use events::{Hub, WsEvent};
pub use feed::{Backoff, FeedCtx, FeedHealth, FeedState, HealthRegistry, spawn_feed};
pub use frame::{Frame, FrameCatalog, Insert, Part, Timeline};
pub use grid::{Field, Raster};
pub use layer::{BandSpec, FrameKind, InputSpec, LAYERS, LayerSpec};
pub use proj::{GridProjection, LonLatBox, TileCoord};
pub use source::{Family, SOURCES, SourceDef};
pub use upstream::UpstreamGate;
