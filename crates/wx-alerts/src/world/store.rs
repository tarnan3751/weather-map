//! Storing CAP messages and threading them into hazards.
//!
//! A hazard is a thread of messages linked by `<references>`: the union of
//! every `(sender, identifier)` pair that refers to another. The thread's
//! key goes in `alerts.event_key`, the column the NWS path keys its
//! notifications on, prefixed `cap:` so it can never equal a VTEC-derived
//! key (`KOUN.TO.W.0042.2026`) or an NWS message id (`urn:oid:...`). Of a
//! thread's messages, only the newest (by `sent`) is in effect, and only if
//! it is not a Cancel, has not expired, and (for feeds that list current
//! alerts only) is still listed. Messages may arrive in any order: a
//! message joins the thread of any stored message it references or that
//! references it, and two threads a message links are merged.
//!
//! Senders are not always consistent, so within one source a reference
//! also matches on the identifier alone (MET Norway sends as
//! `meteorologi@met.no` but refers to `noreply@met.no,<id>`), and two
//! messages that reference the same message belong together even when that
//! message was never seen (the Bureau of Meteorology's corrections).
//!
//! Many senders also re-issue a hazard as a new message with no references,
//! or with references to nothing stored. Such a message joins the active
//! thread from the same sender about the same event and area whose time
//! window overlaps its own (the fallback key, `area_key`).

use super::cap::{Info, Message, MsgType};
use super::geo::{self, SHAPE_SQL};
use super::http::Validators;
use super::register::Source;
use super::{DocState, text};
use crate::Inner;
use crate::feed::Changes;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, LazyLock};

/// How long an alert whose sender gave no expiry is taken to be in effect
/// (as `until_sql!` has it).
const DEFAULT_LIFETIME_HOURS: i64 = 24;
const MAX_REGION_CODES: usize = 500;

/// What became of a parsed message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stored {
    New,
    /// Already stored (from this source or another).
    Known,
    Ignored(&'static str),
}

impl Stored {
    pub fn label(self) -> &'static str {
        match self {
            Self::New => "stored",
            Self::Known => "known",
            Self::Ignored(why) => why,
        }
    }
}

/// The id a world alert is served under: short, URL-safe and stable for
/// a message, whatever characters its sender and identifier contain.
pub(crate) fn alert_id(message_key: &str) -> String {
    format!(
        "cap:{}",
        &geo::hex(&Sha256::digest(message_key.as_bytes()))[..20]
    )
}

fn new_thread_key(message_key: &str) -> String {
    let digest = Sha256::digest(format!("thread\n{message_key}").as_bytes());
    format!("cap:{}", &geo::hex(&digest)[..24])
}

/// When a message stops being in effect.
fn until(m: &Message) -> DateTime<Utc> {
    m.expires()
        .unwrap_or_else(|| m.start() + chrono::Duration::hours(DEFAULT_LIFETIME_HOURS))
}

/// Why a message is not stored at all, if it is not.
fn ignored(m: &Message) -> Option<&'static str> {
    let cancel = m.msg_type == MsgType::Cancel;
    if !m.status.eq_ignore_ascii_case("actual") {
        Some("not Actual")
    } else if !m.is_public() {
        Some("not public")
    } else if m.msg_type == MsgType::Other {
        Some("Ack or Error")
    } else if !cancel && m.infos.is_empty() {
        Some("no info")
    } else if !cancel && until(m) <= Utc::now() {
        // A Cancel is kept however old: it may end a message that arrives later.
        Some("expired")
    } else {
        None
    }
}

/// Store messages from one source, oldest first, and, for feeds that list
/// current alerts only, end those no longer listed. The write lock is taken
/// per message, so a source with a hundred new documents does not hold up
/// the NWS cycle for all of them. Returns the changes and what became of
/// each message.
pub(crate) async fn apply(
    inner: &Arc<Inner>,
    src: &Source,
    messages: Vec<(String, Message)>,
    listed: Option<&[String]>,
) -> anyhow::Result<(Changes, Vec<(String, Stored)>)> {
    let mut results = Vec::with_capacity(messages.len());
    let mut todo = Vec::new();
    for (link, m) in messages {
        match ignored(&m) {
            Some(why) => results.push((link, Stored::Ignored(why))),
            None => todo.push((alert_id(&m.key()), link, m)),
        }
    }
    let mut known: HashSet<String> = HashSet::new();
    if !todo.is_empty() {
        let ids: Vec<&str> = todo.iter().map(|(id, ..)| id.as_str()).collect();
        known = sqlx::query_scalar("SELECT id FROM alerts WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_all(&inner.pool)
            .await?
            .into_iter()
            .collect();
    }
    // Referenced messages were sent before the ones referencing them.
    todo.sort_by_key(|(_, _, m)| m.sent);
    let mut ch = Changes::default();
    for (id, link, m) in todo {
        let stored = if known.contains(&id) {
            Ok(Stored::Known)
        } else {
            let _writes = inner.writes.lock().await;
            store(&inner.pool, src, &link, &m, &id, &mut ch).await
        };
        match stored {
            Ok(r) => {
                known.insert(id);
                results.push((link, r));
            }
            Err(e) => {
                // What was committed before the failure must still reach
                // maps and phones.
                inner.world().add_pending(ch);
                return Err(e);
            }
        }
    }
    if let Some(listed) = listed {
        let _writes = inner.writes.lock().await;
        if let Err(e) = end_unlisted(&inner.pool, &src.id, listed, &mut ch).await {
            inner.world().add_pending(ch);
            return Err(e.into());
        }
    }
    Ok((ch, results))
}

/// Store a message that is not stored yet in the thread it continues (or a
/// new one), and make the thread's newest message the one in effect. The
/// message is stored first, in a thread of its own: if GEOS refuses its
/// shape, nothing else has been done yet to undo.
async fn store(
    pool: &PgPool,
    src: &Source,
    link: &str,
    m: &Message,
    id: &str,
    ch: &mut Changes,
) -> anyhow::Result<Stored> {
    let key = m.key();
    // Some senders list the message itself among its references.
    let refs: Vec<_> = m
        .references
        .iter()
        .filter(|r| r.key() != key && r.identifier != m.identifier)
        .collect();
    let ref_keys: Vec<String> = refs.iter().map(|r| r.key()).collect();
    let ref_ids: Vec<&str> = refs
        .iter()
        .map(|r| r.identifier.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let area_key = area_key(m);
    let own = new_thread_key(&key);
    let row = row(
        src,
        link,
        m,
        id,
        &own,
        (&ref_keys, &ref_ids),
        area_key.as_deref(),
    );
    let shape = if m.msg_type == MsgType::Cancel {
        None
    } else {
        geo::shape_geojson(&m.areas())
    };

    let mut tx = pool.begin().await?;
    let inserted = match insert(&mut tx, &row, shape.as_ref()).await {
        // The alert is still worth listing without its shape.
        Err(sqlx::Error::Database(e)) if shape.is_some() => {
            tracing::warn!(source = %src.id, alert = %id, "shape refused, stored without one: {e}");
            tx.rollback().await?;
            tx = pool.begin().await?;
            insert(&mut tx, &row, None).await?
        }
        inserted => inserted?,
    };
    if !inserted {
        // Stored meanwhile from another source.
        tx.rollback().await?;
        return Ok(Stored::Known);
    }
    let threads: Vec<String> = sqlx::query_scalar(THREADS_SQL)
        .bind(&ref_keys)
        .bind(&key)
        .bind(&src.id)
        .bind(&ref_ids)
        .bind(&m.identifier)
        .bind(&area_key)
        .bind(&m.sender)
        .bind(m.start())
        .bind(until(m))
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
    let thread = match threads.split_first() {
        Some((first, rest)) => {
            join(&mut tx, first, rest, Some(id)).await?;
            first.as_str()
        }
        None => own.as_str(),
    };
    recompute(&mut tx, &[thread], ch).await?;
    tx.commit().await?;
    Ok(Stored::New)
}

/// `$1` the message's references, `$2` its key, `$3` its source, `$4` the
/// identifiers it references, `$5` its identifier, `$6` its area key, `$7`
/// its sender, `$8` and `$9` when it is in effect, `$10` its id. The
/// threads of other messages that the message continues, oldest first:
/// those of messages it references or that reference it (exactly, or within
/// the source by identifier alone), and of messages of the source that
/// reference what it references. With none of those stored, the same hazard
/// re-issued: the thread of an alert in effect from the same sender for the
/// same event and area, at an overlapping time.
const THREADS_SQL: &str = concat!(
    "WITH related AS (
         SELECT event_key, min(first_seen) AS first FROM alerts
         WHERE origin = 'cap' AND id <> $10
           AND (cap_key = ANY($1) OR cap_references @> ARRAY[$2]
                OR (source = $3 AND (cap_identifier = ANY($4)
                                     OR cap_reference_ids @> ARRAY[$5]
                                     OR cap_reference_ids && $4)))
         GROUP BY event_key),
     reissued AS (
         SELECT event_key, first_seen AS first FROM alerts
         WHERE NOT EXISTS (SELECT 1 FROM related)
           AND origin = 'cap' AND id <> $10 AND active AND area_key = $6 AND cap_sender = $7
           AND coalesce(onset, effective, sent) < $9 AND ",
    until_sql!(),
    " > $8
         ORDER BY first_seen LIMIT 1)
     SELECT event_key FROM (SELECT * FROM related UNION ALL SELECT * FROM reissued) t
     ORDER BY first, event_key"
);

/// The message as a row of `alerts`, keyed by column, without its shape.
fn row(
    src: &Source,
    link: &str,
    m: &Message,
    id: &str,
    thread: &str,
    (references, reference_ids): (&[String], &[&str]),
    area_key: Option<&str>,
) -> Value {
    let none = Info::default();
    let p = m.primary().unwrap_or(&none);
    let worst = m.most_severe().unwrap_or(&none);
    let shown = m.shown();
    let areas = m.areas();
    let mut names: Vec<&str> = Vec::new();
    for n in areas.iter().filter_map(|a| a.desc.as_deref()) {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    let region_codes: Vec<String> = areas
        .iter()
        .flat_map(|a| a.geocodes.iter())
        .map(|(n, v)| format!("{}:{v}", n.to_ascii_uppercase()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_REGION_CODES)
        .collect();
    let infos: Vec<Value> = m
        .infos
        .iter()
        .map(|i| {
            let descs: Vec<&str> = i.areas.iter().filter_map(|a| a.desc.as_deref()).collect();
            json!({
                "language": language_or_default(&i.language),
                "event": i.event,
                "headline": i.headline,
                "description": i.description,
                "instruction": i.instruction,
                "area_desc": text::line(&descs.join("; "), 2000),
                "sender_name": i.sender_name,
                "web": i.web,
                "category": category(&i.categories),
                "severity": i.severity,
                "urgency": i.urgency,
                "certainty": i.certainty,
                "onset": i.onset,
                "expires": i.expires,
                "in_effect": !i.over,
            })
        })
        .collect();
    let event = p
        .event
        .as_ref()
        .or(p.headline.as_ref())
        .map_or(m.msg_type.as_str(), String::as_str);
    json!({
        "id": id,
        "event_key": thread,
        "event": event,
        "severity": worst.severity,
        "urgency": worst.urgency,
        "certainty": worst.certainty,
        "status": "Actual",
        "message_type": m.msg_type.as_str(),
        "headline": p.headline,
        "description": p.description,
        "instruction": p.instruction,
        "area_desc": text::line(&names.join("; "), 2000),
        "sender_name": p.sender_name,
        "sent": m.sent,
        "effective": shown.iter().filter_map(|i| i.effective).min().or(p.effective),
        "onset": shown.iter().filter_map(|i| i.onset).min().or(p.onset),
        "expires": m.expires(),
        "origin": "cap",
        "source": src.id,
        "country": src.country,
        "language": language_or_default(&p.language),
        "category": category(&p.categories),
        "infos": infos,
        "cap_sender": m.sender,
        "cap_identifier": m.identifier,
        "cap_key": m.key(),
        "cap_references": references,
        "cap_reference_ids": reference_ids,
        "in_effect": m.in_effect(),
        "area_key": area_key,
        "region_codes": region_codes,
        "cap_url": link,
        "anchor": src.anchor.map(|(lat, lon)| format!("SRID=4326;POINT({lon} {lat})")),
    })
}

/// The first of the info block's categories that CAP defines, lower case,
/// else `other`.
fn category(categories: &[String]) -> String {
    categories
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .find(|c| crate::cache::CATEGORIES.contains(&c.as_str()))
        .unwrap_or_else(|| "other".to_string())
}

fn language_or_default(l: &str) -> String {
    if l.trim().is_empty() {
        "en-US".to_string()
    } else {
        l.to_string()
    }
}

/// The fallback thread key: sender, event and where.
fn area_key(m: &Message) -> Option<String> {
    let sig = geo::area_signature(&m.areas())?;
    let event = m
        .primary()
        .and_then(|p| p.event.as_deref())
        .unwrap_or("")
        .to_lowercase();
    let digest = Sha256::digest(format!("{}\n{}\n{}", m.sender, event, sig).as_bytes());
    Some(geo::hex(&digest[..16]))
}

/// The columns [`row`] fills.
const ROW_COLUMNS: &str =
    "id, event_key, event, severity, urgency, certainty, status, message_type,
    headline, description, instruction, area_desc, sender_name, sent, effective, onset, expires,
    origin, source, country, language, category, infos, cap_sender, cap_identifier, cap_key,
    cap_references, cap_reference_ids, in_effect, area_key, region_codes, cap_url, anchor";

/// `$1` and `$2` the shape (see [`SHAPE_SQL`]), `$3` the row. A new
/// message is stored out of effect; [`recompute`] decides.
static INSERT_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "INSERT INTO alerts ({ROW_COLUMNS}, geom, geom_source, active, ended_at)
         SELECT {ROW_COLUMNS}, s.g, CASE WHEN s.g IS NULL THEN 'none' ELSE 'cap' END, false, now()
         FROM jsonb_populate_record(NULL::alerts, $3) CROSS JOIN (SELECT ({SHAPE_SQL}) AS g) s
         ON CONFLICT (id) DO NOTHING"
    )
});

/// `false` when the message is stored already.
async fn insert(
    tx: &mut Transaction<'_, Postgres>,
    row: &Value,
    shape: Option<&(String, Option<String>)>,
) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(INSERT_SQL.as_str())
        .bind(shape.map(|s| s.0.as_str()))
        .bind(shape.and_then(|s| s.1.as_deref()))
        .bind(row)
        .execute(&mut **tx)
        .await?;
    Ok(done.rows_affected() == 1)
}

/// Put the threads `others`, and the message `id`, under the thread
/// `into`. A place notified under several threads keeps one row (the one
/// under `into`, else the one under the first of `others`), so the joined
/// hazard still notifies it once.
async fn join(
    tx: &mut Transaction<'_, Postgres>,
    into: &str,
    others: &[impl AsRef<str>],
    id: Option<&str>,
) -> Result<(), sqlx::Error> {
    if others.is_empty() && id.is_none() {
        return Ok(());
    }
    let keys: Vec<&str> = std::iter::once(into)
        .chain(others.iter().map(AsRef::as_ref))
        .collect();
    sqlx::query(
        "WITH ranked AS (
             SELECT event_key, place_id,
                    row_number() OVER (PARTITION BY place_id ORDER BY array_position($1, event_key)) AS n
             FROM notifications WHERE event_key = ANY($1)),
         dropped AS (
             DELETE FROM notifications d USING ranked r
             WHERE d.event_key = r.event_key AND d.place_id = r.place_id AND r.n > 1),
         moved AS (
             UPDATE notifications u SET event_key = $1[1] FROM ranked r
             WHERE u.event_key = r.event_key AND u.place_id = r.place_id AND r.n = 1 AND r.event_key <> $1[1])
         UPDATE alerts SET event_key = $1[1]
         WHERE origin = 'cap' AND (event_key = ANY($1[2:]) OR id = $2)",
    )
    .bind(&keys)
    .bind(id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Make each thread's newest message the one in effect (if it may be) and
/// every other message not, and record what that changes: on the map for
/// alerts with a shape, in the lists for alerts without one.
async fn recompute(
    tx: &mut Transaction<'_, Postgres>,
    threads: &[&str],
    ch: &mut Changes,
) -> Result<(), sqlx::Error> {
    const RECOMPUTE_SQL: &str = concat!(
        "WITH want AS (
             SELECT id,
                    row_number() OVER (PARTITION BY event_key ORDER BY sent DESC, first_seen DESC, id DESC) = 1
                    AND message_type <> 'Cancel' AND in_effect AND NOT unlisted AND ",
        until_sql!(),
        " > now() AS on
             FROM alerts WHERE origin = 'cap' AND event_key = ANY($1))
         UPDATE alerts a
         SET active = w.on, ended_at = CASE WHEN w.on THEN NULL ELSE coalesce(a.ended_at, now()) END
         FROM want w
         WHERE a.id = w.id AND a.active IS DISTINCT FROM w.on
         RETURNING a.id, a.active, a.geom IS NOT NULL"
    );
    let changed: Vec<(String, bool, bool)> = sqlx::query_as(RECOMPUTE_SQL)
        .bind(threads)
        .fetch_all(&mut **tx)
        .await?;
    for (id, active, drawn) in changed {
        match (active, drawn) {
            (true, true) => {
                ch.added.insert(id.clone());
                ch.to_match.insert(id);
            }
            (false, true) => {
                ch.removed.insert(id);
            }
            (_, false) => {
                ch.listed.insert(id);
            }
        }
    }
    Ok(())
}

/// For feeds that list only what is in effect: an alert whose document is
/// no longer listed has ended, and one listed again is back.
async fn end_unlisted(
    pool: &PgPool,
    source: &str,
    listed: &[String],
    ch: &mut Changes,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let threads: BTreeSet<String> = sqlx::query_scalar(
        "UPDATE alerts SET unlisted = NOT (cap_url = ANY($2))
         WHERE origin = 'cap' AND source = $1 AND (active OR unlisted)
           AND unlisted IS DISTINCT FROM NOT (cap_url = ANY($2))
         RETURNING event_key",
    )
    .bind(source)
    .bind(listed)
    .fetch_all(&mut *tx)
    .await?
    .into_iter()
    .collect();
    if !threads.is_empty() {
        let threads: Vec<&str> = threads.iter().map(String::as_str).collect();
        recompute(&mut tx, &threads, ch).await?;
    }
    tx.commit().await
}

// ---------------------------------------------------------------------------
// Bookkeeping: the register and the documents fetched.

pub(crate) struct StoredRegister {
    pub body: Vec<u8>,
    pub validators: Validators,
    pub fetched_at: DateTime<Utc>,
}

type RegisterRow = (Vec<u8>, Option<String>, Option<String>, DateTime<Utc>);

pub(crate) async fn load_register(pool: &PgPool) -> Result<Option<StoredRegister>, sqlx::Error> {
    let row: Option<RegisterRow> =
        sqlx::query_as("SELECT body, etag, last_modified, fetched_at FROM alert_register")
            .fetch_optional(pool)
            .await?;
    Ok(
        row.map(|(body, etag, last_modified, fetched_at)| StoredRegister {
            body,
            validators: Validators {
                etag,
                last_modified,
            },
            fetched_at,
        }),
    )
}

pub(crate) async fn save_register(
    pool: &PgPool,
    body: &[u8],
    v: &Validators,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO alert_register (singleton, body, etag, last_modified, fetched_at)
         VALUES (true, $1, $2, $3, now())
         ON CONFLICT (singleton) DO UPDATE SET
             body = EXCLUDED.body, etag = EXCLUDED.etag,
             last_modified = EXCLUDED.last_modified, fetched_at = now()",
    )
    .bind(body)
    .bind(&v.etag)
    .bind(&v.last_modified)
    .execute(pool)
    .await?;
    Ok(())
}

/// What was fetched before. A retry is never further off than `max_wait`,
/// whatever wait was stored with it.
pub(crate) async fn load_documents(
    pool: &PgPool,
    max_wait: chrono::Duration,
) -> Result<HashMap<String, DocState>, sqlx::Error> {
    let rows: Vec<(String, bool, i32, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT url, done, attempts, retry_at FROM cap_documents")
            .fetch_all(pool)
            .await?;
    let latest = Utc::now() + max_wait;
    Ok(rows
        .into_iter()
        .map(|(url, done, attempts, retry_at)| {
            (
                url,
                DocState {
                    done,
                    attempts: attempts.max(0) as u32,
                    retry_at: retry_at.map(|t| t.min(latest)),
                },
            )
        })
        .collect())
}

pub(crate) async fn save_documents(
    pool: &PgPool,
    source: &str,
    rows: &[(String, DocState, String)],
) -> Result<(), sqlx::Error> {
    if rows.is_empty() {
        return Ok(());
    }
    let urls: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    let done: Vec<bool> = rows.iter().map(|r| r.1.done).collect();
    let attempts: Vec<i32> = rows.iter().map(|r| r.1.attempts as i32).collect();
    let retry: Vec<Option<DateTime<Utc>>> = rows.iter().map(|r| r.1.retry_at).collect();
    let outcome: Vec<String> = rows
        .iter()
        .map(|r| r.2.chars().take(300).collect())
        .collect();
    sqlx::query(
        "INSERT INTO cap_documents (url, source, done, attempts, retry_at, outcome, fetched_at)
         SELECT u, $1, d, a, r, o, now()
         FROM unnest($2::text[], $3::bool[], $4::int[], $5::timestamptz[], $6::text[]) AS t(u, d, a, r, o)
         ON CONFLICT (url) DO UPDATE SET
             done = EXCLUDED.done, attempts = EXCLUDED.attempts, retry_at = EXCLUDED.retry_at,
             outcome = EXCLUDED.outcome, fetched_at = now()",
    )
    .bind(source)
    .bind(&urls)
    .bind(&done)
    .bind(&attempts)
    .bind(&retry)
    .bind(&outcome)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ThreadRow {
    event_key: String,
    source: String,
    cap_sender: Option<String>,
    cap_identifier: Option<String>,
    cap_key: Option<String>,
    cap_references: Vec<String>,
    cap_reference_ids: Vec<String>,
    area_key: Option<String>,
    active: bool,
    in_effect: bool,
    first_seen: DateTime<Utc>,
    start: DateTime<Utc>,
    until: DateTime<Utc>,
}

/// Re-thread every stored world message by the rules [`apply`] follows,
/// merging threads that should be one, and bring the messages in effect up
/// to date, so that no hazard has two copies in effect: each is put back to
/// its newest message. Run once at start.
pub(crate) async fn rethread(inner: &Arc<Inner>) -> anyhow::Result<Changes> {
    let rows: Vec<ThreadRow> = sqlx::query_as(concat!(
        "SELECT event_key, source, cap_sender, cap_identifier, cap_key, cap_references, cap_reference_ids,
                area_key, active, in_effect, first_seen, coalesce(onset, effective, sent) AS start, ",
        until_sql!(),
        " AS until FROM alerts WHERE origin = 'cap'"
    ))
    .fetch_all(&inner.pool)
    .await?;
    let mut parent: Vec<usize> = (0..rows.len()).collect();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let (ra, rb) = (find(parent, a), find(parent, b));
        if ra != rb {
            parent[ra.max(rb)] = ra.min(rb);
        }
    }
    let mut by_key: HashMap<&str, usize> = HashMap::new();
    let mut by_ident: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if let Some(k) = &r.cap_key {
            by_key.insert(k, i);
        }
        if let Some(id) = &r.cap_identifier {
            by_ident.entry((&r.source, id)).or_default().push(i);
        }
    }
    let mut related = vec![false; rows.len()];
    let mut first_to_reference: HashMap<(&str, &str), usize> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        for k in &r.cap_references {
            if let Some(&j) = by_key.get(k.as_str())
                && j != i
            {
                union(&mut parent, i, j);
                related[i] = true;
            }
        }
        for id in &r.cap_reference_ids {
            for &j in by_ident
                .get(&(r.source.as_str(), id.as_str()))
                .into_iter()
                .flatten()
            {
                if j != i {
                    union(&mut parent, i, j);
                    related[i] = true;
                }
            }
            match first_to_reference.get(&(r.source.as_str(), id.as_str())) {
                Some(&j) => {
                    union(&mut parent, i, j);
                    related[i] = true;
                }
                None => {
                    first_to_reference.insert((&r.source, id), i);
                }
            }
        }
    }
    // The fallback: an active message with no stored relative joins an
    // earlier active one from its sender, for its event and area, whose
    // time overlaps.
    let mut groups: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        if let (true, Some(s), Some(a)) = (r.active, &r.cap_sender, &r.area_key) {
            groups.entry((s, a)).or_default().push(i);
        }
    }
    for members in groups.values_mut() {
        members.sort_by_key(|&i| (rows[i].first_seen, rows[i].start));
        for (n, &i) in members.iter().enumerate() {
            if related[i] {
                continue;
            }
            if let Some(&j) = members[..n]
                .iter()
                .find(|&&j| rows[j].start < rows[i].until && rows[j].until > rows[i].start)
            {
                union(&mut parent, i, j);
            }
        }
    }

    // Each set of messages that is one hazard, under the key its oldest
    // message has; and threads holding an active message that is not in
    // effect.
    let mut sets: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..rows.len() {
        let root = find(&mut parent, i);
        sets.entry(root).or_default().push(i);
    }
    let mut ch = Changes::default();
    for members in sets.values() {
        let mut keys: Vec<(&DateTime<Utc>, &str)> = members
            .iter()
            .map(|&i| (&rows[i].first_seen, rows[i].event_key.as_str()))
            .collect();
        keys.sort();
        let into = keys[0].1;
        let others: Vec<&str> = keys
            .iter()
            .map(|k| k.1)
            .filter(|k| *k != into)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let stale = members
            .iter()
            .any(|&i| rows[i].active && !rows[i].in_effect);
        if others.is_empty() && !stale {
            continue;
        }
        let _writes = inner.writes.lock().await;
        let mut tx = inner.pool.begin().await?;
        join(&mut tx, into, &others, None).await?;
        recompute(&mut tx, &[into], &mut ch).await?;
        tx.commit().await?;
    }
    Ok(ch)
}

/// Forget documents fetched long ago (their feeds no longer list them), so
/// the table does not grow for ever; and NUTS boundaries old enough to be
/// worth fetching again (editions are revised every few years), which
/// happens the next time an alert names them.
pub(crate) async fn prune(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM cap_documents WHERE fetched_at < now() - interval '45 days'")
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM nuts_regions WHERE fetched_at < now() - interval '180 days'")
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "../../migrations")]
    async fn long_waits_stored_earlier_are_cut_short(pool: PgPool) {
        sqlx::query(
            "INSERT INTO cap_documents (url, source, done, attempts, retry_at)
             VALUES ('https://a.example/1', 'xx', false, 0, now() + interval '24 hours'),
                    ('https://a.example/2', 'xx', false, 1, now() + interval '5 minutes')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let docs = load_documents(&pool, chrono::Duration::hours(2))
            .await
            .unwrap();
        let latest = Utc::now() + chrono::Duration::hours(2);
        assert!(docs["https://a.example/1"].retry_at.unwrap() <= latest);
        assert!(
            docs["https://a.example/2"].retry_at.unwrap()
                < Utc::now() + chrono::Duration::minutes(6),
            "short waits stay"
        );
    }

    #[test]
    fn world_keys_cannot_collide_with_nws_keys() {
        let id = alert_id("met@example.org,ID-1");
        assert!(id.starts_with("cap:") && id.len() == 24);
        assert_eq!(id, alert_id("met@example.org,ID-1"));
        assert_ne!(id, alert_id("met@example.org,ID-2"));
        let thread = new_thread_key("met@example.org,ID-1");
        assert!(thread.starts_with("cap:"));
        assert_ne!(thread, id);
        // VTEC keys start with an office id, NWS ids with "urn:".
        for nws in ["KOUN.TO.W.0042.2026", "urn:oid:2.49.0.1.840.0.abc"] {
            assert!(!nws.starts_with("cap:"));
        }
    }
}
