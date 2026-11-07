//! A small, defensive XML reader for third-party feeds and CAP documents.
//!
//! It builds an element tree from quick-xml events under hard limits: the
//! caller bounds the document size, and nesting depth, element count and
//! text per element are bounded here. Only the five predefined entities,
//! character references and a few HTML entities are resolved; nothing is
//! ever fetched, and a document whose DOCTYPE declares entities is refused
//! outright (no external entities, no entity expansion bombs). Names are
//! compared without their namespace prefix, so CAP 1.1 and 1.2, and
//! `cap:`-prefixed elements inside Atom, read the same.

use super::text;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

const MAX_DEPTH: usize = 64;
const MAX_ELEMENTS: usize = 400_000;
/// Text kept per element. Large CAP polygons run to a few hundred kilobytes.
const MAX_TEXT_BYTES: usize = 4 << 20;

#[derive(Debug, Default, Clone)]
pub(crate) struct Element {
    /// Local name, without a namespace prefix.
    pub name: String,
    /// Attributes by local name.
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    /// The element's own text (character data and CDATA), untrimmed.
    pub text: String,
}

impl Element {
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children
            .iter()
            .filter(move |c| c.name.eq_ignore_ascii_case(name))
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
    }

    /// The element's text, trimmed; `None` when empty.
    pub fn text(&self) -> Option<&str> {
        Some(self.text.trim()).filter(|t| !t.is_empty())
    }

    /// The trimmed, non-empty text of the first child called `name`.
    pub fn child_text(&self, name: &str) -> Option<&str> {
        self.children
            .iter()
            .filter(|c| c.name.eq_ignore_ascii_case(name))
            .find_map(Element::text)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum XmlError {
    #[error("not well-formed XML: {0}")]
    Syntax(String),
    #[error("document declares entities in its DOCTYPE")]
    EntityDeclaration,
    #[error("document is nested more than {MAX_DEPTH} levels deep")]
    TooDeep,
    #[error("document has more than {MAX_ELEMENTS} elements")]
    TooManyElements,
    #[error("document has no root element")]
    Empty,
}

/// Parse a whole document into its root element.
pub(crate) fn parse(bytes: &[u8]) -> Result<Element, XmlError> {
    let text = decode(bytes);
    let mut reader = Reader::from_str(&text);
    let config = reader.config_mut();
    config.trim_text(false);
    config.check_end_names = true;
    config.expand_empty_elements = false;
    // A bare `&` in text is a common feed mistake; keep it as text.
    config.allow_dangling_amp = true;

    let syntax =
        |e: &dyn std::fmt::Display| XmlError::Syntax(e.to_string().chars().take(160).collect());
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut elements = 0usize;
    loop {
        let event = reader.read_event().map_err(|e| syntax(&e))?;
        // An element read to its end, for its parent.
        let done = match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                elements += 1;
                if elements > MAX_ELEMENTS {
                    return Err(XmlError::TooManyElements);
                }
                let el = element_from(start);
                if matches!(event, Event::Empty(_)) {
                    el
                } else if stack.len() >= MAX_DEPTH {
                    return Err(XmlError::TooDeep);
                } else {
                    stack.push(el);
                    continue;
                }
            }
            Event::End(_) => stack.pop().ok_or_else(|| syntax(&"unexpected end tag"))?,
            Event::Text(t) if !stack.is_empty() => {
                push_text(&mut stack, &t.decode().map_err(|e| syntax(&e))?);
                continue;
            }
            Event::CData(t) if !stack.is_empty() => {
                push_text(&mut stack, &t.decode().map_err(|e| syntax(&e))?);
                continue;
            }
            Event::GeneralRef(r) if !stack.is_empty() => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => {
                        let name = r.decode().map_err(|e| syntax(&e))?;
                        text::named_entity(&name).map_or_else(|| format!("&{name};"), String::from)
                    }
                };
                push_text(&mut stack, &resolved);
                continue;
            }
            Event::DocType(d) => {
                let decl = d.decode().unwrap_or_default();
                if decl.contains("<!ENTITY") || decl.contains("<!entity") {
                    return Err(XmlError::EntityDeclaration);
                }
                continue;
            }
            Event::Eof => break,
            _ => continue,
        };
        match stack.last_mut() {
            Some(parent) => parent.children.push(done),
            // Whatever follows the root element is not read.
            None => {
                root = Some(done);
                break;
            }
        }
    }
    if !stack.is_empty() {
        return Err(XmlError::Syntax("document ends inside an element".into()));
    }
    root.ok_or(XmlError::Empty)
}

fn element_from(start: &quick_xml::events::BytesStart<'_>) -> Element {
    let name = String::from_utf8_lossy(start.local_name().as_ref()).into_owned();
    let mut attrs = Vec::new();
    for attr in start.attributes().with_checks(false).flatten().take(64) {
        let key = String::from_utf8_lossy(attr.key.local_name().as_ref()).into_owned();
        let value = match attr.normalized_value(quick_xml::XmlVersion::Explicit1_0) {
            Ok(v) => v.into_owned(),
            Err(_) => String::from_utf8_lossy(&attr.value).into_owned(),
        };
        attrs.push((key, value));
    }
    Element {
        name,
        attrs,
        children: Vec::new(),
        text: String::new(),
    }
}

/// Text for the innermost open element, up to its limit.
fn push_text(stack: &mut [Element], s: &str) {
    let Some(el) = stack.last_mut() else { return };
    let room = MAX_TEXT_BYTES.saturating_sub(el.text.len());
    if room == 0 {
        return;
    }
    if s.len() <= room {
        el.text.push_str(s);
    } else {
        let mut cut = room;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        el.text.push_str(&s[..cut]);
    }
}

/// Bytes to text: UTF-8 (with or without a BOM), UTF-16 with a BOM, or a
/// declared single-byte Latin encoding. Anything else is read as lossy UTF-8.
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if bytes.len() >= 2 && (bytes[..2] == [0xFF, 0xFE] || bytes[..2] == [0xFE, 0xFF]) {
        let little = bytes[0] == 0xFF;
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| {
                if little {
                    u16::from_le_bytes(c)
                } else {
                    u16::from_be_bytes(c)
                }
            })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) if declares_latin(bytes) => bytes.iter().map(|&b| latin_char(b)).collect(),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn declares_latin(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).to_ascii_lowercase();
    let Some(decl) = head.strip_prefix("<?xml") else {
        return false;
    };
    let decl = decl.split("?>").next().unwrap_or("");
    [
        "iso-8859-1",
        "iso8859-1",
        "latin1",
        "latin-1",
        "windows-1252",
        "cp1252",
        "iso-8859-15",
    ]
    .iter()
    .any(|enc| decl.contains(enc))
}

/// Windows-1252, which is what documents labelled ISO-8859-1 mean in practice.
fn latin_char(b: u8) -> char {
    const HIGH: [char; 32] = [
        '\u{20ac}', '\u{fffd}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{fffd}', '\u{17d}',
        '\u{fffd}', '\u{fffd}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}',
        '\u{2013}', '\u{2014}', '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}',
        '\u{fffd}', '\u{17e}', '\u{178}',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_namespaced_documents_by_local_name() {
        let doc = br#"<?xml version="1.0"?>
            <cap:alert xmlns:cap="urn:oasis:names:tc:emergency:cap:1.2">
              <cap:identifier> abc </cap:identifier>
              <cap:info><cap:event>Wind &amp; rain&#x21;</cap:event><cap:empty/></cap:info>
              <cap:info><cap:event><![CDATA[<b>Snow</b>]]></cap:event></cap:info>
            </cap:alert>"#;
        let root = parse(doc).unwrap();
        assert_eq!(root.name, "alert");
        assert_eq!(root.child_text("identifier"), Some("abc"));
        let events: Vec<&str> = root
            .children_named("info")
            .filter_map(|i| i.child_text("event"))
            .collect();
        assert_eq!(events, ["Wind & rain!", "<b>Snow</b>"]);
        assert!(
            root.children_named("info")
                .next()
                .unwrap()
                .children_named("empty")
                .next()
                .is_some()
        );
    }

    #[test]
    fn entity_declarations_are_refused() {
        let xxe = br#"<?xml version="1.0"?>
            <!DOCTYPE alert [ <!ENTITY xxe SYSTEM "file:///etc/passwd"> ]>
            <alert><identifier>&xxe;</identifier></alert>"#;
        assert!(matches!(parse(xxe), Err(XmlError::EntityDeclaration)));
        let laughs = br#"<?xml version="1.0"?>
            <!DOCTYPE lolz [ <!ENTITY lol "lol"> <!ENTITY lol2 "&lol;&lol;&lol;&lol;"> ]>
            <lolz>&lol2;</lolz>"#;
        assert!(matches!(parse(laughs), Err(XmlError::EntityDeclaration)));
        // An undeclared entity is kept as text, never resolved.
        let root = parse(b"<a>x &unknown; y &nbsp;z</a>").unwrap();
        assert_eq!(root.text, "x &unknown; y \u{a0}z");
        // A DOCTYPE without declarations is harmless.
        assert!(parse(b"<!DOCTYPE rss><rss><channel/></rss>").is_ok());
    }

    #[test]
    fn limits_hold() {
        let deep = format!(
            "{}{}",
            "<a>".repeat(MAX_DEPTH + 1),
            "</a>".repeat(MAX_DEPTH + 1)
        );
        assert!(matches!(parse(deep.as_bytes()), Err(XmlError::TooDeep)));
        let wide = format!("<r>{}</r>", "<e/>".repeat(MAX_ELEMENTS + 1));
        assert!(matches!(
            parse(wide.as_bytes()),
            Err(XmlError::TooManyElements)
        ));
        let long = format!("<r>{}</r>", "x".repeat(MAX_TEXT_BYTES + 10));
        assert_eq!(parse(long.as_bytes()).unwrap().text.len(), MAX_TEXT_BYTES);
        assert!(matches!(parse(b""), Err(XmlError::Empty)));
        assert!(parse(b"<a><b></a>").is_err());
        assert!(parse(b"<a>").is_err());
        assert!(parse(b"not xml at all <<<").is_err());
    }

    #[test]
    fn encodings() {
        let latin = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><a>caf\xe9 \x80</a>";
        assert_eq!(parse(latin).unwrap().text, "caf\u{e9} \u{20ac}");
        let bom = b"\xEF\xBB\xBF<a>ok</a>";
        assert_eq!(parse(bom).unwrap().text, "ok");
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(
                "<a>\u{4e2d}</a>"
                    .encode_utf16()
                    .flat_map(|u| u.to_le_bytes()),
            )
            .collect();
        assert_eq!(parse(&utf16).unwrap().text, "\u{4e2d}");
    }
}
