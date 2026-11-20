use crate::core::error::{AppError, Result};
use crate::core::types::{DbValue, RowMap};
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rand::RngCore;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::LazyLock;

const IV_LENGTH: usize = 12;
const TAG_LENGTH: usize = 16;
const ENVELOPE_PREFIX: &str = "enc:";

static ENVELOPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)]
    Regex::new(r"^enc:v(\d+):([A-Za-z0-9+/]+={0,2})$").unwrap()
});

#[derive(Clone)]
pub struct EncryptionConfig {
    pub active_version: u32,
    pub active_key: [u8; 32],
    pub keyring: HashMap<u32, [u8; 32]>,
}

impl std::fmt::Debug for EncryptionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionConfig")
            .field("active_version", &self.active_version)
            .field("active_key", &"[REDACTED]")
            .field("keyring_versions", &self.keyring.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl EncryptionConfig {
    /// Builds encryption keyring from active key and version history.
    ///
    /// # Errors
    /// Returns [`AppError::Encryption`] if key resolution fails or version < 1.
    pub fn new(
        active_ref: &str,
        active_version: u32,
        history: Option<&HashMap<String, String>>,
    ) -> Result<Self> {
        if active_version < 1 {
            return Err(AppError::Encryption(
                "Encryption version must be >= 1".to_string(),
            ));
        }
        let active_key = resolve_key(active_ref)?;
        let mut keyring = HashMap::new();
        keyring.insert(active_version, active_key);

        if let Some(hist) = history {
            for (v_str, ref_str) in hist {
                if let Ok(v) = v_str.parse::<u32>() {
                    if v >= 1 {
                        keyring.insert(v, resolve_key(ref_str)?);
                    }
                }
            }
        }
        // Active key is authoritative
        keyring.insert(active_version, active_key);

        Ok(Self {
            active_version,
            active_key,
            keyring,
        })
    }
}

/// Resolves an encryption key reference into a 32-byte key: `env:VAR` (SHA-256 of the
/// variable's value) or `raw:<base64 of exactly 32 bytes>`.
///
/// # Errors
/// [`AppError::Encryption`] for any other form, a missing/empty env var, or a bad raw key.
pub fn resolve_key(ref_str: &str) -> Result<[u8; 32]> {
    if ref_str.is_empty() {
        return Err(AppError::Encryption(
            "Empty encryption key reference".to_string(),
        ));
    }
    if let Some(var_name) = ref_str.strip_prefix("env:") {
        let val = std::env::var(var_name).map_err(|_| {
            AppError::Encryption(format!(
                "Encryption key reference \"{ref_str}\" requires env var \"{var_name}\" to be set"
            ))
        })?;
        if val.is_empty() {
            return Err(AppError::Encryption(format!(
                "Encryption key env var \"{var_name}\" is empty"
            )));
        }
        let hash = Sha256::digest(val.as_bytes());
        let mut key = [0u8; 32];
        key.copy_from_slice(&hash);
        return Ok(key);
    }
    if let Some(b64) = ref_str.strip_prefix("raw:") {
        let decoded = BASE64
            .decode(b64)
            .map_err(|_| AppError::Encryption("\"raw:\" key must be base64".to_string()))?;
        return <[u8; 32]>::try_from(decoded.as_slice()).map_err(|_| {
            AppError::Encryption("\"raw:\" key must decode to exactly 32 bytes".to_string())
        });
    }
    // A bare string used to become the key itself, which put secrets in source code.
    Err(AppError::Encryption(
        "Encryption key reference must be \"env:VAR_NAME\" or \"raw:<base64 32-byte key>\""
            .to_string(),
    ))
}

#[must_use]
pub fn is_encrypted(val: &str) -> bool {
    val.starts_with(ENVELOPE_PREFIX) && ENVELOPE_RE.is_match(val)
}

/// Encrypts plaintext string into envelope `enc:vN:<base64>`.
///
/// # Errors
/// Returns [`AppError::Encryption`] if cipher fails.
pub fn encrypt(plaintext: &str, key: &[u8; 32], version: u32) -> Result<String> {
    let mut iv = [0u8; IV_LENGTH];
    rand::thread_rng().fill_bytes(&mut iv);

    let cipher = Aes256Gcm::new(key.into());
    let nonce = Nonce::from_slice(&iv);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| AppError::Encryption(format!("GCM encryption error: {e:?}")))?;

    let mut payload = Vec::with_capacity(IV_LENGTH + ciphertext.len());
    payload.extend_from_slice(&iv);
    payload.extend_from_slice(&ciphertext);

    let b64 = BASE64.encode(&payload);
    Ok(format!("{ENVELOPE_PREFIX}v{version}:{b64}"))
}

/// Decrypts envelope `enc:vN:<base64>` back to plaintext.
///
/// # Errors
/// Returns [`AppError::Encryption`] if authentication tag mismatches or key is missing.
pub fn decrypt(envelope: &str, keyring: &HashMap<u32, [u8; 32]>) -> Result<String> {
    let Some(caps) = ENVELOPE_RE.captures(envelope) else {
        return Err(AppError::Encryption(format!(
            "Invalid envelope format: {envelope}"
        )));
    };

    let version: u32 = caps[1]
        .parse()
        .map_err(|_| AppError::Encryption("Invalid version tag in envelope".to_string()))?;

    let key = keyring.get(&version).ok_or_else(|| {
        AppError::Encryption(format!("No key found for envelope version v{version}"))
    })?;

    let raw = BASE64
        .decode(&caps[2])
        .map_err(|e| AppError::Encryption(format!("Base64 decode failed: {e}")))?;

    if raw.len() < IV_LENGTH + TAG_LENGTH {
        return Err(AppError::Encryption(
            "Corrupted ciphertext envelope".to_string(),
        ));
    }

    let iv = &raw[..IV_LENGTH];
    let ciphertext = &raw[IV_LENGTH..];

    let cipher = Aes256Gcm::new(key.into());
    let nonce = Nonce::from_slice(iv);

    let plaintext_bytes = cipher.decrypt(nonce, ciphertext).map_err(|_| {
        AppError::Encryption("GCM authentication tag mismatch (tampering detected)".to_string())
    })?;

    String::from_utf8(plaintext_bytes)
        .map_err(|e| AppError::Encryption(format!("Decrypted bytes not valid UTF-8: {e}")))
}

/// Which columns (and nested JSON keys, `"profile.dob"`) are encrypted, with their base type.
#[derive(Debug, Clone, Default)]
pub struct EncryptionMeta {
    pub columns: Vec<(String, String)>,
    /// (parent column, path inside the JSON value, base type)
    pub json_keys: Vec<(String, Vec<String>, String)>,
}

impl EncryptionMeta {
    #[must_use]
    pub fn from_rules<'a>(
        rules: impl IntoIterator<Item = &'a crate::core::rules::ColumnRules>,
    ) -> Self {
        let mut meta = Self::default();
        for r in rules.into_iter().filter(|r| r.encrypted) {
            let ty = r.base_type.clone().unwrap_or_default();
            match r.column.split_once('.') {
                Some((parent, rest)) => meta.json_keys.push((
                    parent.to_string(),
                    rest.split('.').map(str::to_string).collect(),
                    ty,
                )),
                None => meta.columns.push((r.column.clone(), ty)),
            }
        }
        meta
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.json_keys.is_empty()
    }

    /// Encrypt configured fields in place. Null and already-enveloped values are skipped.
    ///
    /// # Errors
    /// [`AppError::Encryption`] on cipher failure.
    pub fn encrypt_row(&self, cfg: &EncryptionConfig, row: &mut RowMap) -> Result<()> {
        for (col, ty) in &self.columns {
            if let Some(v) = row.get_mut(col.as_str()) {
                if let Some(plain) = serialize_value(v, ty) {
                    *v = DbValue::Text(encrypt(&plain, &cfg.active_key, cfg.active_version)?);
                }
            }
        }
        for (parent, path, ty) in &self.json_keys {
            let Some(slot) = row.get_mut(parent.as_str()) else {
                continue;
            };
            let Some(mut obj) = json_object(slot) else {
                continue;
            };
            let Some(leaf) = json_path_mut(&mut obj, path) else {
                continue;
            };
            if let Some(plain) = serialize_value(&DbValue::from(leaf.clone()), ty) {
                *leaf = Value::String(encrypt(&plain, &cfg.active_key, cfg.active_version)?);
                *slot = DbValue::Json(obj);
            }
        }
        Ok(())
    }

    /// Decrypt configured fields in place; legacy plaintext passes through.
    ///
    /// # Errors
    /// [`AppError::Encryption`] on tampering or a missing key version.
    pub fn decrypt_row(&self, cfg: &EncryptionConfig, row: &mut RowMap) -> Result<()> {
        for (col, ty) in &self.columns {
            if let Some(v) = row.get_mut(col.as_str()) {
                if let DbValue::Text(s) = v {
                    if is_encrypted(s) {
                        *v = deserialize_value(decrypt(s, &cfg.keyring)?, ty);
                    }
                }
            }
        }
        for (parent, path, ty) in &self.json_keys {
            let Some(slot) = row.get_mut(parent.as_str()) else {
                continue;
            };
            let Some(mut obj) = json_object(slot) else {
                continue;
            };
            let Some(Value::String(s)) = json_path_mut(&mut obj, path).map(|l| l.clone()) else {
                continue;
            };
            if is_encrypted(&s) {
                if let Some(leaf) = json_path_mut(&mut obj, path) {
                    *leaf = deserialize_value(decrypt(&s, &cfg.keyring)?, ty).to_json();
                }
                *slot = DbValue::Json(obj);
            }
        }
        Ok(())
    }
}

/// A JSON-object column value, whether stored as JSON or as a JSON string.
fn json_object(v: &DbValue) -> Option<Value> {
    match v {
        DbValue::Json(j @ Value::Object(_)) => Some(j.clone()),
        DbValue::Text(t) => match serde_json::from_str::<Value>(t) {
            Ok(j @ Value::Object(_)) => Some(j),
            _ => None,
        },
        _ => None,
    }
}

fn json_path_mut<'a>(v: &'a mut Value, path: &[String]) -> Option<&'a mut Value> {
    path.iter().try_fold(v, |cur, k| cur.get_mut(k.as_str()))
}

/// Node `serializeValue`: typed value to the plaintext that gets encrypted.
/// Returns `None` for null or already-encrypted values (skip).
#[must_use]
pub fn serialize_value(v: &DbValue, ty: &str) -> Option<String> {
    match v {
        DbValue::Null => None,
        DbValue::Text(s) if is_encrypted(s) => None,
        _ if ty == "boolean" => Some(
            match v {
                DbValue::Bool(b) => *b,
                DbValue::Int(i) => *i == 1,
                DbValue::Text(s) => matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1"),
                _ => false,
            }
            .to_string(),
        ),
        DbValue::Json(j) => Some(j.to_string()),
        other => Some(other.to_string()),
    }
}

/// Node `deserializeValue`: decrypted plaintext back to its typed value.
#[must_use]
pub fn deserialize_value(s: String, ty: &str) -> DbValue {
    match ty {
        "boolean" => DbValue::Bool(s == "true" || s == "1"),
        "integer" => s
            .trim()
            .parse::<i128>()
            .map_or(DbValue::Text(s), DbValue::Int),
        "numeric" => s
            .trim()
            .parse::<f64>()
            .map_or(DbValue::Text(s), DbValue::Float),
        _ => DbValue::Text(s),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_encryption_roundtrip() {
        let key = [42u8; 32];
        let mut keyring = HashMap::new();
        keyring.insert(1, key);

        let secret = "top-secret-password-123!";
        let env = encrypt(secret, &key, 1).unwrap();
        assert!(is_encrypted(&env));

        let decrypted = decrypt(&env, &keyring).unwrap();
        assert_eq!(decrypted, secret);
    }

    #[test]
    fn test_tamper_detection() {
        let key = [0u8; 32];
        let mut keyring = HashMap::new();
        keyring.insert(1, key);

        let env = encrypt("sensitive", &key, 1).unwrap();
        // Tamper with the base64 payload
        let mut bytes = env.into_bytes();
        let len = bytes.len();
        bytes[len - 2] = b'X';
        let tampered = String::from_utf8(bytes).unwrap();

        let err = decrypt(&tampered, &keyring);
        assert!(err.is_err());
    }

    #[test]
    fn typed_and_nested_round_trip() {
        use crate::core::types::{json_to_row, row_to_json};
        let rules = [
            crate::core::rules::compile("ssn", "encrypted|string").unwrap(),
            crate::core::rules::compile("age", "encrypted|integer").unwrap(),
            crate::core::rules::compile("ok", "encrypted|boolean").unwrap(),
            crate::core::rules::compile("profile.dob", "encrypted|datetime").unwrap(),
        ];
        let meta = EncryptionMeta::from_rules(&rules);
        let cfg =
            EncryptionConfig::new("raw:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", 1, None)
                .unwrap();
        let original = serde_json::json!({"ssn":"123","age":42,"ok":"1","profile":{"dob":"2000-01-01 00:00:00","x":1}});
        let mut row = json_to_row(&original);
        meta.encrypt_row(&cfg, &mut row).unwrap();
        let stored = row_to_json(&row);
        assert!(is_encrypted(stored["ssn"].as_str().unwrap()));
        assert!(is_encrypted(stored["age"].as_str().unwrap()));
        assert!(is_encrypted(stored["profile"]["dob"].as_str().unwrap()));
        assert_eq!(stored["profile"]["x"], 1);
        meta.decrypt_row(&cfg, &mut row).unwrap();
        let back = row_to_json(&row);
        assert_eq!(back["ssn"], "123");
        assert_eq!(back["age"], 42);
        assert_eq!(back["ok"], true);
        assert_eq!(back["profile"]["dob"], "2000-01-01 00:00:00");
    }

    #[test]
    fn test_debug_redaction() {
        let cfg =
            EncryptionConfig::new("raw:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", 1, None)
                .unwrap();
        let debug_str = format!("{cfg:?}");
        assert!(debug_str.contains("[REDACTED]"));
        assert!(!debug_str.contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
    }
}
