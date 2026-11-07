//! Web Push delivery: encrypt (RFC 8291), sign (RFC 8292), POST (RFC 8030).

pub(crate) mod ece;
pub(crate) mod vapid;

use anyhow::{Context, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use std::time::Duration;
use url::Url;
use uuid::Uuid;
use vapid::VapidSigner;

#[derive(Debug, Clone)]
pub(crate) struct Subscription {
    pub id: Uuid,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PushOptions {
    pub ttl_secs: u32,
    pub urgency: &'static str,
    pub topic: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Delivery {
    Sent,
    /// 404 or 410: the browser dropped the subscription.
    Gone,
    /// `transient`: the push service was busy or unreachable (429, 5xx,
    /// network), so trying again later may work.
    Failed {
        reason: String,
        transient: bool,
    },
}

pub(crate) struct PushClient {
    http: reqwest::Client,
    signer: Option<VapidSigner>,
    retry_base: Duration,
}

/// Attempts per message for 429 and 5xx answers.
const ATTEMPTS: u32 = 3;

impl PushClient {
    pub fn new(signer: Option<VapidSigner>, retry_base: Duration) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            // A redirect would carry the VAPID token to another origin.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("building the push HTTP client")?;
        Ok(Self {
            http,
            signer,
            retry_base,
        })
    }

    pub fn configured(&self) -> bool {
        self.signer.is_some()
    }

    pub async fn send(&self, sub: &Subscription, payload: &[u8], opts: &PushOptions) -> Delivery {
        match self.try_send(sub, payload, opts).await {
            Ok(d) => d,
            Err(e) => Delivery::Failed {
                reason: format!("{e:#}"),
                transient: false,
            },
        }
    }

    async fn try_send(
        &self,
        sub: &Subscription,
        payload: &[u8],
        opts: &PushOptions,
    ) -> anyhow::Result<Delivery> {
        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| anyhow!("push is not configured (no VAPID keys)"))?;
        let endpoint = Url::parse(&sub.endpoint).context("invalid endpoint")?;
        let ua_public = p256::PublicKey::from_sec1_bytes(
            &vapid::decode_b64url(&sub.p256dh).context("invalid p256dh")?,
        )
        .map_err(|_| anyhow!("invalid p256dh"))?;
        let auth: [u8; 16] = vapid::decode_b64url(&sub.auth)
            .context("invalid auth")?
            .try_into()
            .map_err(|_| anyhow!("auth secret must be 16 bytes"))?;
        let body = bytes::Bytes::from(ece::encrypt(payload, &ua_public, &auth)?);

        let mut last_error = String::new();
        for attempt in 0..ATTEMPTS {
            let authorization = signer.authorization(&endpoint, chrono::Utc::now().timestamp())?;
            let mut req = self
                .http
                .post(endpoint.clone())
                .header("TTL", opts.ttl_secs.to_string())
                .header("Urgency", opts.urgency)
                .header(reqwest::header::CONTENT_ENCODING, "aes128gcm")
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .header(reqwest::header::AUTHORIZATION, authorization)
                .body(body.clone());
            if let Some(topic) = &opts.topic {
                req = req.header("Topic", topic);
            }
            let retry_in = match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        return Ok(Delivery::Sent);
                    }
                    if status == StatusCode::NOT_FOUND || status == StatusCode::GONE {
                        return Ok(Delivery::Gone);
                    }
                    let retry_after = crate::upstream::retry_after(&resp);
                    let reason = short_reason(resp).await;
                    last_error = format!("HTTP {}{reason}", status.as_u16());
                    if status != StatusCode::TOO_MANY_REQUESTS && !status.is_server_error() {
                        return Ok(Delivery::Failed {
                            reason: last_error,
                            transient: false,
                        });
                    }
                    retry_after
                }
                // Not `{e}`: reqwest errors include the URL, and the endpoint
                // is a credential for the subscription.
                Err(e) => {
                    last_error = transport_error(&e);
                    None
                }
            };
            if attempt + 1 < ATTEMPTS {
                let backoff = self.retry_base * 2u32.pow(attempt);
                tokio::time::sleep(
                    retry_in.map_or(backoff, |r| r.min(self.retry_base * 10).max(backoff)),
                )
                .await;
            }
        }
        Ok(Delivery::Failed {
            reason: last_error,
            transient: true,
        })
    }
}

/// The push `Topic` for a hazard: at most 32 characters of the URL-safe
/// base64 alphabet (RFC 8030 section 5.4), so derived from a hash.
pub(crate) fn topic_for(event_key: &str) -> String {
    let digest = Sha256::digest(event_key.as_bytes());
    B64.encode(&digest[..24])
}

/// The start of the push service's explanation (e.g. Apple's
/// `{"reason":"BadJwtToken"}`), which is what makes a failure debuggable.
async fn short_reason(resp: reqwest::Response) -> String {
    match resp.text().await {
        Ok(t) if !t.trim().is_empty() => {
            let t: String = t
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(160)
                .collect();
            format!(": {t}")
        }
        _ => String::new(),
    }
}

pub(crate) fn transport_error(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "connection failed"
    } else if e.is_request() {
        "request failed"
    } else if e.is_body() || e.is_decode() {
        "bad response body"
    } else {
        "transport error"
    };
    kind.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_is_short_url_safe_and_stable() {
        let t = topic_for("KOUN.TO.W.0042.2026");
        assert_eq!(t.len(), 32);
        assert!(
            t.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_eq!(t, topic_for("KOUN.TO.W.0042.2026"));
        assert_ne!(t, topic_for("KOUN.TO.W.0043.2026"));
    }
}
