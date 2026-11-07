//! JSON response bodies made once and served many times, with a weak
//! `ETag` so a client that already has them gets a `304`.
//!
//! Big bodies are kept as raw deflate pieces that can be joined into one
//! gzip stream: each piece ends on a byte boundary (a sync flush) without a
//! final block, so a body made of cached pieces and a few bytes that change
//! per request (a timestamp) is gzipped without compressing it again.

use axum::http::header::{
    ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_TYPE, ETAG, IF_NONE_MATCH, VARY,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use flate2::{Compress, Compression, Crc, FlushCompress};
use std::hash::{DefaultHasher, Hasher};

/// Compression level of cached pieces: each is compressed once per change.
const LEVEL: u32 = 6;

/// Raw deflate data for some bytes, ending on a byte boundary and without
/// a final block, so another piece can follow it.
pub struct Deflated {
    data: Bytes,
    crc: Crc,
}

impl Deflated {
    /// Compress `input` (CPU-heavy for big inputs: call it off the runtime).
    pub fn compress(input: &[u8]) -> Self {
        let mut c = Compress::new(Compression::new(LEVEL), false);
        let mut out = Vec::with_capacity(input.len() / 4 + 64);
        loop {
            let done = c.total_in() as usize;
            c.compress_vec(&input[done..], &mut out, FlushCompress::Sync)
                .expect("deflate of in-memory bytes cannot fail");
            // With room left over, the flush has completed.
            if c.total_in() as usize == input.len() && out.len() < out.capacity() {
                break;
            }
            out.reserve(out.capacity().max(4096));
        }
        Self::with_crc(out.into(), input)
    }

    /// `input` as stored (uncompressed) blocks: for the few bytes around
    /// the cached pieces.
    pub fn stored(input: &[u8]) -> Self {
        let mut out = Vec::with_capacity(input.len() + 5 * (input.len() / 0xffff + 1));
        for block in input.chunks(0xffff) {
            let len = block.len() as u16;
            // BFINAL 0, BTYPE 00, padded to the byte boundary we start on.
            out.push(0);
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(block);
        }
        Self::with_crc(out.into(), input)
    }

    fn with_crc(data: Bytes, input: &[u8]) -> Self {
        let mut crc = Crc::new();
        crc.update(input);
        Self { data, crc }
    }
}

/// One gzip member from pieces, in order.
pub fn gzip<'a>(pieces: impl IntoIterator<Item = &'a Deflated>) -> Bytes {
    let pieces: Vec<&Deflated> = pieces.into_iter().collect();
    let len: usize = pieces.iter().map(|p| p.data.len()).sum();
    let mut out = Vec::with_capacity(len + 20);
    // Magic, deflate, no flags, no mtime, no extra flags, unknown OS.
    out.extend_from_slice(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff]);
    let mut crc = Crc::new();
    for p in pieces {
        out.extend_from_slice(&p.data);
        crc.combine(&p.crc);
    }
    // An empty final block with fixed codes, then the trailer.
    out.extend_from_slice(&[0x03, 0x00]);
    out.extend_from_slice(&crc.sum().to_le_bytes());
    out.extend_from_slice(&crc.amount().to_le_bytes());
    out.into()
}

/// A weak validator for some bytes, the same in every process.
pub fn etag_of<B: AsRef<[u8]>>(parts: &[B]) -> HeaderValue {
    let mut h = DefaultHasher::new();
    for p in parts {
        h.write(p.as_ref());
    }
    HeaderValue::try_from(format!("W/\"{:016x}\"", h.finish())).expect("hex is a valid header")
}

/// `If-None-Match` names `etag` (weak comparison, RFC 9110 13.1.2).
pub fn not_modified(request: &HeaderMap, etag: &HeaderValue) -> bool {
    let ours = etag
        .as_bytes()
        .strip_prefix(b"W/")
        .unwrap_or(etag.as_bytes());
    request
        .get_all(IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|t| t == "*" || t.strip_prefix("W/").unwrap_or(t).as_bytes() == ours)
}

/// Whether `Accept-Encoding` allows gzip (a `q=0` refuses it).
pub fn accepts_gzip(request: &HeaderMap) -> bool {
    request
        .get_all(ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|item| {
            let mut parts = item.split(';');
            let coding = parts.next().unwrap_or("").trim();
            let refused = parts.any(|p| {
                p.trim()
                    .strip_prefix("q=")
                    .is_some_and(|q| q.trim().parse::<f32>().is_ok_and(|q| q == 0.0))
            });
            (coding.eq_ignore_ascii_case("gzip") || coding == "*") && !refused
        })
}

/// A JSON reply from ready bytes: `304` when the client has them, gzip
/// when it accepts that (with `Content-Length`; the server's compression
/// layer leaves an encoded body alone), the plain bytes otherwise.
pub fn respond(
    request: &HeaderMap,
    etag: &HeaderValue,
    cache_control: &'static str,
    json: impl FnOnce() -> Bytes,
    gzip: impl FnOnce() -> Bytes,
) -> Response {
    let common: [(HeaderName, HeaderValue); 3] = [
        (ETAG, etag.clone()),
        (CACHE_CONTROL, HeaderValue::from_static(cache_control)),
        (VARY, HeaderValue::from_static("accept-encoding")),
    ];
    if not_modified(request, etag) {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }
    let json_type = (CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if accepts_gzip(request) {
        let encoding = (CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        (common, [json_type, encoding], gzip()).into_response()
    } else {
        (common, [json_type], json()).into_response()
    }
}

/// A body that never changes while the server runs.
pub struct Fixed {
    json: Bytes,
    gzip: Bytes,
    etag: HeaderValue,
}

impl Fixed {
    pub fn new(json: Vec<u8>) -> Self {
        let deflated = Deflated::compress(&json);
        Self {
            etag: etag_of(&[&json]),
            gzip: gzip([&deflated]),
            json: json.into(),
        }
    }

    pub fn respond(&self, request: &HeaderMap, cache_control: &'static str) -> Response {
        respond(
            request,
            &self.etag,
            cache_control,
            || self.json.clone(),
            || self.gzip.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn gunzip(b: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(b)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    #[test]
    fn pieces_join_into_one_gzip_stream() {
        let big: Vec<u8> = (0..300_000u32)
            .flat_map(|i| (i % 251).to_le_bytes())
            .collect();
        let text = br#"{"frames":[{"valid":"2026-11-06T12:00:00Z"}]}"#.repeat(500);
        let pieces = [
            Deflated::stored(b"{\"a\":"),
            Deflated::compress(&text),
            Deflated::stored(&big[..70_000]),
            Deflated::compress(&big),
            Deflated::compress(b""),
            Deflated::stored(b""),
            Deflated::stored(b"}"),
        ];
        let plain = [&b"{\"a\":"[..], &text, &big[..70_000], &big, b"}"].concat();
        let gz = gzip(&pieces);
        assert_eq!(gunzip(&gz), plain);
        // A conforming decoder also sees exactly one member.
        let mut multi = Vec::new();
        flate2::read::MultiGzDecoder::new(&gz[..])
            .read_to_end(&mut multi)
            .unwrap();
        assert_eq!(multi, plain);
        assert!(gz.len() < plain.len() / 2);
        assert_eq!(gunzip(&gzip([])), b"");
    }

    #[test]
    fn validators_and_encodings() {
        let etag = etag_of(&[b"abc"]);
        assert!(etag.to_str().unwrap().starts_with("W/\""));
        assert_eq!(etag, etag_of(&[&b"a"[..], b"bc"]));
        assert_ne!(etag, etag_of(&[b"abd"]));
        let headers = |pairs: &[(HeaderName, &str)]| {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.append(k, HeaderValue::from_str(v).unwrap());
            }
            h
        };
        let bare = etag.to_str().unwrap().trim_start_matches("W/").to_string();
        assert!(not_modified(
            &headers(&[(IF_NONE_MATCH, etag.to_str().unwrap())]),
            &etag
        ));
        assert!(not_modified(
            &headers(&[(IF_NONE_MATCH, &format!("\"x\", {bare}"))]),
            &etag
        ));
        assert!(not_modified(&headers(&[(IF_NONE_MATCH, "*")]), &etag));
        assert!(!not_modified(
            &headers(&[(IF_NONE_MATCH, "W/\"0\"")]),
            &etag
        ));
        assert!(!not_modified(&HeaderMap::new(), &etag));
        assert!(accepts_gzip(&headers(&[(
            ACCEPT_ENCODING,
            "br, gzip;q=0.8"
        )])));
        assert!(accepts_gzip(&headers(&[(ACCEPT_ENCODING, "*")])));
        assert!(!accepts_gzip(&headers(&[(ACCEPT_ENCODING, "gzip;q=0")])));
        assert!(!accepts_gzip(&headers(&[(ACCEPT_ENCODING, "br")])));
        assert!(!accepts_gzip(&HeaderMap::new()));
    }
}
