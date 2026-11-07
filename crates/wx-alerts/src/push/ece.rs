//! Message encryption for Web Push (RFC 8291) in the `aes128gcm` content
//! coding (RFC 8188), as a single record.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
#[cfg(test)]
use anyhow::Context;
use anyhow::anyhow;
use hkdf::Hkdf;
use p256::elliptic_curve::Generate;
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

/// The record size every push service accepts.
pub(crate) const RECORD_SIZE: u32 = 4096;
/// 4096 minus the 86-byte header, the padding delimiter and the AEAD tag.
pub(crate) const MAX_PLAINTEXT: usize = 3993;
const HEADER_LEN: usize = 16 + 4 + 1 + 65;

/// Encrypt `plaintext` to a subscription's `p256dh` key and `auth` secret
/// with a fresh ephemeral key and salt.
pub(crate) fn encrypt(
    plaintext: &[u8],
    ua_public: &PublicKey,
    auth: &[u8; 16],
) -> anyhow::Result<Vec<u8>> {
    let as_secret = SecretKey::try_generate().map_err(|e| anyhow!("system RNG: {e}"))?;
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| anyhow!("system RNG: {e}"))?;
    encrypt_with(plaintext, ua_public, auth, &as_secret, &salt)
}

pub(crate) fn encrypt_with(
    plaintext: &[u8],
    ua_public: &PublicKey,
    auth: &[u8; 16],
    as_secret: &SecretKey,
    salt: &[u8; 16],
) -> anyhow::Result<Vec<u8>> {
    if plaintext.len() > MAX_PLAINTEXT {
        anyhow::bail!(
            "push payload is {} bytes, the limit is {MAX_PLAINTEXT}",
            plaintext.len()
        );
    }
    let as_public = as_secret.public_key().to_sec1_point(false);
    let shared = as_secret.diffie_hellman(ua_public);
    let (cek, nonce) = derive(
        shared.raw_secret_bytes().as_slice(),
        ua_public.to_sec1_point(false).as_bytes(),
        as_public.as_bytes(),
        auth,
        salt,
    )?;

    let mut record = Vec::with_capacity(plaintext.len() + 1);
    record.extend_from_slice(plaintext);
    // Delimiter of the last (only) record; no further padding.
    record.push(0x02);
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|_| anyhow!("bad content key length"))?;
    let ciphertext = cipher
        .encrypt(&Nonce::from(nonce), record.as_slice())
        .map_err(|_| anyhow!("AES-GCM encryption failed"))?;

    let mut body = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.as_bytes().len() as u8);
    body.extend_from_slice(as_public.as_bytes());
    body.extend_from_slice(&ciphertext);
    Ok(body)
}

/// RFC 8291 section 3.4 then RFC 8188 section 2.2: the content encryption
/// key and nonce.
fn derive(
    ecdh_secret: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    auth: &[u8; 16],
    salt: &[u8; 16],
) -> anyhow::Result<([u8; 16], [u8; 12])> {
    let mut key_info = Vec::with_capacity(14 + 65 + 65);
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), ecdh_secret)
        .expand(&key_info, &mut ikm)
        .map_err(|_| anyhow!("HKDF expand"))?;

    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek)
        .map_err(|_| anyhow!("HKDF expand"))?;
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce)
        .map_err(|_| anyhow!("HKDF expand"))?;
    Ok((cek, nonce))
}

/// What a browser does with the body: used to test the encryption.
#[cfg(test)]
pub(crate) fn decrypt(
    body: &[u8],
    ua_secret: &SecretKey,
    auth: &[u8; 16],
) -> anyhow::Result<Vec<u8>> {
    let salt: [u8; 16] = body.get(..16).context("short body")?.try_into()?;
    let rs = u32::from_be_bytes(body.get(16..20).context("short body")?.try_into()?);
    let idlen = *body.get(20).context("short body")? as usize;
    let as_public = body.get(21..21 + idlen).context("short body")?;
    let record = &body[21 + idlen..];
    anyhow::ensure!(record.len() <= rs as usize, "record larger than rs");
    let as_key = PublicKey::from_sec1_bytes(as_public).map_err(|_| anyhow!("bad sender key"))?;
    let shared = ua_secret.diffie_hellman(&as_key);
    let (cek, nonce) = derive(
        shared.raw_secret_bytes().as_slice(),
        ua_secret.public_key().to_sec1_point(false).as_bytes(),
        as_public,
        auth,
        &salt,
    )?;
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|_| anyhow!("key"))?;
    let mut plain = cipher
        .decrypt(&Nonce::from(nonce), record)
        .map_err(|_| anyhow!("authentication failed"))?;
    while plain.last() == Some(&0) {
        plain.pop();
    }
    anyhow::ensure!(plain.pop() == Some(0x02), "missing last-record delimiter");
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;

    fn b64(s: &str) -> Vec<u8> {
        B64.decode(s).unwrap()
    }

    /// RFC 8291 Appendix A.
    #[test]
    fn matches_the_rfc_8291_example() {
        let plaintext = b"When I grow up, I want to be a watermelon";
        let as_secret =
            SecretKey::from_slice(&b64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
        let ua_secret =
            SecretKey::from_slice(&b64("q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94")).unwrap();
        let ua_public = PublicKey::from_sec1_bytes(&b64(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        ))
        .unwrap();
        assert_eq!(ua_secret.public_key(), ua_public);
        assert_eq!(
            as_secret.public_key().to_sec1_point(false).as_bytes(),
            b64(
                "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"
            )
        );
        let auth: [u8; 16] = b64("BTBZMqHH6r4Tts7J_aSIgg").try_into().unwrap();
        let salt: [u8; 16] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();

        let body = encrypt_with(plaintext, &ua_public, &auth, &as_secret, &salt).unwrap();
        assert_eq!(
            B64.encode(&body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
        assert_eq!(decrypt(&body, &ua_secret, &auth).unwrap(), plaintext);
    }

    #[test]
    fn round_trip_with_fresh_keys() {
        let ua_secret = SecretKey::try_generate().unwrap();
        let mut auth = [0u8; 16];
        getrandom::fill(&mut auth).unwrap();
        let msg = br#"{"title":"Tornado Warning"}"#;
        let a = encrypt(msg, &ua_secret.public_key(), &auth).unwrap();
        let b = encrypt(msg, &ua_secret.public_key(), &auth).unwrap();
        assert_ne!(a, b, "salt and sender key are fresh every time");
        assert_eq!(a.len(), HEADER_LEN + msg.len() + 1 + 16);
        assert_eq!(decrypt(&a, &ua_secret, &auth).unwrap(), msg);

        let mut tampered = a.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(decrypt(&tampered, &ua_secret, &auth).is_err());
        let other = SecretKey::try_generate().unwrap();
        assert!(decrypt(&a, &other, &auth).is_err());
    }

    #[test]
    fn enforces_the_single_record_limit() {
        let ua = SecretKey::try_generate().unwrap().public_key();
        let auth = [7u8; 16];
        let max = vec![b'x'; MAX_PLAINTEXT];
        assert_eq!(
            encrypt(&max, &ua, &auth).unwrap().len(),
            RECORD_SIZE as usize
        );
        assert!(encrypt(&vec![b'x'; MAX_PLAINTEXT + 1], &ua, &auth).is_err());
    }
}
