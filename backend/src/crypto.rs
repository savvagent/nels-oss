//! Application-layer encryption for secrets stored at rest. Originally built
//! for the TOTP secret (issue #15, superseded by nels#551's passkey
//! conversion — see docs/superpowers/specs/2026-06-11-encrypt-totp-secrets-design.md
//! for that history); today it encrypts bank-provider OAuth access tokens
//! (`akahu.rs`, `plaid.rs`) before they touch Postgres, so a DB dump/backup
//! yields no usable credential.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use rand::RngCore;

/// Version marker prefixed to every stored ciphertext. Lets us recognise
/// encrypted values and gives us a rotation handle later (we only emit/accept
/// v1 today).
const PREFIX: &str = "encv1:";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

#[derive(Debug)]
pub struct CryptoError(pub String);

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "crypto error: {}", self.0)
    }
}

impl std::error::Error for CryptoError {}

/// Symmetric cipher for stored secrets. Holds the AES-256-GCM key in memory;
/// the key itself is never written to the database.
#[derive(Clone)]
pub struct SecretCipher {
    cipher: Aes256Gcm,
}

impl SecretCipher {
    /// Build a cipher from raw key bytes. The key MUST be exactly 32 bytes.
    pub fn new(key_bytes: &[u8]) -> Result<Self, CryptoError> {
        if key_bytes.len() != KEY_LEN {
            return Err(CryptoError(format!(
                "key must be {} bytes, got {}",
                KEY_LEN,
                key_bytes.len()
            )));
        }
        let key = Key::<Aes256Gcm>::from_slice(key_bytes);
        Ok(Self {
            cipher: Aes256Gcm::new(key),
        })
    }

    /// Read `TOTP_ENC_KEY` (base64 of exactly 32 bytes) from the environment
    /// and build a cipher. Returns Err if the var is missing, not valid
    /// base64, or not 32 bytes — the caller (main) aborts startup on Err.
    pub fn from_env() -> Result<Self, CryptoError> {
        let encoded = std::env::var("TOTP_ENC_KEY")
            .map_err(|_| CryptoError("TOTP_ENC_KEY is not set".to_string()))?;
        let key_bytes = STANDARD
            .decode(encoded.trim())
            .map_err(|e| CryptoError(format!("TOTP_ENC_KEY is not valid base64: {e}")))?;
        Self::new(&key_bytes)
    }

    /// Encrypt a plaintext secret. Output: `encv1:` + base64(nonce ‖ ct+tag).
    /// A fresh random 96-bit nonce is used per call.
    pub fn encrypt(&self, plaintext: &str) -> Result<String, CryptoError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| CryptoError(format!("encryption failed: {e}")))?;

        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);

        Ok(format!("{PREFIX}{}", STANDARD.encode(&blob)))
    }

    /// Decrypt a value produced by `encrypt`. Errors on wrong prefix/version,
    /// bad base64, truncated blob, or failed authentication (tamper / wrong key).
    pub fn decrypt(&self, stored: &str) -> Result<String, CryptoError> {
        let b64 = stored
            .strip_prefix(PREFIX)
            .ok_or_else(|| CryptoError("missing/unknown version prefix".to_string()))?;
        self.decrypt_v1_body(b64)
    }

    /// Decrypt the base64 body of an `encv1:` value (prefix already stripped).
    fn decrypt_v1_body(&self, b64: &str) -> Result<String, CryptoError> {
        let blob = STANDARD
            .decode(b64)
            .map_err(|e| CryptoError(format!("invalid base64: {e}")))?;

        if blob.len() <= NONCE_LEN {
            return Err(CryptoError("ciphertext too short".to_string()));
        }

        let (nonce_bytes, ciphertext) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| CryptoError("decryption/authentication failed".to_string()))?;

        String::from_utf8(plaintext)
            .map_err(|e| CryptoError(format!("decrypted bytes are not utf8: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A fixed 32-byte test key. Never used outside tests.
    fn test_cipher() -> SecretCipher {
        SecretCipher::new(&[7u8; KEY_LEN]).unwrap()
    }

    #[test]
    fn round_trip_returns_original() {
        let c = test_cipher();
        let secret = "JBSWY3DPEHPK3PXP";
        let stored = c.encrypt(secret).unwrap();
        assert_eq!(c.decrypt(&stored).unwrap(), secret);
    }

    #[test]
    fn ciphertext_is_not_plaintext_and_has_prefix() {
        let c = test_cipher();
        let secret = "JBSWY3DPEHPK3PXP";
        let stored = c.encrypt(secret).unwrap();
        assert!(stored.starts_with("encv1:"));
        assert!(!stored.contains(secret));
    }

    #[test]
    fn same_input_encrypts_differently() {
        let c = test_cipher();
        let a = c.encrypt("JBSWY3DPEHPK3PXP").unwrap();
        let b = c.encrypt("JBSWY3DPEHPK3PXP").unwrap();
        assert_ne!(a, b, "nonce reuse: identical ciphertext for identical input");
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let c = test_cipher();
        let stored = c.encrypt("JBSWY3DPEHPK3PXP").unwrap();
        // Flip the last base64 char to corrupt the auth tag.
        let mut chars: Vec<char> = stored.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert!(c.decrypt(&tampered).is_err());
    }

    #[test]
    fn wrong_format_is_rejected() {
        let c = test_cipher();
        assert!(c.decrypt("JBSWY3DPEHPK3PXP").is_err()); // no prefix (legacy plaintext)
        assert!(c.decrypt("encv2:abc").is_err()); // unknown version prefix
        assert!(c.decrypt("encv1:not!base64!").is_err()); // bad base64
    }

    #[test]
    fn wrong_key_cannot_decrypt() {
        let a = SecretCipher::new(&[1u8; KEY_LEN]).unwrap();
        let b = SecretCipher::new(&[2u8; KEY_LEN]).unwrap();
        let stored = a.encrypt("JBSWY3DPEHPK3PXP").unwrap();
        assert!(b.decrypt(&stored).is_err());
    }

    #[test]
    fn key_must_be_32_bytes() {
        assert!(SecretCipher::new(&[0u8; 16]).is_err());
        assert!(SecretCipher::new(&[0u8; 31]).is_err());
        assert!(SecretCipher::new(&[0u8; 33]).is_err());
        assert!(SecretCipher::new(&[0u8; 32]).is_ok());
    }
}
