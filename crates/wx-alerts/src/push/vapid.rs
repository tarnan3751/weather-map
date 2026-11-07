//! VAPID (RFC 8292): the application server signs a short-lived ES256 JWT
//! for each push service origin.

use crate::Vapid;
use anyhow::{Context, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p256::SecretKey;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::Generate;
use p256::elliptic_curve::sec1::ToSec1Point;
use serde::Serialize;
use url::Url;

/// Push services accept up to 24 hours; a short token limits the damage of
/// one leaking from a log or a proxy.
pub(crate) const TOKEN_LIFETIME_SECS: i64 = 30 * 60;

pub(crate) struct VapidSigner {
    key: SigningKey,
    public_key: String,
    subject: String,
}

impl std::fmt::Debug for VapidSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VapidSigner")
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

impl VapidSigner {
    pub fn new(v: &Vapid) -> anyhow::Result<Self> {
        let private =
            decode_b64url(v.private_key.trim()).context("VAPID private key is not base64url")?;
        anyhow::ensure!(
            private.len() == 32,
            "VAPID private key must be 32 bytes, got {}",
            private.len()
        );
        let secret = SecretKey::from_slice(&private)
            .map_err(|_| anyhow!("VAPID private key is not a valid P-256 scalar"))?;
        let derived = B64.encode(secret.public_key().to_sec1_point(false).as_bytes());
        let given =
            decode_b64url(v.public_key.trim()).context("VAPID public key is not base64url")?;
        anyhow::ensure!(
            B64.encode(&given) == derived,
            "VAPID public key does not belong to the private key"
        );
        let subject = v.subject.trim().to_string();
        anyhow::ensure!(
            subject.starts_with("mailto:") || subject.starts_with("https://"),
            "VAPID subject must be a mailto: or https: URI"
        );
        Ok(Self {
            key: SigningKey::from(&secret),
            public_key: derived,
            subject,
        })
    }

    /// The `Authorization` header value for a push to `endpoint`.
    pub fn authorization(&self, endpoint: &Url, now_unix: i64) -> anyhow::Result<String> {
        let jwt = self.token(&origin(endpoint)?, now_unix + TOKEN_LIFETIME_SECS)?;
        Ok(format!("vapid t={jwt}, k={}", self.public_key))
    }

    fn token(&self, audience: &str, exp: i64) -> anyhow::Result<String> {
        #[derive(Serialize)]
        struct Claims<'a> {
            aud: &'a str,
            exp: i64,
            sub: &'a str,
        }
        let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64.encode(serde_json::to_vec(&Claims {
            aud: audience,
            exp,
            sub: &self.subject,
        })?);
        let signing_input = format!("{header}.{claims}");
        let signature: Signature = self.key.sign(signing_input.as_bytes());
        Ok(format!(
            "{signing_input}.{}",
            B64.encode(signature.to_bytes())
        ))
    }
}

/// `https://fcm.googleapis.com/fcm/send/...` -> `https://fcm.googleapis.com`.
pub(crate) fn origin(endpoint: &Url) -> anyhow::Result<String> {
    let origin = endpoint.origin();
    anyhow::ensure!(origin.is_tuple(), "push endpoint has no origin");
    Ok(origin.ascii_serialization())
}

/// A fresh key pair as `(public, private)`, base64url without padding.
pub(crate) fn generate() -> (String, String) {
    let secret = SecretKey::generate();
    let public = secret.public_key().to_sec1_point(false);
    (B64.encode(public.as_bytes()), B64.encode(secret.to_bytes()))
}

/// Browsers hand out unpadded base64url; tolerate padding and the standard
/// alphabet too, since some tools produce those.
pub(crate) fn decode_b64url(s: &str) -> anyhow::Result<Vec<u8>> {
    let cleaned: String = s
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        })
        .collect();
    Ok(B64.decode(cleaned)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier;
    use serde_json::Value;

    fn vapid() -> Vapid {
        let (public_key, private_key) = generate();
        Vapid {
            public_key,
            private_key,
            subject: "mailto:ops@example.com".to_string(),
        }
    }

    #[test]
    fn generated_keys_have_the_documented_encodings() {
        let (public, private) = generate();
        let public = decode_b64url(&public).unwrap();
        let private = decode_b64url(&private).unwrap();
        assert_eq!(public.len(), 65);
        assert_eq!(public[0], 0x04, "uncompressed point");
        assert_eq!(private.len(), 32);
        let secret = SecretKey::from_slice(&private).unwrap();
        assert_eq!(
            secret.public_key().to_sec1_point(false).as_bytes(),
            public.as_slice()
        );
        assert_ne!(generate().1, generate().1);
    }

    #[test]
    fn rejects_mismatched_or_malformed_keys() {
        let mut v = vapid();
        assert!(VapidSigner::new(&v).is_ok());
        v.public_key = generate().0;
        assert!(VapidSigner::new(&v).is_err());
        let mut v = vapid();
        v.private_key = "not base64!".into();
        assert!(VapidSigner::new(&v).is_err());
        let mut v = vapid();
        v.subject = "ops@example.com".into();
        assert!(VapidSigner::new(&v).is_err());
    }

    #[test]
    fn signs_a_verifiable_token_for_the_endpoint_origin() {
        let v = vapid();
        let signer = VapidSigner::new(&v).unwrap();
        let endpoint =
            Url::parse("https://updates.push.services.mozilla.com:443/wpush/v2/abc?x=1").unwrap();
        let header = signer.authorization(&endpoint, 1_800_000_000).unwrap();
        let (t, k) = header
            .strip_prefix("vapid t=")
            .unwrap()
            .split_once(", k=")
            .unwrap();
        assert_eq!(k, v.public_key);

        let parts: Vec<&str> = t.split('.').collect();
        assert_eq!(parts.len(), 3);
        let head: Value = serde_json::from_slice(&B64.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(head["alg"], "ES256");
        let claims: Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://updates.push.services.mozilla.com");
        assert_eq!(claims["sub"], "mailto:ops@example.com");
        assert_eq!(claims["exp"], 1_800_000_000 + TOKEN_LIFETIME_SECS);

        let key = VerifyingKey::from_sec1_bytes(&decode_b64url(k).unwrap()).unwrap();
        let sig = Signature::from_slice(&B64.decode(parts[2]).unwrap()).unwrap();
        key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .unwrap();
    }

    #[test]
    fn decodes_lenient_base64() {
        assert_eq!(decode_b64url("-_8").unwrap(), [0xfb, 0xff]);
        assert_eq!(decode_b64url("+/8=").unwrap(), [0xfb, 0xff]);
        assert!(decode_b64url("@@").is_err());
    }
}
