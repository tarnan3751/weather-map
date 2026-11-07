//! Reading the feeds that list CAP messages: RSS 2.0, RSS 1.0 and Atom,
//! including MeteoAlarm's Atom (one entry per region, with CAP fields in
//! each entry) and NAADS's (status and type as categories). A feed may also
//! be a single CAP message.

use super::cap;
use super::text::token;
use super::xml::{self, Element};
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use url::Url;

/// Items read from one feed; the rest are ignored.
const MAX_ITEMS: usize = 3000;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Item {
    /// Absolute http(s) URL of the CAP document.
    pub link: String,
    pub published: Option<DateTime<Utc>>,
    /// When the feed itself says the alert expires (MeteoAlarm).
    pub expires: Option<DateTime<Utc>>,
    /// The feed says the message is not `Actual` (a test or exercise).
    pub not_actual: bool,
}

#[derive(Debug)]
pub(crate) enum Listing {
    Items(Vec<Item>),
    /// The feed URL serves one CAP message itself.
    Message(Box<cap::Message>),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum IndexError {
    #[error(transparent)]
    Xml(#[from] xml::XmlError),
    #[error(transparent)]
    Cap(#[from] cap::CapError),
    #[error("not a feed (root element <{0}>)")]
    Unknown(String),
}

pub(crate) fn parse(body: &[u8], feed_url: &Url) -> Result<Listing, IndexError> {
    let root = xml::parse(body)?;
    match root.name.to_ascii_lowercase().as_str() {
        "rss" => Ok(Listing::Items(collect(
            root.children_named("channel")
                .flat_map(|c| c.children_named("item")),
            feed_url,
            rss_item,
        ))),
        "rdf" => Ok(Listing::Items(collect(
            root.children_named("item"),
            feed_url,
            rss_item,
        ))),
        "feed" => Ok(Listing::Items(collect(
            root.children_named("entry"),
            feed_url,
            atom_entry,
        ))),
        "alert" => Ok(Listing::Message(Box::new(cap::parse_element(&root)?))),
        other => Err(IndexError::Unknown(other.chars().take(40).collect())),
    }
}

fn collect<'a>(
    items: impl Iterator<Item = &'a Element>,
    feed_url: &Url,
    read: fn(&Element, &Url) -> Option<Item>,
) -> Vec<Item> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for el in items.take(MAX_ITEMS) {
        let Some(item) = read(el, feed_url) else {
            continue;
        };
        // A link back to the feed is a placeholder ("no active alerts").
        if same_document(&item.link, feed_url) {
            continue;
        }
        // MeteoAlarm lists a message once per region it covers.
        if seen.insert(item.link.clone()) {
            out.push(item);
        }
    }
    out
}

fn rss_item(el: &Element, base: &Url) -> Option<Item> {
    let link = el
        .child_text("link")
        .and_then(|l| absolute(l, base))
        .or_else(|| {
            el.children_named("enclosure")
                .filter_map(|e| e.attr("url"))
                .find_map(|l| absolute(l, base))
        })
        .or_else(|| {
            el.child_text("guid")
                .and_then(|g| absolute(g, base).filter(|_| g.contains("://")))
        })?;
    let published = el
        .child_text("pubDate")
        .and_then(|d| {
            DateTime::parse_from_rfc2822(d.trim())
                .ok()
                .map(|t| t.with_timezone(&Utc))
        })
        .or_else(|| el.child_text("date").and_then(cap::time));
    Some(Item {
        link,
        published,
        expires: None,
        not_actual: false,
    })
}

fn atom_entry(el: &Element, base: &Url) -> Option<Item> {
    let links: Vec<&Element> = el.children_named("link").collect();
    let is_cap = |l: &Element| {
        l.attr("type")
            .is_some_and(|t| t.to_ascii_lowercase().contains("cap"))
    };
    let rel = |l: &Element| l.attr("rel").map(str::to_ascii_lowercase);
    let pick = |want: &dyn Fn(&Element) -> bool| {
        links
            .iter()
            .copied()
            .filter(|l| want(l))
            .find_map(|l| l.attr("href"))
    };
    let link = pick(&is_cap)
        .or_else(|| pick(&|l| rel(l).as_deref() == Some("alternate")))
        .or_else(|| pick(&|l| rel(l).is_none() && l.attr("title").is_none()))
        .or_else(|| pick(&|l| rel(l).as_deref() == Some("related")))
        .and_then(|l| absolute(l, base))?;
    let published = el
        .child_text("published")
        .or_else(|| el.child_text("updated"))
        .and_then(cap::time);
    let expires = el.child_text("expires").and_then(cap::time);
    let mut not_actual = el
        .child_text("status")
        .is_some_and(|s| !s.eq_ignore_ascii_case("actual"));
    for cat in el.children_named("category") {
        if let Some(term) = cat.attr("term")
            && let Some(status) = term.strip_prefix("status=")
            && !status.eq_ignore_ascii_case("actual")
        {
            not_actual = true;
        }
    }
    Some(Item {
        link,
        published,
        expires,
        not_actual,
    })
}

/// `raw` resolved against the feed's URL, if it is an http(s) URL. The
/// fragment is dropped: it never selects a different document.
fn absolute(raw: &str, base: &Url) -> Option<String> {
    let raw = token(raw, 2048)?;
    let mut url = base.join(&raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    url.set_fragment(None);
    Some(url.to_string())
}

fn same_document(link: &str, feed: &Url) -> bool {
    let strip = |s: &str| {
        let s = s.split_once("://").map_or(s, |(_, r)| r);
        s.trim_end_matches('/').to_ascii_lowercase()
    };
    strip(link) == strip(feed.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn rss_items() {
        let body = br#"<?xml version="1.0"?><rss version="2.0"><channel><title>x</title>
            <item><title>A</title><link>https://cap-sources.s3.amazonaws.com/bz-nms-en/2026-10-19-17-53-43.xml</link>
              <guid>urn:oid:2.49.0.1.84.0.2026.10.19.17.53.43</guid><pubDate>Mon, 19 Oct 2026 17:53:43 +0000</pubDate></item>
            <item><title>relative</title><link>cap/2.xml</link></item>
            <item><title>guid only</title><guid isPermaLink="false">https://www.dwd.de/x.xml</guid></item>
            <item><title>hostile</title><link>javascript:alert(1)</link></item>
            <item><title>file</title><link>file:///etc/passwd</link></item>
            <item><title>no link</title></item>
          </channel></rss>"#;
        let Listing::Items(items) = parse(
            body,
            &url("https://cap-sources.s3.amazonaws.com/bz-nms-en/rss.xml"),
        )
        .unwrap() else {
            panic!()
        };
        let links: Vec<&str> = items.iter().map(|i| i.link.as_str()).collect();
        assert_eq!(
            links,
            [
                "https://cap-sources.s3.amazonaws.com/bz-nms-en/2026-10-19-17-53-43.xml",
                "https://cap-sources.s3.amazonaws.com/bz-nms-en/cap/2.xml",
                "https://www.dwd.de/x.xml",
            ]
        );
        assert_eq!(
            items[0].published.unwrap().to_rfc3339(),
            "2026-10-19T17:53:43+00:00"
        );
    }

    #[test]
    fn meteoalarm_entries_are_one_per_message() {
        let body = br#"<feed xmlns="http://www.w3.org/2005/Atom" xmlns:cap="urn:oasis:names:tc:emergency:cap:1.2">
          <entry><cap:geocode><valueName>NUTS3</valueName><value>HR033</value></cap:geocode>
            <link title="Zadarska" href="https://meteoalarm.org?geocode=EMMA_ID:HR023" hreflang="en"/>
            <cap:expires>2026-11-07T22:59:59+00:00</cap:expires><cap:status>Actual</cap:status>
            <link type="application/cap+xml" href="https://feeds.meteoalarm.org/api/v1/warnings/feeds-croatia/e8f6"/>
            <link title="Croatia" rel="related" href="https://meteoalarm.org?region=HR" hreflang="en"/>
            <published>2026-11-06T18:12:31Z</published></entry>
          <entry><link type="application/cap+xml" href="https://feeds.meteoalarm.org/api/v1/warnings/feeds-croatia/e8f6"/></entry>
          <entry><cap:status>Exercise</cap:status><link type="application/cap+xml" href="https://feeds.meteoalarm.org/x/2"/></entry>
        </feed>"#;
        let Listing::Items(items) = parse(
            body,
            &url("https://feeds.meteoalarm.org/feeds/meteoalarm-legacy-atom-croatia"),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        assert_eq!(
            items[0].link,
            "https://feeds.meteoalarm.org/api/v1/warnings/feeds-croatia/e8f6"
        );
        assert_eq!(
            items[0].expires.unwrap().to_rfc3339(),
            "2026-11-07T22:59:59+00:00"
        );
        assert!(!items[0].not_actual);
        assert!(items[1].not_actual);
    }

    #[test]
    fn naads_categories_and_placeholders() {
        let body = br#"<feed xmlns="http://www.w3.org/2005/Atom">
          <entry><link href="http://capcp2.naad-adna.pelmorex.com/2026-11-06/a.xml" rel="alternate"/>
            <category term="status=Test"/><updated>2026-11-06T18:00:53-00:00</updated></entry>
          <entry><link href="http://capcp2.naad-adna.pelmorex.com/2026-11-06/b.xml" rel="alternate"/>
            <category term="status=Actual"/></entry>
          <entry><title>There are no active watches, warnings or advisories</title>
            <link href="https://alert.metservice.gov.jm/capfeed.php"/></entry>
        </feed>"#;
        let Listing::Items(items) =
            parse(body, &url("https://alert.metservice.gov.jm/capfeed.php")).unwrap()
        else {
            panic!()
        };
        assert_eq!(items.len(), 2, "the link back to the feed is not an alert");
        assert!(items[0].not_actual);
        assert!(!items[1].not_actual);
    }

    #[test]
    fn a_feed_that_is_a_message_and_one_that_is_neither() {
        let cap = br#"<alert xmlns="urn:oasis:names:tc:emergency:cap:1.2"><identifier>i</identifier>
            <sender>s</sender><sent>2026-11-06T10:00:00Z</sent><status>Actual</status><msgType>Alert</msgType></alert>"#;
        assert!(matches!(
            parse(cap, &url("https://a.example/cap")).unwrap(),
            Listing::Message(_)
        ));
        assert!(matches!(
            parse(b"<html><body/></html>", &url("https://a.example/")),
            Err(IndexError::Unknown(_))
        ));
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/world/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn real_feeds() {
        let Listing::Items(items) = parse(
            &fixture("feed_s3_rss.xml"),
            &url("https://cap-sources.s3.amazonaws.com/bz-nms-en/rss.xml"),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(items.len(), 24);
        assert!(items.iter().all(|i| {
            i.link
                .starts_with("https://cap-sources.s3.amazonaws.com/bz-nms-en/")
                && i.published.is_some()
        }));

        let feed = url("https://feeds.meteoalarm.org/feeds/meteoalarm-legacy-atom-croatia");
        let Listing::Items(items) = parse(&fixture("feed_meteoalarm_atom.xml"), &feed).unwrap()
        else {
            panic!()
        };
        assert_eq!(items.len(), 3, "six entries, three messages");
        assert!(items.iter().all(|i| {
            i.link
                .starts_with("https://feeds.meteoalarm.org/api/v1/warnings/feeds-croatia/")
                && i.expires.is_some()
        }));

        let Listing::Items(items) = parse(
            &fixture("feed_naads_atom.xml"),
            &url("https://rss.naad-adna.pelmorex.com/"),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(items.len(), 5, "an English and a French entry per message");
        assert_eq!(
            items.iter().filter(|i| i.not_actual).count(),
            2,
            "test messages are marked"
        );
        assert!(
            items
                .iter()
                .all(|i| i.link.starts_with("http://capcp2.naad-adna.pelmorex.com/"))
        );
    }
}
