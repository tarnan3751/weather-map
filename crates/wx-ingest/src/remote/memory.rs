//! An in-memory stand-in for the network: objects by URL, S3 listings over
//! them, byte ranges, validators, canned answers to POSTs, and failures on
//! demand. Family tests run
//! their adapter end to end against it through a real [`Remote`](super::Remote).

use super::list::S3Bucket;
use super::{Request, Response, Transport, TransportError};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use percent_encoding::percent_decode_str;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Debug, Clone)]
struct Object {
    body: Bytes,
    modified: DateTime<Utc>,
    etag: String,
}

#[derive(Debug, Clone)]
struct Fault {
    prefix: String,
    /// `None`: the connection drops.
    status: Option<u16>,
    times: usize,
}

#[derive(Debug, Default)]
struct State {
    objects: BTreeMap<String, Object>,
    /// Answers to POSTs, by URL and request body: status and body.
    posts: BTreeMap<(String, String), (u16, Bytes)>,
    /// Bucket base URLs that answer `ListObjectsV2`, with their page size.
    buckets: Vec<(String, usize)>,
    faults: Vec<Fault>,
    log: Vec<String>,
    versions: u64,
}

/// See the module docs.
#[derive(Debug, Default)]
pub struct MemoryRemote {
    state: Mutex<State>,
}

impl MemoryRemote {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Serve `body` at `url`, modified now.
    pub fn put(&self, url: &str, body: impl Into<Bytes>) {
        self.put_modified(url, body, Utc::now());
    }

    pub fn put_modified(&self, url: &str, body: impl Into<Bytes>, modified: DateTime<Utc>) {
        let mut s = self.lock();
        s.versions += 1;
        let etag = format!("\"v{}\"", s.versions);
        s.objects.insert(
            url.to_string(),
            Object {
                body: body.into(),
                modified,
                etag,
            },
        );
    }

    /// Store an object in an S3 bucket (see [`MemoryRemote::serve_s3`]).
    pub fn put_s3(&self, bucket: &S3Bucket, key: &str, body: impl Into<Bytes>) {
        self.put(&bucket.url(key), body);
    }

    pub fn remove(&self, url: &str) {
        self.lock().objects.remove(url);
    }

    /// Answer a POST of exactly `request` to `url` with `status` and
    /// `answer`. Other POSTs are answered 404. Each is logged as
    /// `POST URL BODY`.
    pub fn put_post(&self, url: &str, request: &str, status: u16, answer: impl Into<Bytes>) {
        self.lock().posts.insert(
            (url.to_string(), request.to_string()),
            (status, answer.into()),
        );
    }

    /// Answer `ListObjectsV2` for `bucket`, at most `page_size` entries a page.
    pub fn serve_s3(&self, bucket: &S3Bucket, page_size: usize) {
        self.lock()
            .buckets
            .push((bucket.base().to_string(), page_size.max(1)));
    }

    /// Answer the next `times` requests for URLs starting with `prefix`
    /// with `status`.
    pub fn fail(&self, prefix: &str, status: u16, times: usize) {
        self.lock().faults.push(Fault {
            prefix: prefix.to_string(),
            status: Some(status),
            times,
        });
    }

    /// Drop the connection of the next `times` requests for URLs starting
    /// with `prefix`.
    pub fn drop_connections(&self, prefix: &str, times: usize) {
        self.lock().faults.push(Fault {
            prefix: prefix.to_string(),
            status: None,
            times,
        });
    }

    /// Every request so far, as `URL` or `URL [range]`, in order.
    pub fn requests(&self) -> Vec<String> {
        self.lock().log.clone()
    }

    pub fn clear_log(&self) {
        self.lock().log.clear();
    }

    /// The requests for URLs starting with `prefix`.
    pub fn requests_for(&self, prefix: &str) -> Vec<String> {
        self.lock()
            .log
            .iter()
            .filter(|r| r.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// The requests other than S3 listings.
    pub fn fetches(&self) -> Vec<String> {
        self.lock()
            .log
            .iter()
            .filter(|r| !r.contains("list-type=2"))
            .cloned()
            .collect()
    }

    /// The S3 listings.
    pub fn listings(&self) -> Vec<String> {
        self.lock()
            .log
            .iter()
            .filter(|r| r.contains("list-type=2"))
            .cloned()
            .collect()
    }

    fn list(&self, s: &State, base: &str, page_size: usize, query: &str) -> Response {
        let params: BTreeMap<String, String> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| {
                (
                    k.to_string(),
                    percent_decode_str(v).decode_utf8_lossy().into_owned(),
                )
            })
            .collect();
        let prefix = params.get("prefix").cloned().unwrap_or_default();
        let after = params
            .get("continuation-token")
            .or(params.get("start-after"))
            .cloned()
            .unwrap_or_default();
        let root = format!("{base}/");
        let mut entries: Vec<(String, &Object)> = Vec::new();
        for (url, obj) in &s.objects {
            let Some(raw) = url.strip_prefix(&root) else {
                continue;
            };
            let key = percent_decode_str(raw).decode_utf8_lossy().into_owned();
            if !key.starts_with(&prefix) {
                continue;
            }
            if key > after {
                entries.push((key, obj));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let truncated = entries.len() > page_size;
        entries.truncate(page_size);
        let esc = |s: &str| {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        let mut xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Prefix>{}</Prefix><IsTruncated>{truncated}</IsTruncated>",
            esc(&prefix)
        );
        if truncated && let Some((last, _)) = entries.last() {
            xml.push_str(&format!(
                "<NextContinuationToken>{}</NextContinuationToken>",
                esc(last)
            ));
        }
        for (key, o) in &entries {
            xml.push_str(&format!(
                "<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size></Contents>",
                esc(key),
                o.modified.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                esc(&o.etag),
                o.body.len()
            ));
        }
        xml.push_str("</ListBucketResult>");
        Response {
            status: 200,
            headers: vec![("content-type".into(), "application/xml".into())],
            body: xml.into(),
        }
    }
}

/// An HTTP date (`Thu, 06 Nov 2026 18:15:00 GMT`).
fn parse_http_date(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc2822(s.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn http_date(t: DateTime<Utc>) -> String {
    t.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

fn status(code: u16) -> Response {
    Response {
        status: code,
        headers: Vec::new(),
        body: Bytes::new(),
    }
}

/// `(start, end exclusive)` of a `bytes=a-b` or `bytes=a-` header.
fn parse_range(v: &str, len: u64) -> Option<(u64, u64)> {
    let (a, b) = v.strip_prefix("bytes=")?.split_once('-')?;
    let start: u64 = a.parse().ok()?;
    let end = if b.is_empty() {
        len
    } else {
        b.parse::<u64>().ok()? + 1
    };
    Some((start, end.min(len)))
}

#[async_trait::async_trait]
impl Transport for MemoryRemote {
    async fn get(&self, req: &Request) -> Result<Response, TransportError> {
        let header = |name: &str| {
            req.headers
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.as_str())
        };
        let mut s = self.lock();
        let entry = match header("range") {
            Some(r) => format!("{} [{r}]", req.url),
            None => req.url.clone(),
        };
        s.log.push(entry);
        if let Some(f) = s
            .faults
            .iter_mut()
            .find(|f| f.times > 0 && req.url.starts_with(&f.prefix))
        {
            f.times -= 1;
            return match f.status {
                Some(code) => Ok(status(code)),
                None => Err(TransportError::Transient("connection reset by peer".into())),
            };
        }
        // An object stored at a URL with a query (an API endpoint) is served
        // as it is; any other query is a listing or nothing.
        if let Some((base, query)) = req.url.split_once('?')
            && !s.objects.contains_key(&req.url)
        {
            let base = base.trim_end_matches('/');
            if query.contains("list-type=2")
                && let Some((_, page)) = s.buckets.iter().find(|(b, _)| b == base)
            {
                let page = *page;
                return Ok(self.list(&s, base, page, query));
            }
            return Ok(status(404));
        }
        let Some(obj) = s.objects.get(&req.url).cloned() else {
            return Ok(status(404));
        };
        drop(s);
        let validators = vec![
            ("etag".to_string(), obj.etag.clone()),
            ("last-modified".to_string(), http_date(obj.modified)),
        ];
        // RFC 9110: with an entity tag to compare, the date is not looked at.
        let unchanged = match header("if-none-match") {
            Some(e) => e == obj.etag,
            None => header("if-modified-since")
                .and_then(parse_http_date)
                .is_some_and(|since| obj.modified.timestamp() <= since.timestamp()),
        };
        if unchanged {
            return Ok(Response {
                status: 304,
                headers: validators,
                body: Bytes::new(),
            });
        }
        let len = obj.body.len() as u64;
        let (code, body) = match header("range") {
            Some(r) => match parse_range(r, len) {
                Some((start, end)) if start < len && start < end => {
                    (206, obj.body.slice(start as usize..end as usize))
                }
                _ => return Ok(status(416)),
            },
            None => (200, obj.body),
        };
        if body.len() as u64 > req.max_bytes {
            return Err(TransportError::TooLarge(req.max_bytes));
        }
        let mut headers = validators;
        headers.push(("content-length".into(), body.len().to_string()));
        Ok(Response {
            status: code,
            headers,
            body,
        })
    }

    async fn post(&self, req: &Request, body: &Bytes) -> Result<Response, TransportError> {
        let request = String::from_utf8_lossy(body).into_owned();
        let mut s = self.lock();
        s.log.push(format!("POST {} {request}", req.url));
        if let Some(f) = s
            .faults
            .iter_mut()
            .find(|f| f.times > 0 && req.url.starts_with(&f.prefix))
        {
            f.times -= 1;
            return match f.status {
                Some(code) => Ok(status(code)),
                None => Err(TransportError::Transient("connection reset by peer".into())),
            };
        }
        let Some((code, answer)) = s.posts.get(&(req.url.clone(), request)).cloned() else {
            return Ok(status(404));
        };
        if answer.len() as u64 > req.max_bytes {
            return Err(TransportError::TooLarge(req.max_bytes));
        }
        Ok(Response {
            status: code,
            headers: vec![("content-type".into(), "application/json".into())],
            body: answer,
        })
    }
}
