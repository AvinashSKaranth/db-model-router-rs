//! Field encryption: key resolution, envelope errors, key rotation, row-level handling.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use dbmr_core::encryption::{
    decrypt, deserialize_value, encrypt, is_encrypted, resolve_key, serialize_value,
    EncryptionConfig, EncryptionMeta,
};
use dbmr_core::rules::compile;
use dbmr_core::types::{row, DbValue};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

fn sha(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

fn keyring(key: [u8; 32]) -> HashMap<u32, [u8; 32]> {
    HashMap::from([(1, key)])
}

#[test]
fn config_rejects_version_zero_and_empty_key() {
    let err = EncryptionConfig::new("k", 0, None).unwrap_err();
    assert_eq!(err.message(), "Encryption version must be >= 1");
    let err = EncryptionConfig::new("", 1, None).unwrap_err();
    assert_eq!(err.message(), "Empty encryption key reference");
}

#[test]
fn resolve_key_reads_env_base64_or_hashes_passphrase() {
    let missing = "DBMR_CORE_TEST_KEY_THAT_IS_NEVER_SET";
    let err = resolve_key(&format!("env:{missing}")).unwrap_err();
    assert_eq!(err.status_code(), 500);
    assert_eq!(
        err.message(),
        format!(
            "Encryption key reference \"env:{missing}\" requires env var \"{missing}\" to be set"
        )
    );

    // PATH exists on every platform; its value is hashed.
    let path = std::env::var("PATH").unwrap();
    assert_eq!(resolve_key("env:PATH").unwrap(), sha(&path));

    // `raw:` + base64 of exactly 32 bytes is used verbatim.
    let raw = [7u8; 32];
    assert_eq!(
        resolve_key(&format!("raw:{}", BASE64.encode(raw))).unwrap(),
        raw
    );
    // Wrong length, bad base64, and bare strings (secrets in source) are refused.
    assert!(resolve_key(&format!("raw:{}", BASE64.encode([1u8; 8]))).is_err());
    assert!(resolve_key("raw:not base64!").is_err());
    assert!(resolve_key(&BASE64.encode(raw)).is_err());
    let err = resolve_key("pass phrase").unwrap_err();
    assert!(err.message().contains("env:VAR_NAME"), "{}", err.message());
}

fn raw(b: u8) -> String {
    format!("raw:{}", BASE64.encode([b; 32]))
}

#[test]
fn rotated_config_decrypts_old_versions_and_encrypts_with_active() {
    let old = encrypt("legacy", &[1u8; 32], 1).unwrap();
    let history = HashMap::from([
        ("1".to_string(), raw(1)),
        ("0".to_string(), "ignored".to_string()),
        ("abc".to_string(), "ignored".to_string()),
        // The active version in history is overridden by the active key.
        ("2".to_string(), raw(3)),
    ]);
    let cfg = EncryptionConfig::new(&raw(2), 2, Some(&history)).unwrap();
    assert_eq!(cfg.keyring.len(), 2);
    assert_eq!(cfg.keyring[&2], [2u8; 32]);
    assert_eq!(decrypt(&old, &cfg.keyring).unwrap(), "legacy");

    let fresh = encrypt("now", &cfg.active_key, cfg.active_version).unwrap();
    assert!(fresh.starts_with("enc:v2:"));
    assert_eq!(decrypt(&fresh, &cfg.keyring).unwrap(), "now");

    // A bad key reference in history fails the whole config.
    let bad = HashMap::from([(
        "1".to_string(),
        "env:DBMR_CORE_TEST_KEY_THAT_IS_NEVER_SET".to_string(),
    )]);
    assert!(EncryptionConfig::new(&raw(2), 2, Some(&bad)).is_err());
}

#[test]
fn decrypt_reports_each_envelope_failure() {
    let ring = keyring(sha("k"));
    let msg = |env: &str| decrypt(env, &ring).unwrap_err().message().to_string();

    assert_eq!(msg("plain"), "Invalid envelope format: plain");
    assert_eq!(
        msg("enc:v99999999999:AAAA"),
        "Invalid version tag in envelope"
    );
    assert_eq!(msg("enc:v3:AAAA"), "No key found for envelope version v3");
    assert!(msg("enc:v1:abcde").starts_with("Base64 decode failed: "));
    assert_eq!(msg("enc:v1:AAAA"), "Corrupted ciphertext envelope");

    let other = encrypt("x", &sha("other"), 1).unwrap();
    assert_eq!(
        msg(&other),
        "GCM authentication tag mismatch (tampering detected)"
    );

    // Authentic ciphertext whose plaintext is not UTF-8.
    let iv = [0u8; 12];
    let ct = Aes256Gcm::new(&sha("k").into())
        .encrypt(Nonce::from_slice(&iv), [0xffu8, 0xfe].as_slice())
        .unwrap();
    let env = format!("enc:v1:{}", BASE64.encode([iv.as_slice(), &ct].concat()));
    assert!(is_encrypted(&env));
    assert!(msg(&env).starts_with("Decrypted bytes not valid UTF-8"));
}

#[test]
fn serialize_value_skips_null_and_envelopes_and_normalises_booleans() {
    let env = encrypt("x", &sha("k"), 1).unwrap();
    assert_eq!(serialize_value(&DbValue::Null, "string"), None);
    assert_eq!(serialize_value(&DbValue::Text(env), "string"), None);
    assert_eq!(
        serialize_value(&DbValue::Bool(true), "boolean").as_deref(),
        Some("true")
    );
    assert_eq!(
        serialize_value(&DbValue::Int(1), "boolean").as_deref(),
        Some("true")
    );
    assert_eq!(
        serialize_value(&DbValue::Int(2), "boolean").as_deref(),
        Some("false")
    );
    assert_eq!(
        serialize_value(&" TRUE ".into(), "boolean").as_deref(),
        Some("true")
    );
    assert_eq!(
        serialize_value(&"no".into(), "boolean").as_deref(),
        Some("false")
    );
    assert_eq!(
        serialize_value(&DbValue::Float(1.0), "boolean").as_deref(),
        Some("false")
    );
    assert_eq!(
        serialize_value(&DbValue::Json(json!({"a": 1})), "string").as_deref(),
        Some(r#"{"a":1}"#)
    );
    assert_eq!(
        serialize_value(&DbValue::Int(42), "integer").as_deref(),
        Some("42")
    );
}

#[test]
fn deserialize_value_restores_types_and_falls_back_to_text() {
    assert_eq!(
        deserialize_value("1".into(), "boolean"),
        DbValue::Bool(true)
    );
    assert_eq!(
        deserialize_value("false".into(), "boolean"),
        DbValue::Bool(false)
    );
    assert_eq!(deserialize_value(" 7 ".into(), "integer"), DbValue::Int(7));
    assert_eq!(
        deserialize_value("x".into(), "integer"),
        DbValue::Text("x".into())
    );
    assert_eq!(
        deserialize_value("1.5".into(), "numeric"),
        DbValue::Float(1.5)
    );
    assert_eq!(
        deserialize_value("n/a".into(), "numeric"),
        DbValue::Text("n/a".into())
    );
    assert_eq!(
        deserialize_value("s".into(), "datetime"),
        DbValue::Text("s".into())
    );
}

fn meta() -> EncryptionMeta {
    let rules = [
        compile("ssn", "encrypted|string").unwrap(),
        compile("score", "encrypted|numeric").unwrap(),
        compile("profile.dob", "encrypted|string").unwrap(),
        compile("name", "string").unwrap(),
    ];
    EncryptionMeta::from_rules(&rules)
}

#[test]
fn meta_collects_columns_and_nested_json_keys() {
    let m = meta();
    assert!(!m.is_empty());
    assert_eq!(
        m.columns,
        [
            ("ssn".to_string(), "string".to_string()),
            ("score".into(), "numeric".into())
        ]
    );
    assert_eq!(
        m.json_keys,
        [(
            "profile".to_string(),
            vec!["dob".to_string()],
            "string".to_string()
        )]
    );
    assert!(EncryptionMeta::from_rules(&[compile("a", "string").unwrap()]).is_empty());
}

#[test]
fn encrypt_row_handles_text_json_and_skips_missing_or_non_object_parents() {
    let cfg =
        EncryptionConfig::new("raw:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", 1, None).unwrap();
    let m = meta();

    // Parent stored as a JSON string is parsed, encrypted and stored as JSON.
    let mut r = row([
        ("ssn", DbValue::Null),
        ("score", DbValue::Float(2.5)),
        ("profile", r#"{"dob":"2000-01-01","x":1}"#.into()),
    ]);
    m.encrypt_row(&cfg, &mut r).unwrap();
    assert_eq!(r["ssn"], DbValue::Null);
    assert!(is_encrypted(r["score"].as_str().unwrap()));
    let DbValue::Json(p) = &r["profile"] else {
        panic!("profile not json: {:?}", r["profile"])
    };
    assert!(is_encrypted(p["dob"].as_str().unwrap()));
    assert_eq!(p["x"], 1);

    // Encrypting twice is a no-op for already enveloped values.
    let before = r.clone();
    m.encrypt_row(&cfg, &mut r).unwrap();
    assert_eq!(r, before);

    m.decrypt_row(&cfg, &mut r).unwrap();
    assert_eq!(r["score"], DbValue::Float(2.5));
    assert_eq!(
        r["profile"],
        DbValue::Json(json!({"dob": "2000-01-01", "x": 1}))
    );

    // Missing parent, non-object parent, missing leaf, null leaf: untouched.
    for profile in [
        None,
        Some(DbValue::Int(3)),
        Some("not json".into()),
        Some("[1]".into()),
        Some(DbValue::Json(json!({"other": 1}))),
        Some(DbValue::Json(json!({"dob": null}))),
    ] {
        let mut r = row([("name", "n".into())]);
        if let Some(p) = profile.clone() {
            r.insert("profile".into(), p);
        }
        let before = r.clone();
        m.encrypt_row(&cfg, &mut r).unwrap();
        m.decrypt_row(&cfg, &mut r).unwrap();
        assert_eq!(r, before, "{profile:?}");
    }
}

#[test]
fn decrypt_row_passes_legacy_plaintext_and_fails_on_unknown_key_version() {
    let cfg =
        EncryptionConfig::new("raw:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", 1, None).unwrap();
    let m = meta();
    let mut r = row([
        ("ssn", "plain-ssn".into()),
        ("score", DbValue::Int(5)),
        (
            "profile",
            DbValue::Json(json!({"dob": "plain", "n": {"deep": 1}})),
        ),
    ]);
    let before = r.clone();
    m.decrypt_row(&cfg, &mut r).unwrap();
    assert_eq!(r, before);

    let foreign = encrypt("x", &sha("k"), 9).unwrap();
    let mut r = row([("ssn", DbValue::Text(foreign.clone()))]);
    let err = m.decrypt_row(&cfg, &mut r).unwrap_err();
    assert_eq!(err.message(), "No key found for envelope version v9");

    let mut r = row([("profile", DbValue::Json(json!({ "dob": foreign })))]);
    let err = m.decrypt_row(&cfg, &mut r).unwrap_err();
    assert_eq!(err.message(), "No key found for envelope version v9");
}
