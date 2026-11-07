//! Listings: S3 `ListObjectsV2` on any S3-compatible endpoint, and the HTML
//! directory indexes Apache and nginx generate.

use super::{FetchError, Remote};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use std::borrow::Cow;
use std::collections::HashSet;

/// A listing that needs more pages than this is a bug in the prefix.
pub const MAX_LIST_PAGES: usize = 20;

/// Characters left as they are in a path segment or query value: RFC 3986's
/// unreserved set. Everything else (`@` in OPERA's keys, spaces) is escaped.
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// An S3 bucket on some endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Bucket {
    /// No trailing slash.
    base: String,
    /// Listings go to `{base}/?...` (virtual-hosted) rather than `{base}?...`.
    root: bool,
}

impl S3Bucket {
    /// A public bucket on AWS, addressed virtual-hosted style.
    pub fn aws(bucket: &str) -> Self {
        Self::at(&format!("https://{bucket}.s3.amazonaws.com"))
    }

    /// A bucket at `base`: `https://bucket.host` (virtual-hosted) or
    /// `https://host/bucket` (path style).
    pub fn at(base: &str) -> Self {
        let base = base.trim_end_matches('/').to_string();
        let root = url::Url::parse(&base).is_ok_and(|u| u.path() == "/" || u.path().is_empty());
        Self { base, root }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// The URL of an object, its key escaped segment by segment.
    pub fn url(&self, key: &str) -> String {
        let path: Vec<String> = key
            .split('/')
            .map(|seg| utf8_percent_encode(seg, UNRESERVED).to_string())
            .collect();
        format!("{}/{}", self.base, path.join("/"))
    }

    fn list_url(&self, params: &[(&str, &str)]) -> String {
        let query: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{k}={}", utf8_percent_encode(v, UNRESERVED)))
            .collect();
        let sep = if self.root { "/?" } else { "?" };
        format!("{}{sep}{}", self.base, query.join("&"))
    }
}

/// One object from a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub key: String,
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
}

/// One page of a `ListObjectsV2` answer.
#[derive(Debug, Default, PartialEq)]
pub struct Page {
    pub objects: Vec<Listed>,
    /// Continuation token for the next page, when truncated.
    pub next: Option<String>,
}

/// One entry of an HTML directory index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlEntry {
    /// The file or directory name, unescaped, without a trailing slash.
    pub name: String,
    /// Absolute URL.
    pub url: String,
    pub is_dir: bool,
    /// When the index shows it (UTC as the server prints it).
    pub modified: Option<DateTime<Utc>>,
    /// When the index shows it. Indexes that abbreviate sizes (`1.2M`) give
    /// an approximation.
    pub size: Option<u64>,
}

impl Remote {
    /// Every object whose key starts with `prefix` (a raw key prefix, not
    /// path segments) and sorts after `start_after`, in key order.
    pub async fn list_s3(
        &self,
        lane: &str,
        bucket: &S3Bucket,
        prefix: &str,
        start_after: Option<&str>,
    ) -> Result<Vec<Listed>, FetchError> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let page = self
                .list_page(lane, bucket, prefix, start_after, token.as_deref())
                .await?;
            out.extend(page.objects);
            match page.next {
                Some(t) => token = Some(t),
                None => {
                    out.sort_by(|a, b| a.key.cmp(&b.key));
                    return Ok(out);
                }
            }
        }
        Err(FetchError::object(
            format!("listing {}/{prefix}", bucket.base),
            format!("more than {MAX_LIST_PAGES} pages; the prefix is too broad"),
        ))
    }

    async fn list_page(
        &self,
        lane: &str,
        bucket: &S3Bucket,
        prefix: &str,
        start_after: Option<&str>,
        token: Option<&str>,
    ) -> Result<Page, FetchError> {
        let mut params = vec![("list-type", "2"), ("prefix", prefix)];
        if let Some(s) = start_after {
            params.push(("start-after", s));
        }
        if let Some(t) = token {
            params.push(("continuation-token", t));
        }
        let url = bucket.list_url(&params);
        let what = format!("listing {}/{prefix}", bucket.base);
        let body = self
            .get(&url)
            .lane(lane)
            .listing()
            .max_bytes(32 * 1024 * 1024)
            .bytes()
            .await?
            .ok_or_else(|| FetchError::object(&what, "the bucket does not exist (404)"))?;
        parse_s3_page(&String::from_utf8_lossy(&body)).map_err(|e| FetchError::object(&what, e))
    }

    /// An HTML directory index, or `None` when the page does not exist.
    pub async fn list_html(
        &self,
        lane: &str,
        url: &str,
    ) -> Result<Option<Vec<HtmlEntry>>, FetchError> {
        let body = self
            .get(url)
            .lane(lane)
            .listing()
            .max_bytes(32 * 1024 * 1024)
            .bytes()
            .await?;
        Ok(body.map(|b| parse_html_index(&String::from_utf8_lossy(&b), url)))
    }
}

/// Parse one `ListObjectsV2` answer.
pub fn parse_s3_page(xml: &str) -> Result<Page, String> {
    if let Some(err) = first(xml, "<Error>", "</Error>") {
        let code = first(err, "<Code>", "</Code>").unwrap_or("");
        let message = first(err, "<Message>", "</Message>").unwrap_or("");
        return Err(format!("{} {}", unescape(code), unescape(message)));
    }
    if !xml.contains("<ListBucketResult") {
        return Err("not a ListObjectsV2 answer".to_string());
    }
    let mut page = Page::default();
    for c in blocks(xml, "<Contents>", "</Contents>") {
        let Some(key) = first(c, "<Key>", "</Key>") else {
            continue;
        };
        page.objects.push(Listed {
            key: unescape(key).into_owned(),
            size: first(c, "<Size>", "</Size>")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0),
            modified: first(c, "<LastModified>", "</LastModified>")
                .and_then(|t| DateTime::parse_from_rfc3339(t.trim()).ok())
                .map(|t| t.with_timezone(&Utc)),
        });
    }
    // Contents blocks never hold these, so the first match is the page's.
    let truncated =
        first(xml, "<IsTruncated>", "</IsTruncated>").is_some_and(|t| t.trim() == "true");
    if truncated {
        match first(xml, "<NextContinuationToken>", "</NextContinuationToken>").map(unescape) {
            Some(t) if !t.is_empty() => page.next = Some(t.into_owned()),
            _ => return Err("a truncated page without a continuation token".to_string()),
        }
    }
    Ok(page)
}

/// The text between each `open` and the `close` after it (tags without
/// attributes), in order.
fn blocks<'a>(
    xml: &'a str,
    open: &'static str,
    close: &'static str,
) -> impl Iterator<Item = &'a str> {
    let mut rest = xml;
    std::iter::from_fn(move || {
        let start = rest.find(open)? + open.len();
        let len = rest[start..].find(close)?;
        let inner = &rest[start..start + len];
        rest = &rest[start + len + close.len()..];
        Some(inner)
    })
}

fn first<'a>(xml: &'a str, open: &'static str, close: &'static str) -> Option<&'a str> {
    blocks(xml, open, close).next()
}

/// Undo XML's five named entities and numeric references.
pub fn unescape(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(semi) = after.find(';').filter(|n| *n <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..semi];
        let ch = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Parse an Apache or nginx directory index served at `page_url`. Only links
/// to entries directly inside the page's directory are kept (no parent, no
/// sort links, nothing elsewhere). The time and size printed after each link
/// are read when present.
pub fn parse_html_index(html: &str, page_url: &str) -> Vec<HtmlEntry> {
    let Ok(page) = url::Url::parse(page_url) else {
        return Vec::new();
    };
    // Links are resolved against the directory, also when the page URL lacks
    // its trailing slash.
    let dir = if page.path().ends_with('/') {
        page.clone()
    } else {
        let mut d = page.clone();
        d.set_path(&format!("{}/", page.path()));
        d
    };
    // Lower case for finding tags; byte offsets are the same in both.
    let lower = html.to_ascii_lowercase();
    let starts: Vec<usize> = lower.match_indices("<a ").map(|(i, _)| i).collect();
    let mut out: Vec<HtmlEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (n, &at) in starts.iter().enumerate() {
        let tag_end = match lower[at..].find('>') {
            Some(e) => at + e,
            None => break,
        };
        let Some(href) = attribute(&html[at..tag_end], &lower[at..tag_end], "href") else {
            continue;
        };
        let elsewhere = href.contains("://") && !href.starts_with(dir.as_str());
        if href.starts_with('?') || href.starts_with('#') || elsewhere {
            continue;
        }
        let Ok(resolved) = dir.join(&unescape(href)) else {
            continue;
        };
        let rest = resolved.as_str().strip_prefix(dir.as_str()).unwrap_or("");
        let trimmed = rest.trim_end_matches('/');
        if trimmed.is_empty()
            || trimmed.contains('/')
            || resolved.query().is_some()
            || seen.contains(resolved.as_str())
        {
            continue;
        }
        let name = percent_decode_str(trimmed).decode_utf8_lossy().into_owned();
        let is_dir = rest.ends_with('/');
        // What the index prints after the link, up to the next one.
        let close = lower[tag_end..]
            .find("</a>")
            .map_or(tag_end, |c| tag_end + c + 4);
        let next = starts.get(n + 1).copied().unwrap_or(html.len()).max(close);
        let (modified, size) = details(&strip_tags(&html[close..next]));
        seen.insert(resolved.to_string());
        out.push(HtmlEntry {
            name,
            url: resolved.into(),
            is_dir,
            modified,
            size: if is_dir { None } else { size },
        });
    }
    out
}

/// The value of attribute `name` in `tag`, found in its lower-case copy.
fn attribute<'a>(tag: &'a str, lower: &str, name: &str) -> Option<&'a str> {
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let value_at = tag.len() - rest.len() + 1;
        let value = tag[value_at..].trim_start();
        let quote = value.chars().next()?;
        return if quote == '"' || quote == '\'' {
            let inner = &value[1..];
            inner.find(quote).map(|end| &inner[..end])
        } else {
            Some(
                value
                    .split(|c: char| c.is_whitespace() || c == '>')
                    .next()
                    .unwrap_or(""),
            )
        };
    }
    None
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    unescape(&out).into_owned()
}

/// A date and time (`06-Nov-2026 12:45`, `2026-11-06 12:45:07`) and then a
/// size (`1031234`, `1.2M`, `-`) from the text after a link.
fn details(text: &str) -> (Option<DateTime<Utc>>, Option<u64>) {
    let date = |w: &str| {
        ["%d-%b-%Y", "%Y-%m-%d"]
            .iter()
            .find_map(|f| NaiveDate::parse_from_str(w, f).ok())
    };
    let time = |w: &str| {
        ["%H:%M", "%H:%M:%S"]
            .iter()
            .find_map(|f| NaiveTime::parse_from_str(w, f).ok())
    };
    let mut words = text.split_whitespace();
    let mut prev = words.next();
    while let Some(w) = prev {
        let next = words.next();
        if let Some(t) = date(w).zip(next.and_then(time)).map(|(d, t)| d.and_time(t)) {
            return (Some(t.and_utc()), words.next().and_then(parse_size));
        }
        prev = next;
    }
    (None, None)
}

fn parse_size(word: &str) -> Option<u64> {
    let (number, unit) = match word.char_indices().last()? {
        (i, c) if c.is_ascii_alphabetic() => (&word[..i], c.to_ascii_uppercase()),
        _ => (word, ' '),
    };
    let mult: f64 = match unit {
        ' ' => 1.0,
        'K' => 1024.0,
        'M' => 1024.0 * 1024.0,
        'G' => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let n: f64 = number.parse().ok()?;
    (n.is_finite() && n >= 0.0).then(|| (n * mult).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s3_pages() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Name>noaa-mrms-pds</Name><Prefix>CONUS/x/</Prefix><KeyCount>2</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>true</IsTruncated><NextContinuationToken>1ab&amp;c</NextContinuationToken><Contents><Key>CONUS/x/a&amp;b.grib2.gz</Key><LastModified>2026-11-06T11:51:18.000Z</LastModified><ETag>"e"</ETag><Size>1283991</Size><StorageClass>STANDARD</StorageClass></Contents><Contents><Key>CONUS/x/c</Key><LastModified>2026-11-06T11:53:18.000Z</LastModified><Size>7</Size></Contents><CommonPrefixes><Prefix>CONUS/x/20261106/</Prefix></CommonPrefixes></ListBucketResult>"#;
        let page = parse_s3_page(xml).unwrap();
        assert_eq!(page.objects.len(), 2);
        assert_eq!(page.objects[0].key, "CONUS/x/a&b.grib2.gz");
        assert_eq!(page.objects[0].size, 1283991);
        assert_eq!(
            page.objects[0].modified.unwrap().to_rfc3339(),
            "2026-11-06T11:51:18+00:00"
        );
        assert_eq!(page.next.as_deref(), Some("1ab&c"));
        let last = parse_s3_page(&xml.replace("<IsTruncated>true", "<IsTruncated>false")).unwrap();
        assert_eq!(last.next, None);
        let err = "<?xml version=\"1.0\"?><Error><Code>NoSuchBucket</Code><Message>The specified bucket does not exist</Message></Error>";
        assert!(parse_s3_page(err).unwrap_err().contains("NoSuchBucket"));
        assert!(parse_s3_page("<html>hello</html>").is_err());
        assert!(
            parse_s3_page(&xml.replace(
                "<NextContinuationToken>1ab&amp;c</NextContinuationToken>",
                ""
            ))
            .is_err()
        );
    }

    #[test]
    fn urls_escape_keys_and_queries() {
        let opera = S3Bucket::at("https://s3.waw3-1.cloudferro.com/openradar-24h/");
        assert_eq!(
            opera.url("2026/11/06/OPERA/COMP/OPERA@20261106T1815@0@DBZH.h5"),
            "https://s3.waw3-1.cloudferro.com/openradar-24h/2026/11/06/OPERA/COMP/OPERA%4020261106T1815%400%40DBZH.h5"
        );
        assert_eq!(
            opera.list_url(&[("list-type", "2"), ("prefix", "2026/11/06/")]),
            "https://s3.waw3-1.cloudferro.com/openradar-24h?list-type=2&prefix=2026%2F11%2F06%2F"
        );
        let aws = S3Bucket::aws("noaa-gfs-bdp-pds");
        assert_eq!(
            aws.url("gfs.20261106/06/atmos/x.f024"),
            "https://noaa-gfs-bdp-pds.s3.amazonaws.com/gfs.20261106/06/atmos/x.f024"
        );
        assert_eq!(
            aws.list_url(&[("list-type", "2")]),
            "https://noaa-gfs-bdp-pds.s3.amazonaws.com/?list-type=2"
        );
    }

    #[test]
    fn xml_entities() {
        assert_eq!(
            unescape("a&amp;b&lt;&gt;&quot;&apos;&#65;&#x42;"),
            "a&b<>\"'AB"
        );
        assert_eq!(unescape("fish & chips"), "fish & chips");
        assert_eq!(unescape("&bogus;"), "&bogus;");
    }

    #[test]
    fn nginx_index() {
        let html = r#"<html>
<head><title>Index of /weather/nwp/icon-d2/grib/12/t_2m/</title></head>
<body>
<h1>Index of /weather/nwp/icon-d2/grib/12/t_2m/</h1><hr><pre><a href="../">../</a>
<a href="icon-d2_germany_regular-lat-lon_single-level_2026110612_000_2d_t_2m.grib2.bz2">icon-d2_germany_regular-lat-lon_single-level_2026110612_000_2d_t_2m.grib2..&gt;</a> 06-Nov-2026 12:45             1031234
<a href="icon-d2_germany_regular-lat-lon_single-level_2026110612_001_2d_t_2m.grib2.bz2">icon-d2_germany_regular-lat-lon_single-level_2026110612_001_2d_t_2m.grib2..&gt;</a> 06-Nov-2026 12:46              998877
<a href="sub/">sub/</a>                                               06-Nov-2026 12:40                   -
</pre><hr></body>
</html>"#;
        let page = "https://opendata.dwd.de/weather/nwp/icon-d2/grib/12/t_2m/";
        let entries = parse_html_index(html, page);
        assert_eq!(entries.len(), 3, "{entries:#?}");
        assert_eq!(
            entries[0].name,
            "icon-d2_germany_regular-lat-lon_single-level_2026110612_000_2d_t_2m.grib2.bz2"
        );
        assert_eq!(entries[0].url, format!("{page}{}", entries[0].name));
        assert_eq!(entries[0].size, Some(1031234));
        assert_eq!(
            entries[0].modified.unwrap().to_rfc3339(),
            "2026-11-06T12:45:00+00:00"
        );
        assert!(!entries[0].is_dir);
        assert_eq!(
            (entries[2].name.as_str(), entries[2].is_dir, entries[2].size),
            ("sub", true, None)
        );
        // The same page asked for without its trailing slash.
        assert_eq!(parse_html_index(html, page.trim_end_matches('/')).len(), 3);
    }

    #[test]
    fn apache_index() {
        let html = r#"<table>
<tr><th valign="top"><img src="/icons/blank.gif" alt="[ICO]"></th><th><a href="?C=N;O=D">Name</a></th><th><a href="?C=M;O=A">Last modified</a></th><th><a href="?C=S;O=A">Size</a></th></tr>
<tr><td valign="top"><img src="/icons/back.gif" alt="[PARENTDIR]"></td><td><a href="/data/">Parent Directory</a></td><td>&nbsp;</td><td align="right">  - </td></tr>
<tr><td valign="top"><img src="/icons/unknown.gif" alt="[   ]"></td><td><a href="MRMS%20x.grib2.gz">MRMS x.grib2.gz</a></td><td align="right">2026-11-06 11:51  </td><td align="right">1.2M</td></tr>
<tr><td valign="top"><img src="/icons/folder.gif" alt="[DIR]"></td><td><a href="20261106/">20261106/</a></td><td align="right">2026-11-06 00:00:12  </td><td align="right">  - </td></tr>
<tr><td><a href="https://elsewhere.example/x">elsewhere</a></td></tr>
</table>"#;
        let entries = parse_html_index(html, "https://example.org/data/radar/");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["MRMS x.grib2.gz", "20261106"]);
        assert_eq!(
            entries[0].url,
            "https://example.org/data/radar/MRMS%20x.grib2.gz"
        );
        assert_eq!(entries[0].size, Some(1_258_291));
        assert_eq!(
            entries[1].modified.unwrap().to_rfc3339(),
            "2026-11-06T00:00:12+00:00"
        );
        assert!(entries[1].is_dir);
    }
}
