//! Anonymous device identity: a random bearer token, of which the server
//! keeps only the SHA-256.

use crate::Alerts;
use crate::error::ApiError;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// 32 random bytes in unpadded base64url.
const TOKEN_LEN: usize = 43;

/// How stale `last_seen_at` may get before a request refreshes it; keeps
/// ordinary reads from turning into writes.
const LAST_SEEN_RESOLUTION: chrono::Duration = chrono::Duration::hours(1);

pub(crate) fn new_token() -> anyhow::Result<(String, [u8; 32])> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("system RNG: {e}"))?;
    let token = B64.encode(bytes);
    let hash = hash_token(&token);
    Ok((token, hash))
}

fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn well_formed(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `Authorization: Bearer <token>`.
fn bearer(parts: &Parts) -> Option<&str> {
    let value = parts
        .headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// The calling device, from its bearer token. Rejects with 401.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Device {
    pub id: Uuid,
}

impl FromRequestParts<Alerts> for Device {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Alerts,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer(parts)
            .filter(|t| well_formed(t))
            .ok_or_else(ApiError::unauthorized)?;
        let pool = &state.inner.pool;
        let row: Option<(Uuid, DateTime<Utc>)> =
            sqlx::query_as("SELECT id, last_seen_at FROM devices WHERE token_hash = $1")
                .bind(hash_token(token).as_slice())
                .fetch_optional(pool)
                .await?;
        let (id, last_seen) = row.ok_or_else(ApiError::unauthorized)?;
        if Utc::now() - last_seen > LAST_SEEN_RESOLUTION {
            sqlx::query("UPDATE devices SET last_seen_at = now() WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await?;
        }
        Ok(Device { id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_base64url_and_hashed() {
        let (a, hash_a) = new_token().unwrap();
        let (b, _) = new_token().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), TOKEN_LEN);
        assert!(well_formed(&a));
        assert_eq!(B64.decode(&a).unwrap().len(), 32);
        assert_eq!(hash_a, hash_token(&a));
        assert_ne!(hash_a, hash_token(&b));
    }

    #[test]
    fn hash_is_plain_sha256_of_the_token_text() {
        // FIPS 180-2 test vector.
        let h = hash_token("abc");
        assert_eq!(
            h.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn bearer_parsing() {
        let parts = |v: &str| {
            let req = axum::http::Request::builder()
                .header("authorization", v)
                .body(())
                .unwrap();
            req.into_parts().0
        };
        assert_eq!(bearer(&parts("Bearer abc")), Some("abc"));
        assert_eq!(bearer(&parts("bearer  abc ")), Some("abc"));
        assert_eq!(bearer(&parts("Basic abc")), None);
        assert_eq!(bearer(&parts("Bearer")), None);
        assert!(!well_formed("short"));
        assert!(!well_formed(&format!("{}=", "a".repeat(42))));
        assert!(well_formed(&"a".repeat(43)));
    }
}
