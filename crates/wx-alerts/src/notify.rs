//! Warnings covering saved places become notification rows, then pushes.
//!
//! The `notifications` primary key `(event_key, place_id)` is the only
//! thing that decides "once per hazard per place": rows come from
//! `INSERT ... ON CONFLICT DO NOTHING`, and only rows actually inserted are
//! pushed. That holds however many times matching runs and from wherever
//! (poll cycle, zone shaper, world store, place creation, sweep). An update
//! of a hazard therefore reaches only places it newly covers, and a hazard
//! that grows serious enough to notify reaches the places it covers then.
//!
//! Which alerts notify (`notifies_sql!`): from the NWS, events whose name
//! ends in "Warning"; from the rest of the world, CAP severity Severe or
//! Extreme with an urgency other than Past. The rest are drawn but silent.

use crate::Inner;
use crate::nws::until_phrase;
use crate::push::{Delivery, PushOptions, Subscription, topic_for};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::Serialize;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct NewNotification {
    pub event_key: String,
    pub place_id: Uuid,
    pub alert_id: String,
}

/// Insert a notification for every (active warning, place with `notify`)
/// pair where the warning's shape covers the place, optionally limited to
/// some alerts or one place. Returns the rows that are new.
pub(crate) async fn match_places(
    inner: &Inner,
    alert_ids: Option<&[String]>,
    place_id: Option<Uuid>,
) -> Result<Vec<NewNotification>, sqlx::Error> {
    const MATCH_SQL: &str = concat!(
        "INSERT INTO notifications (event_key, place_id, alert_id)
         SELECT DISTINCT ON (a.event_key, p.id) a.event_key, p.id, a.id
         FROM alerts a
         JOIN places p ON p.notify AND ST_Covers(a.geom, p.geom)
         WHERE a.active AND a.geom IS NOT NULL AND ",
        notifies_sql!(),
        " AND coalesce(a.ends, a.expires, 'infinity') > now()
           AND ($1::text[] IS NULL OR a.id = ANY($1))
           AND ($2::uuid IS NULL OR p.id = $2)
         ORDER BY a.event_key, p.id, a.sent DESC NULLS LAST, a.id
         ON CONFLICT (event_key, place_id) DO NOTHING
         RETURNING event_key, place_id, alert_id"
    );
    sqlx::query_as(MATCH_SQL)
        .bind(alert_ids)
        .bind(place_id)
        .fetch_all(&inner.pool)
        .await
}

/// Deliver in the background; the poll loop and HTTP handlers never wait
/// on push services.
pub(crate) fn dispatch(inner: &Arc<Inner>, rows: Vec<NewNotification>) {
    let rows: Vec<NewNotification> = {
        let mut in_flight = crate::lock(&inner.in_flight);
        rows.into_iter()
            .filter(|n| in_flight.insert((n.event_key.clone(), n.place_id)))
            .collect()
    };
    if rows.is_empty() {
        return;
    }
    let inner = inner.clone();
    tokio::spawn(async move {
        futures::stream::iter(rows)
            .for_each_concurrent(8, |n| {
                let inner = inner.clone();
                async move {
                    if let Err(e) = deliver(&inner, &n).await {
                        // Left `pending`; the sweep tries again.
                        tracing::warn!(event_key = %n.event_key, place = %n.place_id, "notification not delivered: {e:#}");
                    }
                    crate::lock(&inner.in_flight).remove(&(n.event_key.clone(), n.place_id));
                }
            })
            .await;
    });
}

/// Rows left `pending` by a delivery that died (database trouble, restart).
pub(crate) async fn redeliver_orphans(inner: &Arc<Inner>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE notifications SET status = 'failed', error = 'not delivered within an hour'
         WHERE status = 'pending' AND created_at < now() - interval '1 hour'",
    )
    .execute(&inner.pool)
    .await?;
    let orphans: Vec<NewNotification> = sqlx::query_as(
        "SELECT event_key, place_id, alert_id FROM notifications
         WHERE status = 'pending' AND created_at < now() - interval '2 minutes'",
    )
    .fetch_all(&inner.pool)
    .await?;
    dispatch(inner, orphans);
    Ok(())
}

/// Deliver the warnings that found a place before its device could receive
/// pushes, now that it can (all devices with a subscription, or one).
/// `deliver` checks that each is still in effect; the row's clock restarts
/// so the retry window counts from now.
pub(crate) async fn requeue_waiting(
    inner: &Arc<Inner>,
    device: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let waiting: Vec<NewNotification> = sqlx::query_as(
        "UPDATE notifications n SET status = 'pending', error = NULL, created_at = now()
         FROM places p
         WHERE n.place_id = p.id AND n.status = 'no_subscription'
           AND ($1::uuid IS NULL OR p.device_id = $1)
           AND EXISTS (SELECT 1 FROM push_subscriptions s WHERE s.device_id = p.device_id)
         RETURNING n.event_key, n.place_id, n.alert_id",
    )
    .bind(device)
    .fetch_all(&inner.pool)
    .await?;
    dispatch(inner, waiting);
    Ok(())
}

#[derive(sqlx::FromRow)]
struct Context {
    place_name: String,
    lat: f64,
    lon: f64,
    device_id: Uuid,
    notify: bool,
    alert_id: String,
    origin: String,
    event: String,
    severity: String,
    headline: Option<String>,
    area_desc: Option<String>,
    ends_raw: Option<String>,
    ends: Option<DateTime<Utc>>,
}

/// What the service worker receives.
#[derive(Debug, Serialize)]
pub(crate) struct Payload<'a> {
    pub title: &'a str,
    pub body: String,
    pub tag: &'a str,
    pub url: String,
    pub alert_id: Option<&'a str>,
    pub place_id: Option<Uuid>,
    pub severity: Option<&'a str>,
}

async fn deliver(inner: &Inner, n: &NewNotification) -> anyhow::Result<()> {
    let pool = &inner.pool;
    // The newest message about the hazard that still covers the place, not
    // necessarily the one that created the row: a delivery retried minutes
    // later must follow the warning's continuations, and must not announce
    // a warning that has ended in the meantime.
    const CONTEXT_SQL: &str = concat!(
        "SELECT p.name AS place_name, ST_Y(p.geom) AS lat, ST_X(p.geom) AS lon, p.device_id, p.notify,
                a.id AS alert_id, a.origin, a.event, ",
        severity_sql!(),
        " AS severity, a.headline, a.area_desc,
                a.raw->'properties'->>'ends' AS ends_raw,
                coalesce(a.ends, a.expires) AS ends
         FROM places p
         JOIN alerts a ON a.event_key = $2 AND a.active AND a.geom IS NOT NULL AND ",
        notifies_sql!(),
        " AND coalesce(a.ends, a.expires, 'infinity') > now()
                      AND ST_Covers(a.geom, p.geom)
         WHERE p.id = $1
         ORDER BY a.sent DESC NULLS LAST, a.id
         LIMIT 1"
    );
    let ctx: Option<Context> = sqlx::query_as(CONTEXT_SQL)
        .bind(n.place_id)
        .bind(&n.event_key)
        .fetch_optional(pool)
        .await?;
    // (A deleted place takes its row with it, so recording is a no-op.)
    let Some(ctx) = ctx else {
        return record(
            pool,
            n,
            &n.alert_id,
            "failed",
            Some("warning no longer in effect at the place"),
        )
        .await;
    };
    let alert_id = ctx.alert_id.as_str();
    if !ctx.notify {
        return record(
            pool,
            n,
            alert_id,
            "failed",
            Some("notifications were turned off for the place"),
        )
        .await;
    }
    let subs = subscriptions(pool, ctx.device_id).await?;
    if subs.is_empty() {
        return record(pool, n, alert_id, "no_subscription", None).await;
    }
    if !inner.push.configured() {
        return record(
            pool,
            n,
            alert_id,
            "failed",
            Some("push is not configured (no VAPID keys)"),
        )
        .await;
    }

    let body = notification_body(&ctx);
    let payload = Payload {
        title: &ctx.event,
        body,
        tag: &n.event_key,
        url: format!(
            "/?alert={}&lat={}&lon={}",
            url::form_urlencoded::byte_serialize(alert_id.as_bytes()).collect::<String>(),
            coord(ctx.lat),
            coord(ctx.lon)
        ),
        alert_id: Some(alert_id),
        place_id: Some(n.place_id),
        severity: Some(&ctx.severity),
    };
    let payload = serde_json::to_vec(&payload)?;
    // Worth delivering until the warning ends, not after; an open-ended one
    // until its next statement is due.
    let ttl = ctx
        .ends
        .map_or(3600, |e| (e - Utc::now()).num_seconds())
        .clamp(60, 24 * 3600) as u32;
    let opts = PushOptions {
        ttl_secs: ttl,
        urgency: "high",
        topic: Some(topic_for(&n.event_key)),
    };
    let outcome = send_all(inner, &subs, &payload, &opts).await?;
    let errors = outcome.errors.join("; ");
    if outcome.sent > 0 {
        record(pool, n, alert_id, "sent", None).await
    } else if outcome.gone == subs.len() {
        // Every subscription had expired: the device has nowhere to receive
        // it, the same as never having subscribed. Subscribing again (or the
        // sweep, once it has) delivers it if the warning is still on.
        record(pool, n, alert_id, "no_subscription", Some(&errors)).await
    } else if outcome.transient {
        // Left pending: the sweep tries again every couple of minutes for
        // an hour, as long as the warning is still in effect.
        record(
            pool,
            n,
            alert_id,
            "pending",
            Some(&format!("will retry: {errors}")),
        )
        .await
    } else {
        record(pool, n, alert_id, "failed", Some(&errors)).await
    }
}

/// NWS: "Home: Tornado Warning until 6:45 PM CST" (only the real `ends`
/// counts: `expires` is when the next statement is due). World: the
/// alert's own headline in the language it was shown in, after the place:
/// a sentence of our own could not be in the sender's language.
fn notification_body(ctx: &Context) -> String {
    if ctx.origin == "nws" {
        return match until_phrase(ctx.headline.as_deref(), ctx.ends_raw.as_deref()) {
            Some(until) => format!("{}: {} {until}", ctx.place_name, ctx.event),
            None => format!("{}: {}", ctx.place_name, ctx.event),
        };
    }
    let text = ctx
        .headline
        .clone()
        .or_else(|| {
            ctx.area_desc
                .as_ref()
                .map(|a| format!("{} ({a})", ctx.event))
        })
        .unwrap_or_else(|| ctx.event.clone());
    let body = format!("{}: {text}", ctx.place_name);
    body.chars().take(300).collect()
}

pub(crate) struct SendOutcome {
    pub sent: usize,
    /// Subscriptions the push service said were gone (now deleted).
    pub gone: usize,
    pub errors: Vec<String>,
    /// Some failure might go through on a later try.
    pub transient: bool,
}

/// Push to each subscription and keep their bookkeeping: 404/410 deletes
/// the subscription, success resets `failure_count`, failure increments it
/// (so it counts consecutive failures).
pub(crate) async fn send_all(
    inner: &Inner,
    subs: &[Subscription],
    payload: &[u8],
    opts: &PushOptions,
) -> Result<SendOutcome, sqlx::Error> {
    let results =
        futures::future::join_all(subs.iter().map(|s| inner.push.send(s, payload, opts))).await;
    let mut out = SendOutcome {
        sent: 0,
        gone: 0,
        errors: Vec::new(),
        transient: false,
    };
    for (sub, result) in subs.iter().zip(results) {
        match result {
            Delivery::Sent => {
                out.sent += 1;
                sqlx::query("UPDATE push_subscriptions SET last_success_at = now(), failure_count = 0 WHERE id = $1")
                    .bind(sub.id)
                    .execute(&inner.pool)
                    .await?;
            }
            Delivery::Gone => {
                out.gone += 1;
                out.errors
                    .push("subscription expired (removed)".to_string());
                sqlx::query("DELETE FROM push_subscriptions WHERE id = $1")
                    .bind(sub.id)
                    .execute(&inner.pool)
                    .await?;
            }
            Delivery::Failed { reason, transient } => {
                out.errors.push(reason);
                out.transient |= transient;
                sqlx::query(
                    "UPDATE push_subscriptions SET failure_count = failure_count + 1 WHERE id = $1",
                )
                .bind(sub.id)
                .execute(&inner.pool)
                .await?;
            }
        }
    }
    Ok(out)
}

pub(crate) async fn subscriptions(
    pool: &PgPool,
    device_id: Uuid,
) -> Result<Vec<Subscription>, sqlx::Error> {
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT id, endpoint, p256dh, auth FROM push_subscriptions WHERE device_id = $1 ORDER BY created_at",
    )
    .bind(device_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, endpoint, p256dh, auth)| Subscription {
            id,
            endpoint,
            p256dh,
            auth,
        })
        .collect())
}

/// `alert_id`: the message actually delivered (or last considered).
async fn record(
    pool: &PgPool,
    n: &NewNotification,
    alert_id: &str,
    status: &str,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let error: Option<String> = error.map(|e| e.chars().take(500).collect());
    sqlx::query(
        "UPDATE notifications
         SET status = $3, sent_at = CASE WHEN $3 = 'sent' THEN now() END, error = $4, alert_id = $5
         WHERE event_key = $1 AND place_id = $2",
    )
    .bind(&n.event_key)
    .bind(n.place_id)
    .bind(status)
    .bind(error)
    .bind(alert_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// `35.22`, not `35.220000000000006`.
fn coord(v: f64) -> String {
    let s = format!("{v:.5}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_print_short() {
        assert_eq!(coord(35.22), "35.22");
        assert_eq!(coord(-97.44), "-97.44");
        assert_eq!(coord(35.0), "35");
        assert_eq!(coord(0.1 + 0.2), "0.3");
        assert_eq!(coord(-0.000001), "0");
        assert_eq!(coord(12.345678), "12.34568");
    }
}
