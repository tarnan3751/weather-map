//! Shapes for alerts that name NUTS 3 regions instead of drawing an area
//! (MeteoAlarm countries such as Bulgaria, Croatia, France and Romania).
//!
//! Boundaries come from Eurostat GISCO's public distribution
//! (`gisco-services.ec.europa.eu/distribution/v2/nuts`), one file per region
//! at 1:1 million, fetched the first time a region is named and kept in
//! `nuts_regions` (like NWS zones). Senders use different NUTS editions:
//! France still sends NUTS 2013 codes, which later editions renamed. A code
//! is looked up in the newest edition first and in older ones only when
//! that one does not have it.
//!
//! Licence (read 2026-11-06 on the GISCO statistical-units page): use is
//! granted for non-commercial purposes on condition that the source is
//! acknowledged with "© EuroGeographics for the administrative
//! boundaries" ([`super::NUTS_ATTRIBUTION`]). No key or account is needed.

use crate::Inner;
use crate::feed::Changes;
use crate::nws;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const GISCO_BASE: &str = "https://gisco-services.ec.europa.eu/distribution/v2/nuts";
/// Newest first. A code in an older edition only is read from that one.
const EDITIONS: [u16; 5] = [2024, 2021, 2016, 2013, 2010];
/// The geocode name senders use for NUTS 3 codes.
const PREFIX: &str = "NUTS3:";
const MAX_REGION_BYTES: usize = 16 << 20;
/// A code no edition has is not asked for again for this long.
const UNKNOWN_FOR: Duration = Duration::from_secs(24 * 3600);
/// After this many failed rounds an alert is drawn from its other regions.
const GIVE_UP_AFTER: u32 = 2;
/// Regions fetched per upkeep pass; the rest wait for the next pass.
const REGIONS_PER_PASS: usize = 200;

pub(crate) struct NutsState {
    interval: Duration,
    misses: Mutex<HashMap<String, Miss>>,
}

struct Miss {
    until: Instant,
    failures: u32,
    unknown: bool,
}

impl NutsState {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            misses: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Miss>> {
        crate::lock(&self.misses)
    }

    fn cooling(&self, code: &str) -> bool {
        self.lock()
            .get(code)
            .is_some_and(|m| m.until > Instant::now())
    }

    /// Not worth waiting for: no edition has it, or it failed a few times.
    fn given_up(&self, code: &str) -> bool {
        self.lock()
            .get(code)
            .is_some_and(|m| m.unknown || m.failures >= GIVE_UP_AFTER)
    }

    fn failed(&self, code: &str, unknown: bool) {
        let mut misses = self.lock();
        let failures = misses.get(code).map_or(0, |m| m.failures) + 1;
        let wait = if unknown {
            UNKNOWN_FOR
        } else {
            Duration::from_secs(60 * 2u64.pow(failures.min(6)))
        };
        misses.insert(
            code.to_string(),
            Miss {
                until: Instant::now() + wait,
                failures,
                unknown,
            },
        );
    }
}

/// `BG332` from `NUTS3:BG332`: two letters and three letters or digits.
pub(crate) fn code(region: &str) -> Option<&str> {
    let c = region.strip_prefix(PREFIX)?;
    let b = c.as_bytes();
    (b.len() == 5
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2..]
            .iter()
            .all(|x| x.is_ascii_uppercase() || x.is_ascii_digit()))
    .then_some(c)
}

type Candidate = (String, Vec<String>, bool, Vec<String>);

/// Fetch the regions active alerts are waiting for, then give each alert
/// whose regions are known (or given up on) the union of them.
pub(crate) async fn shape_pending(inner: &Arc<Inner>) -> anyhow::Result<Changes> {
    let world = inner.world();
    let Some(base) = world
        .config
        .nuts_base_url
        .as_deref()
        .map(|b| b.trim_end_matches('/').to_string())
    else {
        return Ok(Changes::default());
    };
    let candidates: Vec<Candidate> = sqlx::query_as(
        "SELECT id, region_codes, geom IS NOT NULL, zones_missing FROM alerts
         WHERE origin = 'cap' AND active
           AND (geom IS NULL OR (geom_source = 'nuts' AND cardinality(zones_missing) > 0))
           AND EXISTS (SELECT 1 FROM unnest(region_codes) c WHERE c LIKE 'NUTS3:%')",
    )
    .fetch_all(&inner.pool)
    .await?;
    if candidates.is_empty() {
        return Ok(Changes::default());
    }
    let wanted: BTreeSet<String> = candidates
        .iter()
        .flat_map(|c| c.1.iter().filter_map(|r| code(r)).map(str::to_string))
        .collect();
    let wanted: Vec<String> = wanted.into_iter().collect();
    let mut present: HashSet<String> =
        sqlx::query_scalar::<_, String>("SELECT code FROM nuts_regions WHERE code = ANY($1)")
            .bind(&wanted)
            .fetch_all(&inner.pool)
            .await?
            .into_iter()
            .collect();

    let to_fetch: Vec<&String> = wanted
        .iter()
        .filter(|c| !present.contains(*c) && !world.nuts.cooling(c))
        .take(REGIONS_PER_PASS)
        .collect();
    for c in to_fetch {
        match fetch_region(inner, &base, c).await {
            Ok(true) => {
                present.insert(c.clone());
            }
            Ok(false) => {
                tracing::info!(code = %c, "no NUTS edition has this region");
                world.nuts.failed(c, true);
            }
            Err(e) => {
                tracing::warn!(code = %c, "NUTS region fetch failed: {e:#}");
                world.nuts.failed(c, false);
            }
        }
    }

    let mut ch = Changes::default();
    for (id, regions, has_shape, was_missing) in candidates {
        let codes: Vec<String> = regions
            .iter()
            .filter_map(|r| code(r))
            .map(str::to_string)
            .collect();
        let (have, missing): (Vec<String>, Vec<String>) =
            codes.into_iter().partition(|c| present.contains(c));
        if have.is_empty() {
            continue;
        }
        let mut missing_sorted = missing.clone();
        missing_sorted.sort();
        let given_up = missing.iter().all(|c| world.nuts.given_up(c));
        if !crate::shapes::reshape_due(has_shape, was_missing, &missing_sorted, given_up) {
            continue;
        }
        let _writes = inner.writes.lock().await;
        let updated: Result<Option<String>, sqlx::Error> = sqlx::query_scalar(
            "UPDATE alerts a SET geom = s.g, geom_source = 'nuts', zones_missing = $3
             FROM (SELECT ST_ForcePolygonCCW(ST_Multi(ST_CollectionExtract(ST_MakeValid(ST_Union(geom)), 3))) AS g
                   FROM nuts_regions WHERE code = ANY($2)) s
             WHERE a.id = $1 AND a.active AND s.g IS NOT NULL AND NOT ST_IsEmpty(s.g)
               AND (a.geom IS NULL OR a.geom_source = 'nuts')
             RETURNING a.id",
        )
        .bind(&id)
        .bind(&have)
        .bind(&missing_sorted)
        .fetch_optional(&inner.pool)
        .await;
        let updated = match updated {
            Ok(u) => u,
            // GEOS refused this union: the alert stays listed without a shape.
            Err(sqlx::Error::Database(e)) => {
                tracing::warn!(alert = %id, "NUTS union failed: {e}");
                continue;
            }
            Err(e) => {
                world.add_pending(ch);
                return Err(e.into());
            }
        };
        if updated.is_some() {
            ch.redrawn(id, has_shape, true);
        }
    }
    Ok(ch)
}

/// `Ok(false)` when no edition has the code.
async fn fetch_region(inner: &Inner, base: &str, code: &str) -> anyhow::Result<bool> {
    let world = inner.world();
    for edition in EDITIONS {
        tokio::time::sleep(world.nuts.interval).await;
        let url = format!("{base}/distribution/{code}-region-01m-4326-{edition}.geojson");
        let body = match world
            .http
            .get(
                &url,
                &Default::default(),
                world.config.request_timeout,
                MAX_REGION_BYTES,
            )
            .await
        {
            Ok(super::http::Fetched::Body { bytes, .. }) => bytes,
            Ok(super::http::Fetched::NotModified) => {
                anyhow::bail!("304 to an unconditional request")
            }
            Err(super::http::FetchError::Status { status: 404, .. }) => continue,
            Err(e) => anyhow::bail!("{url}: {e}"),
        };
        let (name, geometry) = parse_region(&body, code)?;
        let stored: Option<String> = sqlx::query_scalar(
            "INSERT INTO nuts_regions (code, edition, name, geom, fetched_at)
             SELECT $1, $2, $3, g, now()
             FROM (SELECT ST_ForcePolygonCCW(ST_Multi(ST_CollectionExtract(
                       ST_MakeValid(ST_SetSRID(ST_GeomFromGeoJSON($4), 4326)), 3))) AS g) s
             WHERE NOT ST_IsEmpty(g)
             ON CONFLICT (code) DO UPDATE SET
                 edition = EXCLUDED.edition, name = EXCLUDED.name, geom = EXCLUDED.geom, fetched_at = now()
             RETURNING code",
        )
        .bind(code)
        .bind(edition as i16)
        .bind(name)
        .bind(geometry)
        .fetch_optional(&inner.pool)
        .await?;
        anyhow::ensure!(
            stored.is_some(),
            "{code} ({edition}) has no usable boundary"
        );
        return Ok(true);
    }
    Ok(false)
}

/// The name and boundary (clean GeoJSON) of `code` from a GISCO region file.
fn parse_region(body: &[u8], code: &str) -> anyhow::Result<(Option<String>, String)> {
    let doc: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("not JSON: {e}"))?;
    let feature = doc
        .get("features")
        .and_then(|f| f.as_array())
        .and_then(|f| {
            f.iter()
                .find(|f| f["properties"]["NUTS_ID"].as_str() == Some(code))
        })
        .ok_or_else(|| anyhow::anyhow!("file has no feature for {code}"))?;
    let geometry = nws::clean_polygons(&feature["geometry"])
        .ok_or_else(|| anyhow::anyhow!("no polygon for {code}"))?;
    let name = feature["properties"]["NAME_LATN"]
        .as_str()
        .or_else(|| feature["properties"]["NUTS_NAME"].as_str())
        .and_then(|n| super::text::line(n, 200));
    Ok((name, geometry))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(code("NUTS3:BG332"), Some("BG332"));
        assert_eq!(code("NUTS3:FRK26"), Some("FRK26"));
        assert_eq!(code("EMMA_ID:HR023"), None, "EMMA ids are not NUTS codes");
        assert_eq!(code("NUTS3:BG33"), None);
        assert_eq!(code("NUTS3:../x"), None);
        assert_eq!(code("NUTS3:bg332"), None);
    }

    #[test]
    fn region_files() {
        let body = serde_json::json!({
            "type": "FeatureCollection",
            "features": [{ "type": "Feature",
                "properties": { "NUTS_ID": "BG332", "NAME_LATN": "Dobrich" },
                "geometry": { "type": "MultiPolygon", "coordinates": [[[[27.0, 43.5], [28.5, 43.5], [28.5, 44.0], [27.0, 43.5]]]] } }]
        })
        .to_string();
        let (name, g) = parse_region(body.as_bytes(), "BG332").unwrap();
        assert_eq!(name.as_deref(), Some("Dobrich"));
        assert!(g.contains("MultiPolygon"));
        assert!(
            parse_region(body.as_bytes(), "BG331").is_err(),
            "a file for another region is refused"
        );
        assert!(parse_region(b"<html>", "BG332").is_err());
    }
}
