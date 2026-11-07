//! The production [`Transport`]: one `reqwest` client (one connection pool)
//! for every host.

use super::{Request, Response, Transport, TransportError};
use anyhow::Context;
use std::time::Duration;

/// Error bodies are read only this far: they are for the log.
const MAX_ERROR_BODY: u64 = 64 * 1024;

pub struct HttpTransport {
    client: reqwest::Client,
}

impl HttpTransport {
    /// Builds the client only: no I/O.
    pub fn new(user_agent: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .connect_timeout(Duration::from_secs(10))
            // A stalled transfer fails fast; a slow but moving one gets time.
            .read_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(300))
            .pool_idle_timeout(Duration::from_secs(90))
            // Bodies must arrive as stored (`.gz` and `.bz2` files are data,
            // and byte ranges index the stored bytes), so never negotiate
            // encodings.
            .no_gzip()
            .no_brotli()
            .no_zstd()
            .no_deflate()
            // Bulk ranged GETs from object stores do better on HTTP/1.1 with
            // several connections than multiplexed on one.
            .http1_only()
            .build()
            .context("building the HTTP client")?;
        Ok(Self { client })
    }
}

fn transport_error(e: reqwest::Error) -> TransportError {
    if e.is_builder() {
        return TransportError::Invalid(e.to_string());
    }
    let mut msg = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    TransportError::Transient(msg)
}

#[async_trait::async_trait]
impl Transport for HttpTransport {
    async fn get(&self, req: &Request) -> Result<Response, TransportError> {
        read(self.client.get(&req.url), req).await
    }

    async fn post(&self, req: &Request, body: &bytes::Bytes) -> Result<Response, TransportError> {
        read(self.client.post(&req.url).body(body.clone()), req).await
    }
}

/// Send a request with `req`'s headers and read the answer within its limit.
async fn read(
    mut builder: reqwest::RequestBuilder,
    req: &Request,
) -> Result<Response, TransportError> {
    for (name, value) in &req.headers {
        builder = builder.header(*name, value);
    }
    let mut resp = builder.send().await.map_err(transport_error)?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let success = (200..300).contains(&status);
    let limit = if success {
        req.max_bytes
    } else {
        MAX_ERROR_BODY
    };
    if success && resp.content_length().is_some_and(|len| len > limit) {
        return Err(TransportError::TooLarge(limit));
    }
    let capacity = resp.content_length().unwrap_or(0).min(limit) as usize;
    let mut body = Vec::with_capacity(capacity);
    while let Some(chunk) = resp.chunk().await.map_err(transport_error)? {
        if (body.len() + chunk.len()) as u64 > limit {
            if success {
                return Err(TransportError::TooLarge(limit));
            }
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Response {
        status,
        headers,
        body: body.into(),
    })
}
