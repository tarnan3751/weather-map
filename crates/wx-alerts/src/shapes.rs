//! Shapes for alerts that name zones instead of carrying a polygon.

use crate::Inner;
use crate::feed::Changes;
use crate::nws::{self, ZoneRef};
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};

type Candidate = (
    String,
    String,
    Vec<String>,
    Option<Value>,
    bool,
    Vec<String>,
);

/// Give every active, shapeless alert whose zones are all known the union
/// of its zones, and queue the zones that are missing. A zone that cannot be
/// had for now (the API has no boundary for it, or it keeps failing) does
/// not hold the alert back: it is drawn from its other zones, the missing
/// ones are recorded in `zones_missing`, and the shape is redone when they
/// arrive. Call with the write lock held.
pub(crate) async fn assign(inner: &Inner, ch: &mut Changes) -> anyhow::Result<()> {
    let candidates: Vec<Candidate> = sqlx::query_as(
        "SELECT id, event, ugc, raw->'properties'->'affectedZones', geom IS NOT NULL, zones_missing
         FROM alerts
         WHERE active AND cardinality(ugc) > 0
           AND (geom IS NULL OR (geom_source = 'zones' AND cardinality(zones_missing) > 0))",
    )
    .fetch_all(&inner.pool)
    .await?;
    if candidates.is_empty() {
        return Ok(());
    }
    let wanted: Vec<String> = candidates
        .iter()
        .flat_map(|c| c.2.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let present: HashSet<String> =
        sqlx::query_scalar::<_, String>("SELECT id FROM zones WHERE id = ANY($1)")
            .bind(&wanted)
            .fetch_all(&inner.pool)
            .await?
            .into_iter()
            .collect();

    for (id, event, ugc, affected, has_shape, was_missing) in candidates {
        let affected: Vec<String> = affected
            .as_ref()
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        let refs = nws::zone_refs(&affected, &ugc);
        let (have, missing): (Vec<&ZoneRef>, Vec<&ZoneRef>) =
            refs.iter().partition(|z| present.contains(&z.id));
        // Keep asking for what is missing; the queue's cooldowns keep that cheap.
        inner.zones.request(
            missing.iter().map(|z| (*z).clone()),
            event.ends_with("Warning"),
        );
        if have.is_empty() {
            continue;
        }
        let mut missing_ids: Vec<String> = missing.iter().map(|z| z.id.clone()).collect();
        missing_ids.sort();
        let given_up = missing.iter().all(|z| inner.zones.given_up(&z.id));
        if !reshape_due(has_shape, was_missing, &missing_ids, given_up) {
            continue;
        }
        let have: Vec<String> = have.iter().map(|z| z.id.clone()).collect();
        match union_into(&inner.pool, &id, &ugc, &have, &missing_ids).await {
            Ok(true) => ch.redrawn(id, has_shape, true),
            Ok(false) => {}
            Err(sqlx::Error::Database(e)) => tracing::warn!(alert = %id, "zone union failed: {e}"),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Whether an alert drawn from regions (zones, NUTS regions) is shaped now,
/// given the regions it is `missing` (sorted): a partial shape is redone
/// only once something it lacked has arrived; an alert without a shape
/// waits a little for the rest, until they are `given_up` on.
pub(crate) fn reshape_due(
    has_shape: bool,
    mut was_missing: Vec<String>,
    missing: &[String],
    given_up: bool,
) -> bool {
    if has_shape {
        was_missing.sort();
        was_missing != missing
    } else {
        given_up
    }
}

/// Updates of an alert usually name the same zones, so an existing complete
/// union for the same zone list is reused before computing a new one. A
/// partial union is never reused: it is redone when its zones arrive.
async fn union_into(
    pool: &sqlx::PgPool,
    id: &str,
    ugc: &[String],
    zones: &[String],
    missing: &[String],
) -> Result<bool, sqlx::Error> {
    let updated: Option<String> = sqlx::query_scalar(
        "UPDATE alerts a SET geom = s.g, geom_source = 'zones', zones_missing = $4
         FROM (SELECT coalesce(
                   CASE WHEN cardinality($4::text[]) = 0 THEN
                       (SELECT b.geom FROM alerts b
                        WHERE b.geom_source = 'zones' AND b.ugc = $2 AND b.id <> $1
                          AND b.geom IS NOT NULL AND cardinality(b.zones_missing) = 0
                        LIMIT 1)
                   END,
                   (SELECT ST_Multi(ST_CollectionExtract(ST_Union(z.geom), 3))
                    FROM zones z WHERE z.id = ANY($3))) AS g) s
         WHERE a.id = $1 AND a.active AND a.ugc = $2
           AND (a.geom IS NULL OR cardinality(a.zones_missing) > 0)
           AND s.g IS NOT NULL AND NOT ST_IsEmpty(s.g)
         RETURNING a.id",
    )
    .bind(id)
    .bind(ugc)
    .bind(zones)
    .bind(missing)
    .fetch_optional(pool)
    .await?;
    Ok(updated.is_some())
}
