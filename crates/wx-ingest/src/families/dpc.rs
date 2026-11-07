//! DPC: the Italian national radar composite, through the backend of
//! `radar.protezionecivile.it`. The API is undocumented (read from the web
//! app's code), so everything it answers is checked, and every way it can
//! fail ends as "source unavailable" on the status page (the task backs
//! off and tries again) or, for one frame, as that frame being skipped;
//! never as a panic or a fast restart loop.
//!
//! * Newest time: `GET {API}/findLastProductByType?type={product}&lang=it`
//!   answers `{"total":1,"lastProducts":[{"productType":"VMI","time":<ms>,"period":"PT5M"}]}`.
//!   Earlier frames are every `period` before it, as the web app assumes.
//! * One frame: `POST {API}/downloadProduct` with
//!   `{"productType":"VMI","productDate":<ms>}` answers
//!   `{"bucket":..,"key":"VMI/DD-MM-YYYY-HH-MM.tif","url":<presigned S3 URL>}`,
//!   valid for 300 seconds; the GeoTIFF is then a GET of that URL. A time
//!   with no product is answered 500 with an S3 `HeadObject` error.
//!
//! Failures: the time list unreachable, refused, in another shape or stuck
//! for [`STALE`], and the link request failing or answering in another
//! shape, mean the API is down or has changed: a service failure, which
//! stops the task, and it tries again. Only the API's own "no such frame"
//! (the 500 carrying S3's `HeadObject` error, see [`is_no_such_frame`])
//! makes an older frame missing; it is then not asked for again for
//! [`MISSING_RETRY`], and asked once more if the driver still lists it.
//! For the newest frame, which the API has just announced, even that means
//! the API is failing. A refused download (the link expired while the
//! request waited its turn) is asked for once more with a fresh link;
//! after that the frame is charged like any broken file.

use crate::clock;
use crate::driver::{Cadence, Cx, Decoded, Decoder, Fetched, Need, ObsFile, Observed, Payload};
use crate::families::common;
use crate::lock;
use crate::remote::{Failure, FetchError};
use anyhow::ensure;
use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use wx_core::SourceDef;
use wx_core::families::dpc as reg;
use wx_formats::Quantity;
use wx_formats::dpc::Product;

pub const API: &str = "https://radar-api.protezionecivile.it";
/// The API's answers are under 2 kB.
const MAX_ANSWER: u64 = 64 * 1024;
/// A frame is under a megabyte.
const MAX_FILE: u64 = 32 * 1024 * 1024;
/// Frames listed at most, whatever the period the API states.
const MAX_FRAMES: usize = 200;
/// A frame the API said it does not have is not asked for again for this
/// long. Less than the ten minutes the driver lists behind its newest
/// frame, so a frame taken for missing by mistake is asked for again.
pub const MISSING_RETRY: Duration = Duration::minutes(4);
/// The API announcing nothing newer than this long ago is a failing source.
pub const STALE: Duration = Duration::minutes(45);

pub struct Dpc {
    api: String,
    /// The newest time the API announced, which must exist.
    newest: Mutex<Option<DateTime<Utc>>>,
    /// The newest time announced that the API then said it has no file
    /// for: the next listing fails while it is still the newest.
    newest_absent: Mutex<Option<DateTime<Utc>>>,
    /// Times the API said it has no frame for, with when it said so: not
    /// listed again for [`MISSING_RETRY`] (the driver lists a little
    /// behind the newest frame it has).
    missing: Mutex<BTreeMap<DateTime<Utc>, DateTime<Utc>>>,
}

impl Default for Dpc {
    fn default() -> Self {
        Self::at(API)
    }
}

impl Dpc {
    /// An adapter talking to another server (tests).
    pub fn at(api: &str) -> Self {
        Self {
            api: api.trim_end_matches('/').to_string(),
            newest: Mutex::default(),
            newest_absent: Mutex::default(),
            missing: Mutex::default(),
        }
    }

    pub fn last_url(&self, product: &str) -> String {
        format!("{}/findLastProductByType?type={product}&lang=it", self.api)
    }

    pub fn download_url(&self) -> String {
        format!("{}/downloadProduct", self.api)
    }

    /// A fresh download link for `product` at `valid`, or `None` when the API
    /// says there is no such frame.
    async fn link(
        &self,
        cx: &Cx<'_>,
        product: &str,
        valid: DateTime<Utc>,
    ) -> Result<Option<String>, FetchError> {
        let url = self.download_url();
        let reply = cx
            .post(&url, "application/json", download_request(product, valid))
            .max_bytes(MAX_ANSWER)
            .bytes()
            .await;
        let body = match reply {
            Ok(Some(b)) => b,
            Ok(None) => return Err(unavailable(&url, "answered 404")),
            Err(e) if is_no_such_frame(&e) && *lock(&self.newest) != Some(valid) => {
                tracing::debug!(product, %valid, "DPC has no frame at this time: {e}");
                return Ok(None);
            }
            Err(e) if is_no_such_frame(&e) => {
                *lock(&self.newest_absent) = Some(valid);
                return Err(unavailable(
                    &url,
                    format!("has no file for {valid}, the newest frame it announced: {e}"),
                ));
            }
            Err(e) => return Err(unavailable(&url, e)),
        };
        let (key, link) = parse_link(&body).map_err(|e| unavailable(&url, e))?;
        let expected = key_for(product, valid);
        if key != expected {
            return Err(FetchError::object(
                &url,
                format!("asked for {expected}, offered {key}"),
            ));
        }
        Ok(Some(link))
    }
}

/// The API's answer for a time it has no frame for: HTTP 500 with S3's
/// `HeadObject` error (`{"error":"S3 error: An error occurred (403) when
/// calling the HeadObject operation: Forbidden"}` on 2026-11-06). Any other
/// failure, a 500 included, is the API or the network failing.
pub fn is_no_such_frame(e: &FetchError) -> bool {
    e.answer()
        .is_some_and(|(status, body)| status == 500 && body.contains("HeadObject"))
}

/// A failure that means the API is down or has changed.
fn unavailable(what: &str, e: impl std::fmt::Display) -> FetchError {
    FetchError::service(what, format!("the DPC radar API (undocumented) {e}"))
}

pub fn download_request(product: &str, valid: DateTime<Utc>) -> String {
    format!(
        "{{\"productType\":\"{product}\",\"productDate\":{}}}",
        valid.timestamp_millis()
    )
}

/// The object key the API names for a frame: `VMI/06-11-2026-20-20.tif`.
pub fn key_for(product: &str, valid: DateTime<Utc>) -> String {
    format!("{product}/{}.tif", valid.format("%d-%m-%Y-%H-%M"))
}

/// The newest time and the period between frames from the
/// `findLastProductByType` answer.
pub fn parse_last(body: &[u8], product: &str) -> Result<(DateTime<Utc>, Duration), String> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| format!("answered something that is not JSON ({e})"))?;
    let last = v
        .get("lastProducts")
        .and_then(|l| l.as_array())
        .and_then(|l| {
            l.iter()
                .find(|p| p.get("productType").and_then(|t| t.as_str()) == Some(product))
        })
        .ok_or_else(|| format!("answered without lastProducts[productType={product}]"))?;
    let ms = last
        .get("time")
        .and_then(serde_json::Value::as_i64)
        .ok_or("answered without a time in milliseconds")?;
    let time =
        DateTime::from_timestamp_millis(ms).ok_or_else(|| format!("answered the time {ms}"))?;
    let period = match last.get("period").and_then(|p| p.as_str()) {
        Some(p) => parse_minutes(p).ok_or_else(|| format!("answered the period {p:?}"))?,
        None => Duration::minutes(reg::EVERY_MINUTES as i64),
    };
    Ok((time, period))
}

/// `PT5M` as five minutes; whole minutes from 1 to 60 only.
fn parse_minutes(iso: &str) -> Option<Duration> {
    let m: i64 = iso.strip_prefix("PT")?.strip_suffix('M')?.parse().ok()?;
    (1..=60).contains(&m).then(|| Duration::minutes(m))
}

/// The key and the presigned link from the `downloadProduct` answer. The
/// link must be HTTPS to an S3 host: the adapter fetches nothing else the
/// API might name.
pub fn parse_link(body: &[u8]) -> Result<(String, String), String> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| format!("answered something that is not JSON ({e})"))?;
    let field = |k: &str| v.get(k).and_then(|s| s.as_str()).map(str::to_string);
    let (Some(key), Some(link)) = (field("key"), field("url")) else {
        return Err("answered without a key and a url".to_string());
    };
    let host = url::Url::parse(&link)
        .ok()
        .filter(|u| u.scheme() == "https")
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase));
    match host {
        Some(h) if h.ends_with(".amazonaws.com") => Ok((key, link)),
        _ => Err("offered a download link that is not HTTPS on Amazon S3".to_string()),
    }
}

fn product_of(name: &str) -> Option<Product> {
    match name {
        "VMI" => Some(Product::Vmi),
        "SRI" => Some(Product::Sri),
        _ => None,
    }
}

/// Decodes a frame asked for at `valid`, checking the file says the same
/// product and time (its `DocumentName`; a file without one is taken at
/// the time asked for).
pub fn decoder(valid: DateTime<Utc>) -> Decoder {
    Arc::new(move |bytes: &[u8], need: &Need| {
        let product = product_of(need.product())
            .ok_or_else(|| anyhow::anyhow!("no DPC product {}", need.product()))?;
        let d = match wx_formats::dpc::decode(bytes) {
            Ok(d) => d,
            Err(_) => wx_formats::dpc::decode_as(bytes, product, valid)?,
        };
        let quantity = match product {
            Product::Vmi => Quantity::Reflectivity,
            Product::Sri => Quantity::RainRate,
        };
        ensure!(
            d.quantity == quantity,
            "a DPC {} frame holding {:?}",
            need.product(),
            d.quantity
        );
        ensure!(
            d.valid == valid,
            "the frame asked for at {valid} is for {}",
            d.valid
        );
        Ok(Decoded {
            field: d.field,
            valid: Some(d.valid),
        })
    })
}

#[async_trait::async_trait]
impl Observed for Dpc {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        common::one_region(source, "DPC's composite")
    }

    fn cadence(&self, _: &SourceDef, _: &str) -> Cadence {
        // The 20:20 frame was announced by 20:31 on 2026-11-06 (the research
        // saw 18:20 written at 18:27); polled gently, as an API that was not
        // published for this.
        Cadence::every(
            Duration::minutes(reg::EVERY_MINUTES as i64),
            Duration::seconds(450),
            Duration::seconds(60),
        )
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        product: &str,
        from: DateTime<Utc>,
    ) -> Result<Vec<ObsFile>, FetchError> {
        let url = self.last_url(product);
        let body = match cx.get(&url).max_bytes(MAX_ANSWER).bytes().await {
            Ok(Some(b)) => b,
            Ok(None) => return Err(unavailable(&url, "answered 404")),
            Err(e) => return Err(unavailable(&url, e)),
        };
        let (newest, period) = parse_last(&body, product).map_err(|e| unavailable(&url, e))?;
        if newest < cx.now - STALE {
            return Err(unavailable(
                &url,
                format!("has announced nothing newer than {newest}"),
            ));
        }
        if *lock(&self.newest_absent) == Some(newest) {
            return Err(unavailable(
                &url,
                format!("announced {newest} but has no file for it"),
            ));
        }
        *lock(&self.newest) = Some(newest);
        let mut missing = lock(&self.missing);
        missing.retain(|t, at| *t >= from && cx.now - *at < MISSING_RETRY);
        let mut t = clock::floor(from, period);
        if t < from {
            t += period;
        }
        let mut out = Vec::new();
        while t <= newest && out.len() < MAX_FRAMES {
            if !missing.contains_key(&t) {
                out.push(ObsFile {
                    valid: t,
                    name: key_for(product, t),
                    size: None,
                });
            }
            t += period;
        }
        Ok(out)
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        product: &str,
        file: &ObsFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let mut fresh_link_taken = false;
        loop {
            let Some(link) = self.link(cx, product, file.valid).await? else {
                lock(&self.missing).insert(file.valid, cx.now);
                return Ok(Fetched::Missing);
            };
            match cx.get(&link).max_bytes(MAX_FILE).bytes().await {
                Ok(Some(bytes)) => {
                    return Ok(Fetched::all(
                        needs,
                        Payload::Custom {
                            bytes,
                            decode: decoder(file.valid),
                        },
                    ));
                }
                Ok(None) => {
                    lock(&self.missing).insert(file.valid, cx.now);
                    return Ok(Fetched::Missing);
                }
                Err(e) if e.failure == Failure::Object && !fresh_link_taken => {
                    tracing::debug!(product, valid = %file.valid, "DPC download refused, asking for a fresh link: {e}");
                    fresh_link_taken = true;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests;
