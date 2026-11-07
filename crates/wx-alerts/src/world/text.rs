//! Turning third-party text into plain text that is safe to store and show.
//!
//! Feeds put HTML into fields that CAP defines as plain text, escape it
//! twice, or pad it with control characters. Everything stored goes
//! through [`plain`]: tags become line breaks or nothing, HTML entities are
//! decoded, control characters (and the bidi overrides that can disguise
//! text) are removed, whitespace is tidied, and the result is cut to a
//! length. What comes out is meant to be shown as text, never as markup.

/// Clean `raw` and keep at most `max_chars` characters; `None` when nothing
/// is left.
pub(crate) fn plain(raw: &str, max_chars: usize) -> Option<String> {
    let looks_like_markup =
        (raw.contains('<') && raw.contains('>')) || raw.contains("&lt;") || raw.contains("&#");
    let s = if looks_like_markup {
        strip_markup(raw)
    } else {
        raw.to_string()
    };
    let s = decode_entities(&s);
    let mut out = String::with_capacity(s.len().min(max_chars * 4));
    let mut count = 0usize;
    let mut pending_space = false;
    let mut newlines = 0usize;
    for c in s.chars() {
        let c = match c {
            '\r' => '\n',
            '\t' | '\u{a0}' | '\u{2007}' | '\u{202f}' => ' ',
            _ => c,
        };
        if c == '\n' {
            if count > 0 {
                newlines += 1;
            }
            pending_space = false;
            continue;
        }
        if c.is_control() || is_disguising(c) {
            continue;
        }
        if c.is_whitespace() {
            pending_space = count > 0 && newlines == 0;
            continue;
        }
        // Flush the separator owed before this character.
        let sep: &str = match newlines {
            0 if pending_space => " ",
            0 => "",
            1 => "\n",
            _ => "\n\n",
        };
        if count + sep.chars().count() + 1 > max_chars {
            break;
        }
        out.push_str(sep);
        count += sep.chars().count();
        newlines = 0;
        pending_space = false;
        out.push(c);
        count += 1;
    }
    (!out.is_empty()).then_some(out)
}

/// A single line: [`plain`] with line breaks turned into spaces.
pub(crate) fn line(raw: &str, max_chars: usize) -> Option<String> {
    plain(&raw.replace(['\n', '\r'], " "), max_chars)
}

/// Text that serves as a key or code: no control characters or spaces at
/// either end, bounded in length, and refused rather than cut when longer.
pub(crate) fn token(raw: &str, max_len: usize) -> Option<String> {
    let t = raw.trim();
    (!t.is_empty() && t.len() <= max_len && !t.chars().any(|c| c.is_control()))
        .then(|| t.to_string())
}

/// An `http(s)` URL worth handing to a browser, or `None`.
pub(crate) fn web_url(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw.trim()).ok()?;
    let ok = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.as_str().len() <= 1000;
    ok.then(|| url.to_string())
}

/// Bidi overrides and isolates, and zero-width characters that only hide
/// what text says. Marks (U+200E, U+200F) stay: right-to-left text uses them.
fn is_disguising(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200b}' | '\u{feff}')
}

/// Drop tags; `<br>`, `<p>`, `<li>` and `<div>` boundaries become line breaks.
fn strip_markup(raw: &str) -> String {
    // Markup that was escaped once more than it should have been.
    let raw = if !raw.contains('<') && raw.contains("&lt;") {
        raw.replace("&lt;", "<").replace("&gt;", ">")
    } else {
        raw.to_string()
    };
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw.as_str();
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        // A '<' that does not start a tag ("< 5 mm") is text.
        let starts_tag = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!');
        match after.find('>') {
            Some(close) if starts_tag => {
                let tag = after[..close].trim_start_matches('/').to_ascii_lowercase();
                let name: String = tag
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect();
                if matches!(
                    name.as_str(),
                    "br" | "p" | "li" | "div" | "tr" | "h1" | "h2" | "h3" | "h4"
                ) {
                    out.push('\n');
                }
                // Script and style bodies are not text.
                if (name == "script" || name == "style") && !after.starts_with('/') {
                    let end = format!("</{name}");
                    let lower = after.to_ascii_lowercase();
                    match lower.find(&end) {
                        Some(e) => {
                            let tail = &after[e..];
                            rest = tail.find('>').map_or("", |g| &tail[g + 1..]);
                        }
                        None => rest = "",
                    }
                    continue;
                }
                rest = &after[close + 1..];
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let semi = after.find(';').filter(|&i| i <= 10);
        let decoded = semi.and_then(|i| entity(&after[..i]));
        match (semi, decoded) {
            (Some(i), Some(c)) => {
                out.push(c);
                rest = &after[i + 1..];
            }
            _ => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    match name.strip_prefix('#') {
        Some(num) => char::from_u32(match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        }),
        None => named_entity(name).and_then(|s| s.chars().next()),
    }
}

/// The XML entities, and the HTML ones that turn up in feeds although XML
/// does not define them.
pub(crate) fn named_entity(name: &str) -> Option<&'static str> {
    const ENTITIES: [(&str, &str); 14] = [
        ("lt", "<"),
        ("gt", ">"),
        ("amp", "&"),
        ("quot", "\""),
        ("apos", "'"),
        ("nbsp", "\u{a0}"),
        ("ndash", "\u{2013}"),
        ("mdash", "\u{2014}"),
        ("deg", "\u{b0}"),
        ("rsquo", "\u{2019}"),
        ("lsquo", "\u{2018}"),
        ("rdquo", "\u{201d}"),
        ("ldquo", "\u{201c}"),
        ("hellip", "\u{2026}"),
    ];
    ENTITIES.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_becomes_plain_text() {
        assert_eq!(
            plain(
                "<p>Heavy <b>rain</b>.</p><p>Stay&nbsp;home &amp; safe</p>",
                100
            )
            .as_deref(),
            Some("Heavy rain.\n\nStay home & safe"),
            "paragraphs are separated by a blank line"
        );
        assert_eq!(
            plain("Line one<br/>Line two", 100).as_deref(),
            Some("Line one\nLine two")
        );
        assert_eq!(
            plain("&lt;b&gt;Escaped twice&lt;/b&gt;", 100).as_deref(),
            Some("Escaped twice")
        );
        assert_eq!(
            plain("<script>alert(1)</script>Wind < 5 m/s", 100).as_deref(),
            Some("Wind < 5 m/s"),
            "script bodies go, a bare '<' stays"
        );
        assert_eq!(plain("<img src=x onerror=alert(1)>", 100), None);
    }

    #[test]
    fn whitespace_and_controls_are_tidied() {
        assert_eq!(
            plain("  a \t b\r\n\r\n\r\n c\u{0}\u{7}  ", 100).as_deref(),
            Some("a b\n\nc")
        );
        assert_eq!(
            plain("evil\u{202e}txt.exe", 100).as_deref(),
            Some("eviltxt.exe")
        );
        assert_eq!(
            plain("\u{200f}مرحبا", 100).as_deref(),
            Some("\u{200f}مرحبا"),
            "RTL marks stay"
        );
        assert_eq!(plain(" \n\t ", 100), None);
    }

    #[test]
    fn length_is_counted_in_characters() {
        assert_eq!(plain(&"é".repeat(50), 10).unwrap().chars().count(), 10);
        assert_eq!(line("a\nb", 10).as_deref(), Some("a b"));
    }

    #[test]
    fn urls_and_tokens() {
        assert_eq!(
            web_url(" https://dwd.de/warnungen ").as_deref(),
            Some("https://dwd.de/warnungen")
        );
        assert_eq!(web_url("javascript:alert(1)"), None);
        assert_eq!(web_url("ftp://x.org/a"), None);
        assert_eq!(web_url("https://user:pw@x.org/"), None);
        assert_eq!(token(" ab ", 8).as_deref(), Some("ab"));
        assert_eq!(token("a\u{0}b", 8), None);
        assert_eq!(token("abcdefghi", 8), None);
    }
}
