//! Fetching from data providers: plain HTTPS GETs with optional byte ranges
//! and conditional requests (and POSTs, for an API that hands out its data
//! behind one), S3-style and HTML directory listings, and the rules every
//! request follows.
//!
//! Every request, whoever makes it, goes through one [`Remote`]:
//!
//! * **Politeness per host.** At most [`HostPolicy::concurrency`] requests to
//!   one host at once, and request starts at least
//!   [`HostPolicy::min_interval`] apart. Defaults suit AWS S3 (built for
//!   heavy use) and are gentler for everyone else; [`Remote::new`] takes
//!   overrides per host.
//! * **Fairness across sources.** On top of the per-host caps, a shared
//!   [`FairPool`] caps requests in flight overall and serves the waiting
//!   tasks in turn (each request names its task as its lane), so one model
//!   run cannot starve the two-minute radar. A request takes its host's slot
//!   before the shared one, so requests queued for a busy host never sit on
//!   shared slots; it then waits out the host's spacing.
//! * **Retries.** Transport failures, 429 and 5xx are retried with backoff
//!   (honouring `Retry-After`); what is left after that is a
//!   [`Failure::Service`]. Anything that asking again cannot fix (a range
//!   past the end, another 4xx, a body over its size limit) is a
//!   [`Failure::Object`]. Missing objects (404, 410) are not errors.
//! * **Size limits.** A body larger than the request's limit is refused
//!   before it is read, or as soon as it outgrows the limit.
//! * **Counters.** Requests and bytes per host, for the status page.
//!
//! The network itself sits behind [`Transport`]: [`http::HttpTransport`] in
//! production, [`memory::MemoryRemote`] in tests.

pub mod codec;
pub mod http;
pub mod list;
#[cfg(test)]
pub mod memory;

use crate::idx::ByteRange;
use crate::limits::FairPool;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::time::Instant;

pub use list::{HtmlEntry, Listed, S3Bucket};

/// Bodies larger than this are refused unless a request raises its limit.
/// Whole small files and ranges of big ones stay far below it.
pub const DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// How a host is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostPolicy {
    /// Requests in flight to the host at once.
    pub concurrency: usize,
    /// Least time between the starts of two requests to the host.
    pub min_interval: Duration,
}

impl HostPolicy {
    /// For hosts not named otherwise: five requests a second, two at once.
    pub const GENTLE: HostPolicy = HostPolicy {
        concurrency: 2,
        min_interval: Duration::from_millis(200),
    };
    /// AWS S3 and other bulk object stores, which are made for this.
    pub const BULK: HostPolicy = HostPolicy {
        concurrency: 6,
        min_interval: Duration::from_millis(20),
    };

    /// The built-in default for `host`.
    pub fn default_for(host: &str) -> HostPolicy {
        if host.ends_with(".amazonaws.com") {
            HostPolicy::BULK
        } else {
            HostPolicy::GENTLE
        }
    }
}

/// What to retry and for how long.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Retries after the first attempt.
    pub retries: u32,
    /// Delay before the first retry; doubled each time.
    pub base: Duration,
    /// Longest single delay, also the cap on `Retry-After`.
    pub max: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        // Retries cover blips; an outage surfaces as a feed error within
        // about a minute, and the supervisor backs off from there.
        Self {
            retries: 3,
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

/// Settings for a [`Remote`].
#[derive(Debug, Clone)]
pub struct RemoteConfig {
    /// Sent as `User-Agent` with every request.
    pub user_agent: String,
    /// Requests in flight at once across all hosts.
    pub max_downloads: usize,
    /// Overrides of [`HostPolicy::default_for`]: a pattern matches a host
    /// equal to it or ending in `.` followed by it.
    pub hosts: Vec<(String, HostPolicy)>,
    pub retry: RetryPolicy,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            user_agent: concat!("weather-map-ingest/", env!("CARGO_PKG_VERSION")).to_string(),
            max_downloads: 8,
            hosts: Vec::new(),
            retry: RetryPolicy::default(),
        }
    }
}

/// Whose fault a failed request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The network or the service is not working (no connection, timeout,
    /// throttling, 5xx) and retrying did not help. Nothing is known to be
    /// wrong with the data; the feed backs off and tries again.
    Service,
    /// The object is not what was asked for: a range past its end, a short
    /// or mismatched response, a body over its size limit, another 4xx, or
    /// bytes that do not parse. Asking again gets the same answer, so the
    /// data (or its index) is what is broken.
    Object,
}

/// A request or its result that failed, with its cause classified.
#[derive(Debug)]
pub struct FetchError {
    pub failure: Failure,
    what: String,
    message: String,
    /// The HTTP status and the start of the body, when the server answered
    /// and the answer is what failed.
    answer: Option<(u16, String)>,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.what, self.message)
    }
}

impl std::error::Error for FetchError {}

impl FetchError {
    /// The data at `what` is broken: see [`Failure::Object`].
    pub fn object(what: impl Into<String>, message: impl std::fmt::Display) -> Self {
        Self {
            failure: Failure::Object,
            what: what.into(),
            message: message.to_string(),
            answer: None,
        }
    }

    /// The service behind `what` is not working: see [`Failure::Service`].
    pub fn service(what: impl Into<String>, message: impl std::fmt::Display) -> Self {
        Self {
            failure: Failure::Service,
            what: what.into(),
            message: message.to_string(),
            answer: None,
        }
    }

    /// The HTTP status of the answer that failed the request (a 4xx, or a
    /// 429 or 5xx left after retrying) and the first 200 bytes of its body,
    /// for an API that says what went wrong there. `None` when there was
    /// no answer (a transport failure) or the failure is the client's own
    /// (a size limit, a short range).
    pub fn answer(&self) -> Option<(u16, &str)> {
        self.answer.as_ref().map(|(s, b)| (*s, b.as_str()))
    }

    fn with_answer(mut self, status: u16, body: &[u8]) -> Self {
        let text = String::from_utf8_lossy(&body[..body.len().min(200)])
            .trim()
            .to_string();
        self.answer = Some((status, text));
        self
    }
}

/// One GET (or POST, see [`Transport::post`]) as the transport sees it.
#[derive(Debug, Clone)]
pub struct Request {
    pub url: String,
    /// Lower-case header names.
    pub headers: Vec<(&'static str, String)>,
    /// The transport refuses a body larger than this, without reading it
    /// all when it can.
    pub max_bytes: u64,
}

/// What came back.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    /// Lower-case header names.
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// No connection, a timeout, a reset: worth retrying.
    Transient(String),
    /// The body is larger than the request allows.
    TooLarge(u64),
    /// The request cannot be made at all (a malformed URL).
    Invalid(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Transient(m) => write!(f, "{m}"),
            TransportError::TooLarge(limit) => write!(f, "the body is larger than {limit} bytes"),
            TransportError::Invalid(m) => write!(f, "invalid request: {m}"),
        }
    }
}

/// Sends GETs. See [`http::HttpTransport`] and [`memory::MemoryRemote`].
#[async_trait::async_trait]
pub trait Transport: Send + Sync + 'static {
    async fn get(&self, req: &Request) -> Result<Response, TransportError>;

    /// A POST of `body`, its `content-type` among `req.headers`. Only APIs
    /// that hand out their data behind one need it (DPC's download links).
    /// Transports that only GET refuse it.
    async fn post(&self, req: &Request, body: &Bytes) -> Result<Response, TransportError> {
        let _ = body;
        Err(TransportError::Invalid(format!(
            "this transport cannot POST to {}",
            req.url
        )))
    }
}

/// Requests and bytes for one host since the subsystem started.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Traffic {
    pub host: String,
    /// Listing requests (S3 pages, HTML indexes).
    pub list_requests: u64,
    /// Other GETs (whole objects, ranges, inventories).
    pub get_requests: u64,
    /// Requests answered 404 or 410: objects that do not exist (yet).
    pub not_found: u64,
    /// Attempts repeated after a transient failure.
    pub retries: u64,
    /// Body bytes received.
    pub bytes: u64,
}

#[derive(Debug, Default)]
struct Counters {
    list: AtomicU64,
    get: AtomicU64,
    missing: AtomicU64,
    retries: AtomicU64,
    bytes: AtomicU64,
}

struct Host {
    name: String,
    policy: HostPolicy,
    slots: Semaphore,
    /// When the next request may start.
    next_start: Mutex<Instant>,
    counters: Counters,
}

struct Inner {
    transport: Arc<dyn Transport>,
    config: RemoteConfig,
    downloads: FairPool,
    hosts: Mutex<HashMap<String, Arc<Host>>>,
}

/// Validators from a response, for asking next time whether it changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validators {
    pub etag: Option<String>,
    /// `Last-Modified` exactly as sent.
    pub last_modified: Option<String>,
}

/// A successful GET.
#[derive(Debug, Clone)]
pub struct Reply {
    /// Empty when `not_modified`.
    pub body: Bytes,
    pub validators: Validators,
    /// The conditional request was answered 304: what was had is current.
    pub not_modified: bool,
}

/// The one way out to the network. Cheap to clone.
#[derive(Clone)]
pub struct Remote {
    inner: Arc<Inner>,
}

impl Remote {
    /// A remote over the real network. Builds a client only: no I/O.
    pub fn new(config: RemoteConfig) -> anyhow::Result<Self> {
        let transport = http::HttpTransport::new(&config.user_agent)?;
        Ok(Self::with_transport(Arc::new(transport), config))
    }

    /// A remote over any transport (tests use [`memory::MemoryRemote`]).
    pub fn with_transport(transport: Arc<dyn Transport>, config: RemoteConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                transport,
                downloads: FairPool::new(config.max_downloads),
                config,
                hosts: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Start a GET of `url`.
    pub fn get(&self, url: impl Into<String>) -> Get<'_> {
        Get {
            remote: self,
            url: url.into(),
            lane: "ingest".into(),
            range: None,
            validators: None,
            max_bytes: DEFAULT_MAX_BYTES,
            listing: false,
            body: None,
        }
    }

    /// Start a POST of `body` as `content_type` to `url`. It follows the
    /// same rules as a GET (politeness, fairness, retries of 429 and 5xx,
    /// size limits, counted as a GET), so it suits only requests that may
    /// be repeated.
    pub fn post(
        &self,
        url: impl Into<String>,
        content_type: &str,
        body: impl Into<Bytes>,
    ) -> Get<'_> {
        Get {
            body: Some((content_type.to_string(), body.into())),
            ..self.get(url)
        }
    }

    /// Requests and bytes per host, busiest first.
    pub fn traffic(&self) -> Vec<Traffic> {
        let hosts = self.inner.hosts.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<Traffic> = hosts
            .values()
            .map(|h| Traffic {
                host: h.name.clone(),
                list_requests: h.counters.list.load(Ordering::Relaxed),
                get_requests: h.counters.get.load(Ordering::Relaxed),
                not_found: h.counters.missing.load(Ordering::Relaxed),
                retries: h.counters.retries.load(Ordering::Relaxed),
                bytes: h.counters.bytes.load(Ordering::Relaxed),
            })
            .collect();
        out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.host.cmp(&b.host)));
        out
    }

    /// The policy that applies to `host`.
    pub fn policy(&self, host: &str) -> HostPolicy {
        self.inner
            .config
            .hosts
            .iter()
            .find(|(pattern, _)| {
                host == pattern
                    || host
                        .strip_suffix(pattern.as_str())
                        .is_some_and(|rest| rest.ends_with('.'))
            })
            .map_or_else(|| HostPolicy::default_for(host), |(_, p)| *p)
    }

    fn host(&self, name: &str) -> Arc<Host> {
        let mut hosts = self.inner.hosts.lock().unwrap_or_else(|e| e.into_inner());
        hosts
            .entry(name.to_string())
            .or_insert_with(|| {
                let policy = self.policy(name);
                Arc::new(Host {
                    name: name.to_string(),
                    policy,
                    slots: Semaphore::new(policy.concurrency.max(1)),
                    next_start: Mutex::new(Instant::now()),
                    counters: Counters::default(),
                })
            })
            .clone()
    }

    async fn send(&self, get: Get<'_>) -> Result<Option<Reply>, FetchError> {
        let what = get.describe();
        let host_name =
            host_of(&get.url).ok_or_else(|| FetchError::object(&what, "not an http(s) URL"))?;
        let host = self.host(&host_name);
        let mut headers = Vec::new();
        if let Some(r) = get.range {
            match r.end {
                Some(end) if end > r.start => {
                    headers.push(("range", format!("bytes={}-{}", r.start, end - 1)))
                }
                Some(_) => return Err(FetchError::object(&what, "empty byte range")),
                None => headers.push(("range", format!("bytes={}-", r.start))),
            }
        }
        if let Some(v) = &get.validators {
            if let Some(etag) = &v.etag {
                headers.push(("if-none-match", etag.clone()));
            }
            if let Some(lm) = &v.last_modified {
                headers.push(("if-modified-since", lm.clone()));
            }
        }
        if let Some((content_type, _)) = &get.body {
            headers.push(("content-type", content_type.clone()));
        }
        let req = Request {
            url: get.url.clone(),
            headers,
            max_bytes: get.max_bytes,
        };
        let retry = self.inner.config.retry;
        let mut attempt = 0u32;
        loop {
            let result = {
                // Host slot first, then a shared one: see the module docs.
                // The spacing is taken last, so starts are spaced as sent.
                let _host_slot = host
                    .slots
                    .acquire()
                    .await
                    .map_err(|e| FetchError::service(&what, e))?;
                let _shared = self.inner.downloads.acquire(&get.lane).await;
                let start = {
                    let mut next = host.next_start.lock().unwrap_or_else(|e| e.into_inner());
                    let start = (*next).max(Instant::now());
                    *next = start + host.policy.min_interval;
                    start
                };
                if start > Instant::now() {
                    tokio::time::sleep_until(start).await;
                }
                let counter = if get.listing {
                    &host.counters.list
                } else {
                    &host.counters.get
                };
                counter.fetch_add(1, Ordering::Relaxed);
                match &get.body {
                    Some((_, body)) => self.inner.transport.post(&req, body).await,
                    None => self.inner.transport.get(&req).await,
                }
            };
            let retry_after = match result {
                Ok(resp) => match resp.status {
                    200 | 206 => {
                        host.counters
                            .bytes
                            .fetch_add(resp.body.len() as u64, Ordering::Relaxed);
                        return finish(&what, get.range, resp).map(Some);
                    }
                    304 => {
                        return Ok(Some(Reply {
                            body: Bytes::new(),
                            validators: validators(&resp),
                            not_modified: true,
                        }));
                    }
                    404 | 410 => {
                        host.counters.missing.fetch_add(1, Ordering::Relaxed);
                        return Ok(None);
                    }
                    429 | 500..=599 => {
                        let status = resp.status;
                        let wait = resp
                            .header("retry-after")
                            .and_then(|v| v.trim().parse::<u64>().ok());
                        if attempt >= retry.retries {
                            return Err(FetchError::service(
                                &what,
                                format!("HTTP {status} after {} attempts", attempt + 1),
                            )
                            .with_answer(status, &resp.body));
                        }
                        wait.map(Duration::from_secs)
                    }
                    status => {
                        let detail =
                            String::from_utf8_lossy(&resp.body[..resp.body.len().min(200)])
                                .into_owned();
                        return Err(FetchError::object(
                            &what,
                            format!("HTTP {status} {}", detail.trim()),
                        )
                        .with_answer(status, &resp.body));
                    }
                },
                Err(TransportError::TooLarge(limit)) => {
                    return Err(FetchError::object(
                        &what,
                        format!("the body is larger than the {limit} bytes allowed"),
                    ));
                }
                Err(TransportError::Invalid(m)) => return Err(FetchError::object(&what, m)),
                Err(TransportError::Transient(m)) => {
                    if attempt >= retry.retries {
                        return Err(FetchError::service(
                            &what,
                            format!("{m} (after {} attempts)", attempt + 1),
                        ));
                    }
                    None
                }
            };
            let backoff = retry.base.saturating_mul(1 << attempt.min(16));
            let delay = retry_after.unwrap_or(backoff).min(retry.max);
            host.counters.retries.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(request = %what, attempt, ?delay, "retrying");
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }
}

/// A GET being set up. See [`Remote::get`].
pub struct Get<'a> {
    remote: &'a Remote,
    url: String,
    lane: String,
    range: Option<ByteRange>,
    validators: Option<Validators>,
    max_bytes: u64,
    listing: bool,
    /// For a POST: the content type and the body.
    body: Option<(String, Bytes)>,
}

impl Get<'_> {
    /// Queue with the other requests of this task (see [`FairPool`]).
    pub fn lane(mut self, lane: &str) -> Self {
        self.lane = lane.to_string();
        self
    }

    /// Only these bytes. The reply holds exactly them, or the request fails
    /// as [`Failure::Object`].
    pub fn range(mut self, range: ByteRange) -> Self {
        self.range = Some(range);
        self
    }

    /// Ask only for a newer version than the one these came with: an
    /// unchanged object is answered with [`Reply::not_modified`].
    pub fn if_changed(mut self, since: &Validators) -> Self {
        self.validators = Some(since.clone());
        self
    }

    /// Refuse bodies larger than this (default [`DEFAULT_MAX_BYTES`]).
    pub fn max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = max;
        self
    }

    /// Count the request as a listing.
    pub fn listing(mut self) -> Self {
        self.listing = true;
        self
    }

    /// The reply, or `None` when the object does not exist.
    pub async fn send(self) -> Result<Option<Reply>, FetchError> {
        self.remote.send(self).await
    }

    /// The body, or `None` when the object does not exist.
    pub async fn bytes(self) -> Result<Option<Bytes>, FetchError> {
        Ok(self.send().await?.map(|r| r.body))
    }

    fn describe(&self) -> String {
        let method = if self.body.is_some() { "POST" } else { "GET" };
        match self.range {
            Some(r) => format!(
                "{method} {} bytes {}..{}",
                self.url,
                r.start,
                r.end.map_or(String::new(), |e| e.to_string())
            ),
            None => format!("{method} {}", self.url),
        }
    }
}

/// Check a 200 or 206 against what was asked for.
fn finish(what: &str, range: Option<ByteRange>, resp: Response) -> Result<Reply, FetchError> {
    let validators = validators(&resp);
    let body = match (range, resp.status) {
        (Some(r), 206) => {
            if let Some(want) = r.size()
                && resp.body.len() as u64 != want
            {
                return Err(FetchError::object(
                    what,
                    format!("asked for {want} bytes, got {}", resp.body.len()),
                ));
            }
            resp.body
        }
        // The server ignored the range and sent everything: cut it here.
        (Some(r), _) => {
            let len = resp.body.len() as u64;
            let end = r.end.unwrap_or(len);
            if r.start >= len || end > len {
                return Err(FetchError::object(
                    what,
                    format!("the range ends past the object's {len} bytes"),
                ));
            }
            resp.body.slice(r.start as usize..end as usize)
        }
        (None, _) => resp.body,
    };
    Ok(Reply {
        body,
        validators,
        not_modified: false,
    })
}

fn validators(resp: &Response) -> Validators {
    Validators {
        etag: resp.header("etag").map(str::to_string),
        last_modified: resp.header("last-modified").map(str::to_string),
    }
}

/// `host[:port]` of an http(s) URL.
fn host_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let host = u.host_str()?.to_ascii_lowercase();
    Some(match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    })
}

#[cfg(test)]
mod tests;
