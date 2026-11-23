//! Encryption at rest (AES-256-GCM) for aggregator tokens and TOTP secrets,
//! plus the small hashing helpers used for sessions and TOTP.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha1::Sha1;
use sha2::{Digest, Sha256};

pub fn encrypt(key: &[u8; 32], plaintext: &str) -> Result<String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| anyhow!("bad key: {e}"))?;
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
        .map_err(|_| anyhow!("encryption failed"))?;
    let mut out = nonce.to_vec();
    out.extend(ct);
    Ok(B64.encode(out))
}

pub fn decrypt(key: &[u8; 32], encoded: &str) -> Result<String> {
    let raw = B64.decode(encoded)?;
    if raw.len() < 13 {
        return Err(anyhow!("ciphertext too short"));
    }
    let (nonce, ct) = raw.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| anyhow!("bad key: {e}"))?;
    let pt = cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| anyhow!("decryption failed (wrong APP_ENCRYPTION_KEY?)"))?;
    Ok(String::from_utf8(pt)?)
}

pub fn random_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    data_encoding::BASE64URL_NOPAD.encode(&b)
}

pub fn sha256_hex(s: &str) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(s.as_bytes()))
}

/// New random TOTP secret, base32 (the form authenticator apps expect).
pub fn new_totp_secret() -> String {
    let mut b = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut b);
    data_encoding::BASE32_NOPAD.encode(&b)
}

/// RFC 6238 code (SHA-1, 6 digits, 30 s) for a given time step.
pub fn totp_at(secret_b32: &str, step: u64) -> Result<String> {
    let key = data_encoding::BASE32_NOPAD
        .decode(secret_b32.trim().to_ascii_uppercase().as_bytes())
        .map_err(|_| anyhow!("bad totp secret"))?;
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&key).map_err(|_| anyhow!("bad totp key"))?;
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[19] & 0x0f) as usize;
    let bin = ((digest[offset] as u32 & 0x7f) << 24)
        | ((digest[offset + 1] as u32) << 16)
        | ((digest[offset + 2] as u32) << 8)
        | digest[offset + 3] as u32;
    Ok(format!("{:06}", bin % 1_000_000))
}

/// Checks `code` against the current step ±1. Returns the matching step so the
/// caller can reject replays of the same code.
pub fn totp_verify(secret_b32: &str, code: &str, unix_time: u64) -> Option<u64> {
    let code = code.trim();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let now = unix_time / 30;
    [now.saturating_sub(1), now, now + 1].into_iter().find(|&step| {
        totp_at(secret_b32, step)
            .map(|expected| constant_time_eq(expected.as_bytes(), code.as_bytes()))
            .unwrap_or(false)
    })
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let key = [7u8; 32];
        let enc = encrypt(&key, "access-sandbox-123").unwrap();
        assert_ne!(enc, "access-sandbox-123");
        assert_eq!(decrypt(&key, &enc).unwrap(), "access-sandbox-123");
        assert!(decrypt(&[8u8; 32], &enc).is_err());
    }

    #[test]
    fn totp_rfc6238_vector() {
        // RFC 6238 appendix B, SHA-1, secret "12345678901234567890", T = 59 s.
        let secret = data_encoding::BASE32_NOPAD.encode(b"12345678901234567890");
        assert_eq!(totp_at(&secret, 59 / 30).unwrap(), "287082");
        assert_eq!(totp_verify(&secret, "287082", 59), Some(1));
        assert_eq!(totp_verify(&secret, "000000", 59), None);
    }
}
