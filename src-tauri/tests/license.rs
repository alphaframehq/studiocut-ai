use ed25519_dalek::Signer;
use studiocut_ai::license;

fn payload(fingerprint: &str, expires: Option<u64>) -> String {
    format!(
        "STUDIOCUT-LICENSE-V1\nJane Doe\njane@example.com\n{fingerprint}\n1\n{}",
        expires.map(|s| s.to_string()).unwrap_or_else(|| "-".into())
    )
}

fn signing_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[0x5d; 32])
}

fn base64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn sign(key: &ed25519_dalek::SigningKey, text: &str) -> String {
    let signature = key.sign(text.as_bytes());
    format!(
        "{}.{}",
        base64(text.as_bytes()),
        base64(&signature.to_bytes())
    )
}

fn reason(error: &str) -> String {
    if error.contains("different machine") {
        "reject: different machine".into()
    } else if error.contains("expired") {
        "reject: expired".into()
    } else {
        format!("reject: {}", error.chars().take(30).collect::<String>())
    }
}

/// Evaluation cases over real license artifacts written to disk.
/// (name, fingerprint bound into the license, expiry, fingerprint at verification, expected)
#[test]
fn license_activation_evaluation_set() {
    let key = signing_key();
    let real_fp = license::fingerprint();
    let foreign_fp = "deadbeefdeadbeefdeadbeefdeadbeef";
    let cases = [
        (
            "matching-machine-perpetual",
            real_fp.clone(),
            None,
            real_fp.clone(),
            "accept",
        ),
        (
            "matching-machine-with-expiry",
            real_fp.clone(),
            Some(u64::MAX - 1),
            real_fp.clone(),
            "accept",
        ),
        (
            "other-machine",
            foreign_fp.into(),
            None,
            real_fp.clone(),
            "reject: different machine",
        ),
        (
            "expired",
            real_fp.clone(),
            Some(1),
            real_fp.clone(),
            "reject: expired",
        ),
        (
            "unknown-fingerprint",
            real_fp.clone(),
            None,
            foreign_fp.into(),
            "reject: different machine",
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    let mut passed = 0usize;

    for (name, bound_fp, expires, verify_fp, expected) in &cases {
        let text = sign(&key, &payload(&bound_fp, *expires));
        let license_path = dir.path().join(format!("{name}.license.txt"));
        if *expected == "accept" {
            let info = license::activate_with_key(&license_path, &text, &key.verifying_key())
                .unwrap_or_else(|e| panic!("{name}: activate failed: {e}"));
            assert_eq!(info.name, "Jane Doe", "{name}");
            assert_eq!(info.email, "jane@example.com", "{name}");
            assert_eq!(info.expires_at, *expires, "{name}");
            assert!(
                license_path.is_file(),
                "{name}: license file must exist on disk"
            );
            let stored = std::fs::read_to_string(&license_path).unwrap();
            assert_eq!(
                stored.trim(),
                text.trim(),
                "{name}: file holds the exact license"
            );
            // Round-trip: read the artifact back and verify it with the embedded vendor key.
            let read_back = std::fs::read_to_string(&license_path).unwrap();
            let info2 =
                license::verify_with_key(read_back.trim(), &verify_fp, &key.verifying_key())
                    .unwrap_or_else(|e| panic!("{name}: round-trip verify failed: {e}"));
            assert_eq!(info2, info, "{name}: round-trip info matches");
        } else {
            let outcome = license::verify_with_key(&text, &verify_fp, &key.verifying_key())
                .map(|_| "accept".to_string())
                .unwrap_or_else(|e| reason(&e));
            assert_eq!(outcome, *expected, "{name}");
            // A rejected license must never be stored by the activation path.
            // On this machine, activation must refuse to store a license this machine rejects.
            if *verify_fp == real_fp {
                assert!(
                    license::activate_with_key(&license_path, &text, &key.verifying_key()).is_err(),
                    "{name}: rejected license was stored"
                );
                assert!(
                    !license_path.exists(),
                    "{name}: rejected license left a file behind"
                );
            }
        }
        passed += 1;
    }
    println!(
        "license evaluation set: {passed}/{} cases passed",
        cases.len()
    );
}
