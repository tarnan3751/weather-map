//! Checks on everything a browser sends.

use crate::error::ApiError;
use crate::push::vapid::decode_b64url;
use std::net::IpAddr;
use url::{Host, Url};

pub(crate) const MAX_PLACES_PER_DEVICE: i64 = 20;
const MAX_NAME_CHARS: usize = 80;
const MAX_ENDPOINT_LEN: usize = 2048;

pub(crate) fn place_name(raw: &str) -> Result<String, ApiError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "name must not be empty",
        ));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ApiError::bad_request(
            "invalid_name",
            format!("name must be at most {MAX_NAME_CHARS} characters"),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "invalid_name",
            "name must not contain control characters",
        ));
    }
    Ok(name.to_string())
}

pub(crate) fn coordinates(lat: f64, lon: f64) -> Result<(), ApiError> {
    if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
        return Err(ApiError::bad_request(
            "invalid_coordinates",
            "lat must be between -90 and 90",
        ));
    }
    if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
        return Err(ApiError::bad_request(
            "invalid_coordinates",
            "lon must be between -180 and 180",
        ));
    }
    Ok(())
}

/// A push endpoint must be an `https` URL on a public host name: the
/// server will POST to it, so it must not be usable to reach loopback or
/// private addresses. `allow_insecure` lifts that for local test doubles.
pub(crate) fn push_endpoint(raw: &str, allow_insecure: bool) -> Result<Url, ApiError> {
    let bad = |why: &str| ApiError::bad_request("invalid_subscription", format!("endpoint {why}"));
    if raw.len() > MAX_ENDPOINT_LEN {
        return Err(bad("is too long"));
    }
    let url = Url::parse(raw).map_err(|_| bad("is not a URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad("must not carry credentials"));
    }
    if allow_insecure {
        return match url.scheme() {
            "https" | "http" if url.host().is_some() => Ok(url),
            _ => Err(bad("must be an http(s) URL")),
        };
    }
    if url.scheme() != "https" {
        return Err(bad("must be an https URL"));
    }
    match url.host() {
        Some(Host::Domain(d))
            if d.contains('.') && !d.ends_with(".localhost") && !d.ends_with(".local") => {}
        Some(Host::Ipv4(ip)) if is_public(IpAddr::V4(ip)) => {}
        Some(Host::Ipv6(ip)) if is_public(IpAddr::V6(ip)) => {}
        _ => return Err(bad("must be on a public host")),
    }
    Ok(url)
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.to_ipv4_mapped().is_some())
        }
    }
}

/// `p256dh` must be an uncompressed point on P-256 and `auth` 16 bytes,
/// or every push to the subscription would fail.
pub(crate) fn subscription_keys(p256dh: &str, auth: &str) -> Result<(), ApiError> {
    let bad = |why: &str| ApiError::bad_request("invalid_subscription", why.to_string());
    let point = decode_b64url(p256dh.trim()).map_err(|_| bad("keys.p256dh is not base64url"))?;
    if point.len() != 65 || point[0] != 0x04 || p256::PublicKey::from_sec1_bytes(&point).is_err() {
        return Err(bad("keys.p256dh is not an uncompressed P-256 public key"));
    }
    let auth = decode_b64url(auth.trim()).map_err(|_| bad("keys.auth is not base64url"))?;
    if auth.len() != 16 {
        return Err(bad("keys.auth must be 16 bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::vapid::generate;

    #[test]
    fn names() {
        assert_eq!(place_name("  Home ").unwrap(), "Home");
        assert_eq!(place_name("Café Ω").unwrap(), "Café Ω");
        assert!(place_name("").is_err());
        assert!(place_name("   ").is_err());
        assert!(place_name("a\nb").is_err());
        assert!(
            place_name(&"é".repeat(80)).is_ok(),
            "the limit counts characters, not bytes"
        );
        assert!(place_name(&"é".repeat(81)).is_err());
    }

    #[test]
    fn coordinate_ranges() {
        assert!(coordinates(35.22, -97.44).is_ok());
        assert!(coordinates(90.0, 180.0).is_ok());
        assert!(coordinates(-90.0, -180.0).is_ok());
        assert!(coordinates(90.0001, 0.0).is_err());
        assert!(coordinates(0.0, -180.5).is_err());
        assert!(coordinates(f64::NAN, 0.0).is_err());
        assert!(coordinates(0.0, f64::INFINITY).is_err());
    }

    #[test]
    fn endpoints() {
        for ok in [
            "https://fcm.googleapis.com/fcm/send/abc:def",
            "https://updates.push.services.mozilla.com/wpush/v2/gAAAA",
            "https://web.push.apple.com/QGx",
            "https://8.8.8.8/push",
        ] {
            assert!(push_endpoint(ok, false).is_ok(), "{ok}");
        }
        for bad in [
            "http://fcm.googleapis.com/fcm/send/abc",
            "ftp://example.com/x",
            "https://localhost/x",
            "https://push.localhost/x",
            "https://intranet/x",
            "https://127.0.0.1/x",
            "https://10.1.2.3/x",
            "https://192.168.0.1/x",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/x",
            "https://[fd00::1]/x",
            "https://user:pw@example.com/x",
            "not a url",
        ] {
            assert!(push_endpoint(bad, false).is_err(), "{bad}");
        }
        assert!(
            push_endpoint(&format!("https://example.com/{}", "a".repeat(3000)), false).is_err()
        );
        assert!(push_endpoint("http://127.0.0.1:9999/push", true).is_ok());
        assert!(push_endpoint("ftp://127.0.0.1/push", true).is_err());
    }

    #[test]
    fn keys() {
        let (public, _) = generate();
        let auth = "BTBZMqHH6r4Tts7J_aSIgg";
        assert!(subscription_keys(&public, auth).is_ok());
        assert!(subscription_keys(&public, "BTBZMqHH6r4Tts7J").is_err());
        assert!(subscription_keys("BAAA", auth).is_err());
        // Right length, not on the curve.
        let mut off_curve = vec![4u8];
        off_curve.extend([1u8; 64]);
        use base64::Engine;
        let off_curve = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(off_curve);
        assert!(subscription_keys(&off_curve, auth).is_err());
    }
}
