//! CWA: Taiwan's composite reflectivity, from the Central Weather
//! Administration's anonymous open-data bucket on AWS:
//! `Observation/{product}.json` (`O-A0059-001`), one JSON document of about
//! 9 MB overwritten in place every 10 minutes, about 7 minutes after its
//! time. Only the newest frame exists.
//!
//! The frame's time is in the first kilobyte (`DateTime`), so a poll is a
//! ranged GET of the first [`HEAD_BYTES`] made conditional on the `ETag`
//! of the last one: a 304 while nothing is new, and the whole document is
//! downloaded once per frame. Its own `DateTime` is checked again after the
//! download, in case it was overwritten in between.

use crate::driver::{Cadence, Cx, Decoded, Decoder, Fetched, Need, ObsFile, Observed, Payload};
use crate::families::common;
use crate::idx::ByteRange;
use crate::lock;
use crate::remote::{FetchError, Validators};
use anyhow::ensure;
use chrono::{DateTime, Duration, Utc};
use std::sync::{Arc, Mutex};
use wx_core::SourceDef;
use wx_core::families::cwa as reg;
use wx_formats::Quantity;

pub const BUCKET_URL: &str = "https://cwaopendata.s3.ap-northeast-1.amazonaws.com";
/// Enough of the document to hold its `DateTime`, which sits at about byte
/// 900.
pub const HEAD_BYTES: u64 = 4096;
/// A frame is about 9 MB of text.
const MAX_FILE: u64 = 64 * 1024 * 1024;
/// A frame older than this is a source that has stopped: a failing task,
/// not a quiet one.
pub const STALE: Duration = Duration::minutes(45);

pub struct Cwa {
    base: String,
    /// The newest frame seen and the validators of the head it came from.
    head: Mutex<Option<(Validators, ObsFile)>>,
}

impl Default for Cwa {
    fn default() -> Self {
        Self::at(BUCKET_URL)
    }
}

impl Cwa {
    /// An adapter reading from another bucket (tests).
    pub fn at(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            head: Mutex::default(),
        }
    }

    pub fn url(&self, product: &str) -> String {
        format!("{}/Observation/{product}.json", self.base)
    }
}

/// The frame's time: the first `"DateTime": "..."` of the document, an ISO
/// 8601 time with its offset (`2026-11-07T04:30:00+08:00`).
pub fn date_time(text: &[u8]) -> Option<DateTime<Utc>> {
    // A head may end inside a multi-byte character (the document has
    // Chinese descriptions): what comes before the first bad byte is enough.
    let text = match std::str::from_utf8(text) {
        Ok(t) => t,
        Err(e) => std::str::from_utf8(&text[..e.valid_up_to()]).ok()?,
    };
    let after = &text[text.find("\"DateTime\"")? + "\"DateTime\"".len()..];
    let value = after
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    let value = &value[..value.find('"')?];
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|t| t.to_utc())
}

pub fn decoder() -> Decoder {
    Arc::new(|bytes: &[u8], need: &Need| {
        let d = wx_formats::cwa::decode(bytes)?;
        ensure!(
            d.quantity == Quantity::Reflectivity,
            "a CWA {} frame holding {:?}",
            need.product(),
            d.quantity
        );
        Ok(Decoded {
            field: d.field,
            valid: Some(d.valid),
        })
    })
}

#[async_trait::async_trait]
impl Observed for Cwa {
    fn check(&self, source: &SourceDef) -> anyhow::Result<()> {
        common::one_region(source, "CWA's composite")
    }

    fn cadence(&self, _: &SourceDef, _: &str) -> Cadence {
        // Written at 20:36:47 for 20:30 on 2026-11-06; the research saw 18:20
        // at 18:27.
        Cadence::every(
            Duration::minutes(reg::EVERY_MINUTES as i64),
            Duration::seconds(410),
            Duration::seconds(30),
        )
    }

    async fn list(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        product: &str,
        _: DateTime<Utc>,
    ) -> Result<Vec<ObsFile>, FetchError> {
        let url = self.url(product);
        let since = lock(&self.head).as_ref().map(|(v, _)| v.clone());
        let mut get = cx
            .get(&url)
            .range(ByteRange::new(0, HEAD_BYTES))
            .max_bytes(HEAD_BYTES);
        if let Some(v) = &since {
            get = get.if_changed(v);
        }
        let Some(reply) = get.send().await? else {
            *lock(&self.head) = None;
            return Err(FetchError::service(
                &url,
                "the document is gone (404): has it moved?",
            ));
        };
        let cached = lock(&self.head).as_ref().map(|(_, f)| f.clone());
        let valid = match (reply.not_modified, cached) {
            (true, Some(file)) => file.valid,
            _ => date_time(&reply.body).ok_or_else(|| {
                FetchError::object(
                    &url,
                    format!(
                        "no DateTime in the first {HEAD_BYTES} bytes: the document has changed"
                    ),
                )
            })?,
        };
        if valid < cx.now - STALE {
            return Err(FetchError::service(
                &url,
                format!("the newest frame is from {valid}: the source has stopped"),
            ));
        }
        if reply.not_modified
            && let Some((_, file)) = lock(&self.head).as_ref()
        {
            return Ok(vec![file.clone()]);
        }
        let file = ObsFile {
            valid,
            name: url,
            size: None,
        };
        *lock(&self.head) = Some((reply.validators, file.clone()));
        Ok(vec![file])
    }

    async fn fetch(
        &self,
        cx: &Cx<'_>,
        _: &SourceDef,
        _: &str,
        file: &ObsFile,
        needs: &[Need],
    ) -> Result<Fetched, FetchError> {
        let Some(bytes) = cx.get(&file.name).max_bytes(MAX_FILE).bytes().await? else {
            return Ok(Fetched::Missing);
        };
        // Overwritten since it was listed: that frame is gone, and the next
        // listing finds this one.
        let head = &bytes[..bytes.len().min(HEAD_BYTES as usize)];
        if date_time(head) != Some(file.valid) {
            *lock(&self.head) = None;
            return Ok(Fetched::Missing);
        }
        Ok(Fetched::all(
            needs,
            Payload::Custom {
                bytes,
                decode: decoder(),
            },
        ))
    }
}

#[cfg(test)]
mod tests;
