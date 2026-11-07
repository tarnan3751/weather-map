//! Fetching third-party feeds politely and safely.
//!
//! * Only http(s) URLs on public hosts are fetched: names are resolved by
//!   [`PublicResolver`], which drops loopback, private, link-local and
//!   other non-public addresses, IP literals are checked directly, and the
//!   same rules apply to every redirect. A link found in a feed therefore
//!   cannot reach the server's own network.
//! * At most `per_host` requests run against one host and `global` in
//!   all; a slow host only slows the sources it serves.
//! * Bodies are read up to a size limit, requests have a deadline, and
//!   `ETag` / `Last-Modified` are sent back when the host gave them.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::{Host, Url};

/// Longest wait a `Retry-After` is honoured for.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Default)]
pub(crate) struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl Validators {
    pub fn any(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }
}

#[derive(Debug)]
pub(crate) enum Fetched {
    NotModified,
    Body {
        bytes: Vec<u8>,
        validators: Validators,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum FetchError {
    #[error("{0}")]
    Refused(String),
    #[error("HTTP {status}")]
    Status {
        status: u16,
        retry_after: Option<Duration>,
    },
    #[error("{0}")]
    Transport(String),
    #[error("response is larger than {0} bytes")]
    TooLarge(usize),
}

impl FetchError {
    /// The host asked us to wait this long before asking again.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// `true` for answers that will not change by asking again soon (the
    /// document does not exist, or is not ours to read).
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Refused(_) | Self::TooLarge(_))
            || matches!(self, Self::Status { status, .. } if matches!(status, 400 | 401 | 403 | 404 | 410 | 451))
    }
}

pub(crate) struct Http {
    client: reqwest::Client,
    allow_private: bool,
    per_host: usize,
    hosts: Mutex<HashMap<String, Arc<Semaphore>>>,
    global: Arc<Semaphore>,
}

impl Http {
    pub fn new(
        user_agent: &str,
        per_host: usize,
        global: usize,
        allow_private: bool,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(user_agent.trim())
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(60))
            .dns_resolver(Arc::new(PublicResolver { allow_private }))
            .redirect(redirect_policy(allow_private))
            .build()?;
        Ok(Self {
            client,
            allow_private,
            per_host: per_host.max(1),
            hosts: Mutex::new(HashMap::new()),
            global: Arc::new(Semaphore::new(global.max(1))),
        })
    }

    /// Wait for a slot on `host` and a global one.
    async fn slots(&self, host: &str) -> (OwnedSemaphorePermit, OwnedSemaphorePermit) {
        let per_host = {
            let mut hosts = crate::lock(&self.hosts);
            hosts
                .entry(host.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(self.per_host)))
                .clone()
        };
        // Host first: waiting for a busy host must not hold a global slot.
        let host_permit = per_host
            .acquire_owned()
            .await
            .expect("semaphores are never closed");
        let global = self
            .global
            .clone()
            .acquire_owned()
            .await
            .expect("semaphores are never closed");
        (host_permit, global)
    }

    /// GET `url` within `timeout`, reading at most `limit` bytes.
    pub async fn get(
        &self,
        url: &str,
        validators: &Validators,
        timeout: Duration,
        limit: usize,
    ) -> Result<Fetched, FetchError> {
        let parsed = check_url(url, self.allow_private)?;
        let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
        let _slots = self.slots(&host).await;
        let mut req = self.client.get(parsed).timeout(timeout);
        if let Some(etag) = &validators.etag {
            req = req.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        if let Some(lm) = &validators.last_modified {
            req = req.header(reqwest::header::IF_MODIFIED_SINCE, lm);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| FetchError::Transport(describe(&e)))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(Fetched::NotModified);
        }
        if !status.is_success() {
            return Err(FetchError::Status {
                status: status.as_u16(),
                retry_after: crate::upstream::retry_after(&resp).map(|d| d.min(MAX_RETRY_AFTER)),
            });
        }
        let header = |name| {
            resp.headers()
                .get(name)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty() && v.len() <= 256)
        };
        let validators = Validators {
            etag: header(reqwest::header::ETAG),
            last_modified: header(reqwest::header::LAST_MODIFIED),
        };
        let bytes = read_limited(resp, limit).await?;
        Ok(Fetched::Body { bytes, validators })
    }
}

async fn read_limited(mut resp: reqwest::Response, limit: usize) -> Result<Vec<u8>, FetchError> {
    if resp.content_length().is_some_and(|n| n > limit as u64) {
        return Err(FetchError::TooLarge(limit));
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| FetchError::Transport(describe(&e)))?
    {
        if buf.len() + chunk.len() > limit {
            return Err(FetchError::TooLarge(limit));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// An http(s) URL without credentials whose host, if an address, is public.
pub(crate) fn check_url(raw: &str, allow_private: bool) -> Result<Url, FetchError> {
    let url = Url::parse(raw).map_err(|_| FetchError::Refused("not a URL".into()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::Refused(format!(
            "{} URLs are not fetched",
            url.scheme()
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::Refused("URL carries credentials".into()));
    }
    if !allow_private {
        match url.host() {
            Some(Host::Domain(d)) => {
                let d = d.trim_end_matches('.').to_ascii_lowercase();
                if d == "localhost"
                    || d.ends_with(".localhost")
                    || d.ends_with(".local")
                    || d.ends_with(".internal")
                    || !d.contains('.')
                {
                    return Err(FetchError::Refused("not a public host".into()));
                }
            }
            Some(Host::Ipv4(ip)) if is_public(IpAddr::V4(ip)) => {}
            Some(Host::Ipv6(ip)) if is_public(IpAddr::V6(ip)) => {}
            _ => return Err(FetchError::Refused("not a public host".into())),
        }
    }
    Ok(url)
}

fn redirect_policy(allow_private: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.error("too many redirects");
        }
        match check_url(attempt.url().as_str(), allow_private) {
            Ok(_) => attempt.follow(),
            Err(e) => attempt.error(format!("redirect refused: {e}")),
        }
    })
}

/// Resolves names to public addresses only.
struct PublicResolver {
    allow_private: bool,
}

impl reqwest::dns::Resolve for PublicResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let allow_private = self.allow_private;
        Box::pin(async move {
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let usable: Vec<SocketAddr> = addrs
                .into_iter()
                .filter(|a| allow_private || is_public(a.ip()))
                .collect();
            if usable.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(usable.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1]))
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && (18..20).contains(&o[1]))
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.is_multicast()
                || v6.segments()[0] == 0x2001 && v6.segments()[1] == 0x0db8)
        }
    }
}

/// A transport error without the URL (which may be long, or a credential).
fn describe(e: &reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "connection failed"
    } else if e.is_redirect() {
        "redirect refused"
    } else if e.is_body() || e.is_decode() {
        "bad response body"
    } else if e.is_request() {
        "request failed"
    } else {
        "transport error"
    };
    // The source chain says why (DNS, TLS, refused address).
    let mut cause = String::new();
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        cause = s.to_string();
        src = s.source();
    }
    let cause: String = cause
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    if cause.is_empty() {
        kind.to_string()
    } else {
        format!("{kind}: {cause}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_http_urls() {
        for ok in [
            "https://cap-sources.s3.amazonaws.com/a/rss.xml",
            "http://capcp2.naad-adna.pelmorex.com/x.xml",
            "https://8.8.8.8/x",
        ] {
            assert!(check_url(ok, false).is_ok(), "{ok}");
        }
        for bad in [
            "file:///etc/passwd",
            "ftp://example.com/x",
            "gopher://example.com/",
            "http://localhost/x",
            "http://metadata.internal/",
            "http://intranet/x",
            "http://127.0.0.1:5432/",
            "http://10.0.0.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://100.64.0.1/",
            "http://user:pw@example.com/",
            "not a url",
        ] {
            assert!(check_url(bad, false).is_err(), "{bad}");
        }
        assert!(check_url("http://127.0.0.1:9/x", true).is_ok());
        assert!(check_url("file:///x", true).is_err());
    }

    #[test]
    fn permanent_failures() {
        assert!(
            FetchError::Status {
                status: 404,
                retry_after: None
            }
            .is_permanent()
        );
        assert!(
            !FetchError::Status {
                status: 503,
                retry_after: None
            }
            .is_permanent()
        );
        assert!(!FetchError::Transport("timed out".into()).is_permanent());
        assert!(FetchError::TooLarge(10).is_permanent());
    }
}
