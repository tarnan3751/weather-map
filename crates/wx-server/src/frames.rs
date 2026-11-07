//! `/api/frames` and `/api/frames/{layer}`, serialized once per change.
//!
//! Each layer's timeline is kept as JSON (and, once a client asks for gzip,
//! as deflate pieces) for as long as the catalog hands out the same shared
//! snapshot of it, which it does until the timeline changes. So what is
//! served is always the catalog's current timeline, and a client that has
//! heard of a change and then asks gets it. The bytes are exactly those the
//! timeline's JSON value would serialize to: the cache adds nothing on the
//! wire but the `ETag` (weak, the same for every encoding) and
//! `Cache-Control: no-cache`.

use crate::body::{self, Deflated};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::Response;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use std::sync::{Arc, OnceLock, RwLock};
use wx_core::{FrameCatalog, FrameKind, LAYERS, Timeline};

/// What the status page shows of a layer's timeline.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub frames: usize,
    pub oldest_valid: Option<DateTime<Utc>>,
    pub newest_valid: Option<DateTime<Utc>>,
    pub newest_observed: Option<DateTime<Utc>>,
}

impl Summary {
    fn of(t: &Timeline) -> Self {
        Self {
            frames: t.frames.len(),
            oldest_valid: t.frames.first().map(|f| f.valid),
            newest_valid: t.frames.last().map(|f| f.valid),
            newest_observed: t
                .frames
                .iter()
                .rev()
                .find(|f| f.kind == FrameKind::Observed)
                .map(|f| f.valid),
        }
    }
}

/// One layer's timeline as served.
struct Entry {
    /// The snapshot it was made from. Held, so its identity cannot be
    /// taken by a later one.
    snapshot: Arc<Timeline>,
    id: &'static str,
    /// The JSON of the timeline's `frames` and of its `parts`.
    frames: Bytes,
    parts: Bytes,
    /// Of `/api/frames/{layer}`; `/api/frames` combines these.
    etag: HeaderValue,
    /// `frames` and `parts` deflated, made when first needed.
    deflated: OnceLock<[Deflated; 2]>,
}

impl Entry {
    fn build(id: &'static str, snapshot: Arc<Timeline>) -> Self {
        // Through `Value`, as the handlers always did, so keys stay in the
        // same (sorted) order.
        let json = |v| serde_json::to_vec(&v).expect("a timeline serializes");
        let frames = json(serde_json::to_value(&snapshot.frames).expect("frames serialize"));
        let parts = json(serde_json::to_value(&snapshot.parts).expect("parts serialize"));
        Self {
            etag: body::etag_of(&layer_pieces(id, &frames, &parts)),
            snapshot,
            id,
            frames: frames.into(),
            parts: parts.into(),
            deflated: OnceLock::new(),
        }
    }

    fn deflated(&self) -> &[Deflated; 2] {
        self.deflated.get_or_init(|| {
            [
                Deflated::compress(&self.frames),
                Deflated::compress(&self.parts),
            ]
        })
    }
}

/// `{"frames":F,"layer":"id","parts":P}` around the cached `F` and `P`.
fn layer_pieces<'a>(
    id: &str,
    frames: &'a [u8],
    parts: &'a [u8],
) -> [std::borrow::Cow<'a, [u8]>; 5] {
    use std::borrow::Cow::{Borrowed, Owned};
    let layer = serde_json::to_string(id).expect("an id serializes");
    [
        Borrowed(b"{\"frames\":"),
        Borrowed(frames),
        Owned(format!(",\"layer\":{layer},\"parts\":").into_bytes()),
        Borrowed(parts),
        Borrowed(b"}"),
    ]
}

struct Slot {
    current: RwLock<Option<Arc<Entry>>>,
    /// Held while the entry is rebuilt, so a crowd asking right after a
    /// change shares one rebuild.
    building: tokio::sync::Mutex<()>,
}

pub struct Timelines {
    catalog: Arc<FrameCatalog>,
    /// Indexed like [`LAYERS`].
    slots: Vec<Slot>,
}

impl Timelines {
    pub fn new(catalog: Arc<FrameCatalog>) -> Self {
        let slots = LAYERS
            .iter()
            .map(|_| Slot {
                current: RwLock::new(None),
                building: tokio::sync::Mutex::new(()),
            })
            .collect();
        Self { catalog, slots }
    }

    /// The entry for `LAYERS[i]`'s current timeline.
    async fn entry(&self, i: usize) -> Arc<Entry> {
        let (slot, id) = (&self.slots[i], LAYERS[i].id);
        let fresh = |snapshot: &Arc<Timeline>| {
            let current = slot.current.read().unwrap_or_else(|p| p.into_inner());
            current
                .clone()
                .filter(|e| Arc::ptr_eq(&e.snapshot, snapshot))
        };
        if let Some(e) = fresh(&self.catalog.shared_timeline(id)) {
            return e;
        }
        let _building = slot.building.lock().await;
        let snapshot = self.catalog.shared_timeline(id);
        if let Some(e) = fresh(&snapshot) {
            return e;
        }
        let entry = tokio::task::spawn_blocking(move || Arc::new(Entry::build(id, snapshot)))
            .await
            .expect("serializing a timeline does not panic");
        *slot.current.write().unwrap_or_else(|p| p.into_inner()) = Some(entry.clone());
        entry
    }

    /// Every layer's summary, in registry order: what the status page shows.
    pub fn summaries(&self) -> Vec<Summary> {
        LAYERS
            .iter()
            .map(|l| Summary::of(&self.catalog.shared_timeline(l.id)))
            .collect()
    }

    /// `/api/frames/{layer}`; `None` for an unknown layer.
    pub async fn layer(&self, id: &str, request: &HeaderMap) -> Option<Response> {
        let i = LAYERS.iter().position(|l| l.id == id)?;
        let e = self.entry(i).await;
        if accepts_and_changed(request, &e.etag) {
            deflate_off_runtime(std::slice::from_ref(&e)).await;
        }
        let pieces = layer_pieces(e.id, &e.frames, &e.parts);
        Some(body::respond(
            request,
            &e.etag,
            "no-cache",
            || pieces.concat().into(),
            || {
                let [f, p] = e.deflated();
                let [a, b, c] = [0, 2, 4].map(|k| Deflated::stored(&pieces[k]));
                body::gzip([&a, f, &b, p, &c])
            },
        ))
    }

    /// `/api/frames`: `{"server_time":..,"timelines":{"<id>":{"frames":..,"parts":..},..}}`
    /// with the layers in id order. Its `ETag` stands for the timelines;
    /// `server_time` is when the body was made.
    pub async fn all(&self, request: &HeaderMap) -> Response {
        let mut entries = Vec::with_capacity(LAYERS.len());
        for i in 0..LAYERS.len() {
            entries.push(self.entry(i).await);
        }
        entries.sort_by_key(|e| e.id);
        let etags: Vec<&[u8]> = entries.iter().map(|e| e.etag.as_bytes()).collect();
        let etag = body::etag_of(&etags);
        if accepts_and_changed(request, &etag) {
            deflate_off_runtime(&entries).await;
        }
        // The pieces between the cached ones, which carry the time.
        let mut glue: Vec<Vec<u8>> = Vec::with_capacity(entries.len() * 2 + 1);
        let now = serde_json::to_string(&Utc::now()).expect("a time serializes");
        let mut open = format!("{{\"server_time\":{now},\"timelines\":{{").into_bytes();
        for (k, e) in entries.iter().enumerate() {
            if k > 0 {
                open.push(b',');
            }
            let id = serde_json::to_string(e.id).expect("an id serializes");
            open.extend_from_slice(format!("{id}:{{\"frames\":").as_bytes());
            glue.push(std::mem::take(&mut open));
            glue.push(b",\"parts\":".to_vec());
            open.push(b'}');
        }
        open.extend_from_slice(b"}}");
        glue.push(open);
        let json = || {
            let mut out = Vec::with_capacity(
                entries
                    .iter()
                    .map(|e| e.frames.len() + e.parts.len() + 64)
                    .sum(),
            );
            for (k, e) in entries.iter().enumerate() {
                out.extend_from_slice(&glue[2 * k]);
                out.extend_from_slice(&e.frames);
                out.extend_from_slice(&glue[2 * k + 1]);
                out.extend_from_slice(&e.parts);
            }
            out.extend_from_slice(&glue[2 * entries.len()]);
            out.into()
        };
        let gzip = || {
            let stored: Vec<Deflated> = glue.iter().map(|g| Deflated::stored(g)).collect();
            let mut pieces = Vec::with_capacity(stored.len() + entries.len() * 2);
            for (k, e) in entries.iter().enumerate() {
                let [f, p] = e.deflated();
                pieces.extend([&stored[2 * k], f, &stored[2 * k + 1], p]);
            }
            pieces.push(&stored[2 * entries.len()]);
            body::gzip(pieces)
        };
        body::respond(request, &etag, "no-cache", json, gzip)
    }
}

/// The reply will be a gzipped body (not a 304).
fn accepts_and_changed(request: &HeaderMap, etag: &HeaderValue) -> bool {
    body::accepts_gzip(request) && !body::not_modified(request, etag)
}

/// Compress what has not been compressed yet on a blocking thread, so the
/// first gzipped reply after a change does not stall the runtime.
async fn deflate_off_runtime(entries: &[Arc<Entry>]) {
    let todo: Vec<Arc<Entry>> = entries
        .iter()
        .filter(|e| e.deflated.get().is_none())
        .cloned()
        .collect();
    if !todo.is_empty() {
        tokio::task::spawn_blocking(move || {
            todo.iter().for_each(|e| {
                e.deflated();
            })
        })
        .await
        .expect("compressing does not panic");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::tests::hrrr_temp;
    use axum::http::header;
    use serde_json::json;
    use std::io::Read;

    fn gunzip(b: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(b)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    async fn body_of(r: Response) -> (axum::http::StatusCode, HeaderMap, Vec<u8>) {
        use http_body_util::BodyExt;
        let (parts, body) = r.into_parts();
        // A body of known length, which the connection sends as `Content-Length`.
        let exact = axum::body::HttpBody::size_hint(&body).exact();
        let bytes = body.collect().await.unwrap().to_bytes().to_vec();
        assert_eq!(exact, Some(bytes.len() as u64));
        (parts.status, parts.headers, bytes)
    }

    fn gzip_request() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
        h
    }

    #[tokio::test]
    async fn bytes_are_what_the_json_value_gives() {
        let (_, catalog, _) = hrrr_temp().await;
        let cache = Arc::new(Timelines::new(catalog.clone()));
        let t = catalog.timeline("temp");
        let expected =
            serde_json::to_vec(&json!({ "layer": "temp", "frames": t.frames, "parts": t.parts }))
                .unwrap();
        let (_, h, plain) = body_of(cache.layer("temp", &HeaderMap::new()).await.unwrap()).await;
        assert_eq!(plain, expected);
        assert!(h.get(header::CONTENT_ENCODING).is_none());
        let (_, h, gz) = body_of(cache.layer("temp", &gzip_request()).await.unwrap()).await;
        assert_eq!(h[header::CONTENT_ENCODING], "gzip");
        assert_eq!(gunzip(&gz), expected);

        let (_, _, all) = body_of(cache.all(&gzip_request()).await).await;
        let all: serde_json::Value = serde_json::from_slice(&gunzip(&all)).unwrap();
        let (_, _, plain_all) = body_of(cache.all(&HeaderMap::new()).await).await;
        let plain_all: serde_json::Value = serde_json::from_slice(&plain_all).unwrap();
        let expected_all =
            json!({ "server_time": all["server_time"], "timelines": catalog.timelines() });
        assert_eq!(all, expected_all);
        assert_eq!(plain_all["timelines"], expected_all["timelines"]);
        assert!(all["server_time"].as_str().unwrap().ends_with('Z'));
        assert!(cache.layer("nope", &HeaderMap::new()).await.is_none());
    }

    #[tokio::test]
    async fn a_change_is_served_at_once() {
        let (_, catalog, part) = hrrr_temp().await;
        let cache = Arc::new(Timelines::new(catalog.clone()));
        let (_, h, _) = body_of(cache.layer("temp", &HeaderMap::new()).await.unwrap()).await;
        let etag = h[header::ETAG].clone();
        let (_, all_h, _) = body_of(cache.all(&HeaderMap::new()).await).await;
        let all_etag = all_h[header::ETAG].clone();
        let mut revalidate = HeaderMap::new();
        revalidate.insert(header::IF_NONE_MATCH, etag.clone());
        let (status, _, body) = body_of(cache.layer("temp", &revalidate).await.unwrap()).await;
        assert_eq!((status.as_u16(), body.len()), (304, 0));
        // Unchanged timelines are not serialized again.
        let temp = LAYERS.iter().position(|l| l.id == "temp").unwrap();
        let before = cache.entry(temp).await;
        assert!(Arc::ptr_eq(&before, &cache.entry(temp).await));

        // The next hour from the same run joins the timeline.
        let mut next = part.clone();
        next.valid += chrono::Duration::hours(1);
        next.id = wx_core::Part::forecast_id("hrrr", part.run.unwrap(), 240);
        catalog.insert("temp", next, Utc::now()).unwrap();
        let (status, h, body) = body_of(cache.layer("temp", &revalidate).await.unwrap()).await;
        assert_eq!(status.as_u16(), 200);
        assert_ne!(h[header::ETAG], etag);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["frames"].as_array().unwrap().len(), 2);
        let (_, all_h, _) = body_of(cache.all(&HeaderMap::new()).await).await;
        assert_ne!(all_h[header::ETAG], all_etag);
        assert_eq!(cache.summaries()[temp].frames, 2);
    }
}
