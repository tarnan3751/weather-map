//! A stand-in for the Datamart in tests: files published at URLs (kept in a
//! [`MemoryRemote`], which serves them), and Apache indexes of every
//! directory above them made the way the real server makes them: an entry's
//! time is its last change to the minute (a directory's, its newest file's),
//! sizes are abbreviated (`598K`, `1.2M`), and a `P` pattern keeps only the
//! files it matches. Every request is logged.

use crate::IngestConfig;
use crate::engine::Engine;
use crate::remote::memory::MemoryRemote;
use crate::remote::{
    HostPolicy, Remote, RemoteConfig, Request, Response, RetryPolicy, Transport, TransportError,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use object_store::memory::InMemory;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wx_core::{FrameCatalog, Hub, LAYERS, SOURCES};

#[derive(Debug, Default)]
pub struct FakeDatamart {
    mem: Arc<MemoryRemote>,
    /// URL to size and time of publication.
    files: Mutex<BTreeMap<String, (u64, DateTime<Utc>)>>,
    log: Mutex<Vec<String>>,
}

impl FakeDatamart {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            mem: MemoryRemote::new(),
            ..Self::default()
        })
    }

    pub fn publish(&self, url: &str, body: impl Into<Bytes>, at: DateTime<Utc>) {
        let body: Bytes = body.into();
        self.files
            .lock()
            .unwrap()
            .insert(url.to_string(), (body.len() as u64, at));
        self.mem.put_modified(url, body, at);
    }

    /// Every request so far, in order.
    pub fn requests(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub fn clear_log(&self) {
        self.log.lock().unwrap().clear();
    }

    /// A remote over this Datamart that does not wait between requests.
    pub fn remote(self: &Arc<Self>) -> Remote {
        let fast = HostPolicy {
            concurrency: 8,
            min_interval: Duration::ZERO,
        };
        Remote::with_transport(
            self.clone(),
            RemoteConfig {
                hosts: vec![("weather.gc.ca".into(), fast), ("example.org".into(), fast)],
                retry: RetryPolicy {
                    retries: 1,
                    base: Duration::from_millis(1),
                    max: Duration::from_millis(5),
                },
                ..RemoteConfig::default()
            },
        )
    }

    /// An engine over the real registries reading from this Datamart, with
    /// a fresh store and catalog.
    pub fn engine(self: &Arc<Self>) -> (Arc<Engine>, Arc<InMemory>) {
        let store = Arc::new(InMemory::new());
        let catalog = Arc::new(FrameCatalog::with_registry(Hub::new(), LAYERS, SOURCES));
        let engine = Engine::new(
            store.clone(),
            catalog,
            IngestConfig::default(),
            self.remote(),
            LAYERS,
        );
        (Arc::new(engine), store)
    }

    /// The index of directory `dir` (ending in `/`), or `None` when nothing
    /// is under it.
    fn index(&self, dir: &str, pattern: Option<&str>) -> Option<String> {
        let files = self.files.lock().unwrap();
        // Name to (is a directory, size, newest time).
        let mut entries: BTreeMap<String, (bool, u64, DateTime<Utc>)> = BTreeMap::new();
        for (url, &(size, at)) in files.range(dir.to_string()..) {
            let Some(rest) = url.strip_prefix(dir) else {
                break;
            };
            match rest.split_once('/') {
                Some((sub, _)) => {
                    let e = entries.entry(format!("{sub}/")).or_insert((true, 0, at));
                    e.2 = e.2.max(at);
                }
                None => {
                    if pattern.is_none_or(|p| glob(p, rest)) {
                        entries.insert(rest.to_string(), (false, size, at));
                    }
                }
            }
        }
        if !files.keys().any(|u| u.starts_with(dir)) {
            return None;
        }
        let path = url::Url::parse(dir)
            .map(|u| u.path().to_string())
            .unwrap_or_default();
        let mut html = format!(
            "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 3.2 Final//EN\">\n<html>\n <head>\n  <title>Index of {path}</title>\n </head>\n <body>\n<h1>Index of {path}</h1>\n<pre><img src=\"/icons/blank.gif\" alt=\"Icon \"> <a href=\"?C=N;O=D\">Name</a>                    <a href=\"?C=M;O=A\">Last modified</a>      <a href=\"?C=S;O=A\">Size</a>  <a href=\"?C=D;O=A\">Description</a><hr><img src=\"/icons/back.gif\" alt=\"[PARENTDIR]\"> <a href=\"/\">Parent Directory</a>                             -   \n"
        );
        for (name, (is_dir, size, at)) in entries {
            let (icon, shown) = if is_dir {
                (
                    "<img src=\"/icons/folder.gif\" alt=\"[DIR]\">",
                    "-".to_string(),
                )
            } else {
                (
                    "<img src=\"/icons/unknown.gif\" alt=\"[   ]\">",
                    apache_size(size),
                )
            };
            html.push_str(&format!(
                "{icon} <a href=\"{name}\">{name}</a>  {}  {shown:>4}  \n",
                at.format("%Y-%m-%d %H:%M")
            ));
        }
        html.push_str("<hr></pre>\n</body></html>\n");
        Some(html)
    }
}

/// Sizes as Apache abbreviates them.
fn apache_size(n: u64) -> String {
    let n = n as f64;
    let (k, m) = (1024.0, 1024.0 * 1024.0);
    if n < k {
        format!("{n}")
    } else if n < m {
        let v = n / k;
        if v < 10.0 {
            format!("{v:.1}K")
        } else {
            format!("{v:.0}K")
        }
    } else {
        let v = n / m;
        if v < 10.0 {
            format!("{v:.1}M")
        } else {
            format!("{v:.0}M")
        }
    }
}

/// Shell-style matching as Apache's `P` does it: `*`, `?` and `[...]`.
pub fn glob(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => (0..=s.len()).any(|k| go(&p[1..], &s[k..])),
            Some('?') => !s.is_empty() && go(&p[1..], &s[1..]),
            Some('[') => {
                let Some(close) = p.iter().position(|&c| c == ']') else {
                    return false;
                };
                !s.is_empty() && p[1..close].contains(&s[0]) && go(&p[close + 1..], &s[1..])
            }
            Some(c) => s.first() == Some(c) && go(&p[1..], &s[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = name.chars().collect();
    go(&p, &s)
}

#[async_trait::async_trait]
impl Transport for FakeDatamart {
    async fn get(&self, req: &Request) -> Result<Response, TransportError> {
        self.log.lock().unwrap().push(req.url.clone());
        let (path, query) = req.url.split_once('?').unwrap_or((&req.url, ""));
        if path.ends_with('/') {
            let pattern = query
                .strip_prefix("P=")
                .map(|p| p.replace("%5B", "[").replace("%5D", "]"));
            return Ok(match self.index(path, pattern.as_deref()) {
                Some(html) => Response {
                    status: 200,
                    headers: vec![("content-type".into(), "text/html".into())],
                    body: html.into(),
                },
                None => Response {
                    status: 404,
                    headers: Vec::new(),
                    body: Bytes::new(),
                },
            });
        }
        self.mem.get(req).await
    }
}

#[test]
fn globs() {
    assert!(glob("a*_[AS][Gf][Lc]*", "a_x_AGL-2m_y"));
    assert!(glob("a*_[AS][Gf][Lc]*", "a_x_Sfc_y"));
    assert!(!glob("a*_[AS][Gf][Lc]*", "a_x_IsbL-0500_y"));
    assert!(glob("x?z", "xyz") && !glob("x?z", "xz"));
    assert_eq!(apache_size(181), "181");
    assert_eq!(apache_size(5222), "5.1K");
    assert_eq!(apache_size(612_591), "598K");
    assert_eq!(apache_size(1_263_899), "1.2M");
}
