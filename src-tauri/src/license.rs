use base64::Engine;
use ed25519_dalek::{
    Signature, Signer, Verifier, VerifyingKey, PUBLIC_KEY_LENGTH, SIGNATURE_LENGTH,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Ed25519 public key that verifies offline license files. The matching private
/// key is held by the vendor, offline, and never ships with the application.
const LICENSE_PUBLIC_KEY: [u8; PUBLIC_KEY_LENGTH] = [
    0x59, 0xfe, 0x17, 0x05, 0x24, 0x65, 0xe2, 0xa7, 0xe8, 0x75, 0x49, 0x29, 0xff, 0x0e, 0x0a, 0x46,
    0x91, 0x22, 0x87, 0x69, 0x7b, 0x25, 0x0e, 0xf7, 0x2b, 0x53, 0x45, 0x2c, 0x30, 0x38, 0xc8, 0x7b,
];

const PAYLOAD_PREFIX: &str = "STUDIOCUT-LICENSE-V1";
const MAX_LICENSE_BYTES: usize = 8192;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LicenseInfo {
    pub name: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseStatus {
    pub fingerprint: String,
    pub mode: &'static str,
    pub licensed: bool,
    pub license: Option<LicenseInfo>,
}

fn engine() -> &'static base64::engine::GeneralPurpose {
    &base64::engine::general_purpose::URL_SAFE_NO_PAD
}

/// Signed license text: base64url(payload lines) "." base64url(signature).
/// Payload lines: prefix, name, email, fingerprint, issued-at unix seconds,
/// optional expiry unix seconds.
pub fn sign_for_test(payload: &str, signing_key: &ed25519_dalek::SigningKey) -> String {
    let signature = signing_key.sign(payload.as_bytes());
    format!(
        "{}.{}",
        engine().encode(payload.as_bytes()),
        engine().encode(signature.to_bytes())
    )
}

pub fn verify_with_key(
    text: &str,
    fingerprint: &str,
    key: &VerifyingKey,
) -> Result<LicenseInfo, String> {
    let text = text.trim();
    if text.len() > MAX_LICENSE_BYTES {
        return Err("License file is too large".into());
    }
    let (payload_b64, signature_b64) = text
        .split_once('.')
        .ok_or("License is not a signed payload (expected payload.signature)")?;
    let payload = engine()
        .decode(payload_b64)
        .map_err(|_| "License payload is not valid base64")?;
    let signature_bytes = engine()
        .decode(signature_b64)
        .map_err(|_| "License signature is not valid base64")?;
    if signature_bytes.len() != SIGNATURE_LENGTH {
        return Err("License signature has the wrong length".into());
    }
    let signature = Signature::from_bytes(
        signature_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "License signature has the wrong length")?,
    );
    key.verify(&payload, &signature)
        .map_err(|_| "License signature is invalid")?;
    let payload = String::from_utf8(payload).map_err(|_| "License payload is not UTF-8")?;
    let mut lines = payload.lines();
    if lines.next() != Some(PAYLOAD_PREFIX) {
        return Err("License payload has an unknown format".into());
    }
    let mut field = |error: &'static str| -> Result<String, String> {
        match lines.next() {
            Some(line) => Ok(line.trim().to_string()),
            None => Err(error.to_string()),
        }
    };
    let _name = field("License payload is missing the licensee name")?;
    let _email = field("License payload is missing the licensee email")?;
    let bound = field("License payload is missing the machine fingerprint")?;
    if bound != fingerprint {
        return Err("This license is bound to a different machine".into());
    }
    let issued_at: u64 = field("License payload is missing the issue date")?
        .parse()
        .map_err(|_| "License issue date is invalid")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "System clock is invalid")?
        .as_secs();
    if issued_at > now + 86_400 {
        return Err("License issue date is in the future".into());
    }
    let expires_at = match field("License payload is missing the expiry field")?.as_str() {
        "-" | "" => None,
        raw => {
            let stamp: u64 = raw.parse().map_err(|_| "License expiry is invalid")?;
            if stamp <= now {
                return Err("This license has expired".into());
            }
            Some(stamp)
        }
    };
    let name = _name;
    let email = _email;
    if name.is_empty() || name.len() > 200 || email.is_empty() || email.len() > 200 {
        return Err("Licensee details are missing or too long".into());
    }
    Ok(LicenseInfo {
        name,
        email,
        expires_at,
    })
}

pub fn verify(text: &str, fingerprint: &str) -> Result<LicenseInfo, String> {
    let key = VerifyingKey::from_bytes(&LICENSE_PUBLIC_KEY)
        .expect("embedded license public key is valid");
    verify_with_key(text, fingerprint, &key)
}

fn first_line(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn command_output(mut command: std::process::Command) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut output = String::new();
    child
        .stdout
        .take()?
        .take(65_536)
        .read_to_string(&mut output)
        .ok()?;
    let _ = child.wait();
    let trimmed = output.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Stable per-installation machine identifier used to bind licenses.
fn machine_id() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Some(id) =
            first_line("/etc/machine-id").or_else(|| first_line("/var/lib/dbus/machine-id"))
        {
            return id;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(uuid) = command_output({
            let mut command = std::process::Command::new("ioreg");
            command.args(["-rd1", "-c", "IOPlatformExpertDevice"]);
            command
        })
        .and_then(|text| {
            text.lines()
                .find(|line| line.contains("IOPlatformUUID"))
                .and_then(|line| line.split('"').nth(3))
                .map(str::to_string)
        }) {
            return uuid;
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(guid) = command_output({
            let mut command = std::process::Command::new("reg");
            command.args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ]);
            command
        })
        .and_then(|text| {
            text.lines()
                .find(|line| line.contains("MachineGuid"))
                .and_then(|line| line.split_whitespace().last())
                .map(str::to_string)
        }) {
            return guid;
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .or_else(|_| first_line("/etc/hostname").ok_or(std::env::VarError::NotPresent))
        .unwrap_or_else(|_| "unknown-host".into())
}

/// Hex hardware fingerprint this license copy is bound to.
pub fn fingerprint() -> String {
    let machine_id = machine_id();
    let digest = Sha256::digest(format!("{machine_id}|studiocut-license-v1").as_bytes());
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn stored(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Debug builds run without a license (returned mode "dev"). Release builds
/// require a valid, machine-bound license file at `path`.
pub fn ensure_licensed(path: &Path) -> Result<LicenseStatus, String> {
    let fp = fingerprint();
    #[cfg(debug_assertions)]
    {
        let _ = path;
        let license = stored(path).and_then(|text| verify(&text, &fp).ok());
        Ok(LicenseStatus {
            fingerprint: fp,
            mode: "dev",
            licensed: true,
            license,
        })
    }
    #[cfg(not(debug_assertions))]
    {
        match stored(path) {
            Some(text) => verify(&text, &fp).map(|license| LicenseStatus {
                fingerprint: fp,
                mode: "licensed",
                licensed: true,
                license: Some(license),
            }),
            None => Err(format!(
                "License required. Activate a license for machine fingerprint {fp} in Settings."
            )),
        }
    }
}

pub fn activate(path: &Path, text: &str) -> Result<LicenseInfo, String> {
    let key = VerifyingKey::from_bytes(&LICENSE_PUBLIC_KEY)
        .expect("embedded license public key is valid");
    activate_with_key(path, text, &key)
}

/// Activation against an explicit verification key; used by tests and by
/// tooling that re-keys a deployment.
pub fn activate_with_key(
    path: &Path,
    text: &str,
    key: &VerifyingKey,
) -> Result<LicenseInfo, String> {
    let info = verify_with_key(text, &fingerprint(), key)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, text.trim()).map_err(|e| e.to_string())?;
    Ok(info)
}

pub fn deactivate(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) | Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn payload(fp: &str, expires: Option<u64>) -> String {
        format!(
            "{PAYLOAD_PREFIX}\nJane Doe\njane@example.com\n{fp}\n1\n{}",
            expires.map(|s| s.to_string()).unwrap_or_else(|| "-".into())
        )
    }

    #[test]
    fn fingerprint_is_stable_hex() {
        let fp = fingerprint();
        assert_eq!(fp.len(), 32);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(fp, fingerprint());
    }

    #[test]
    fn accepts_valid_machine_bound_license() {
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let fp = fingerprint();
        let text = sign_for_test(&payload(&fp, None), &key);
        let info = verify_with_key(&text, &fp, &key.verifying_key()).unwrap();
        assert_eq!(info.name, "Jane Doe");
        assert_eq!(info.email, "jane@example.com");
        assert_eq!(info.expires_at, None);
    }

    #[test]
    fn rejects_other_machine_and_tampering() {
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let text = sign_for_test(&payload("another-machine", None), &key);
        assert!(verify_with_key(&text, &fingerprint(), &key.verifying_key())
            .unwrap_err()
            .contains("different machine"));
        let good = sign_for_test(&payload(&fingerprint(), None), &key);
        let (body, sig) = good.split_once('.').unwrap();
        let payload_bytes = engine().decode(body).unwrap();
        let corrupted = String::from_utf8(payload_bytes)
            .map(|mut text| {
                text.replace_range(20..28, "Jane Evi");
                text
            })
            .unwrap_or_else(|_| payload(&fingerprint(), Some(1)));
        let tampered = format!("{}.{}", engine().encode(corrupted.as_bytes()), sig);
        assert!(verify_with_key(&tampered, &fingerprint(), &key.verifying_key()).is_err());
    }

    #[test]
    fn rejects_expired_and_garbage() {
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let vk = key.verifying_key();
        let expired = sign_for_test(&payload(&fingerprint(), Some(1)), &key);
        assert!(verify_with_key(&expired, &fingerprint(), &vk).is_err());
        assert!(verify_with_key("not-a-license", &fingerprint(), &vk).is_err());
        assert!(verify_with_key(
            &format!(
                "{}.{}",
                engine().encode(b"payload"),
                engine().encode([0u8; 64])
            ),
            &fingerprint(),
            &vk
        )
        .is_err());
    }

    #[test]
    fn embedded_key_rejects_test_signed_licenses() {
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let text = sign_for_test(&payload(&fingerprint(), None), &key);
        assert!(verify(&text, &fingerprint()).is_err());
    }

    #[cfg(debug_assertions)]
    #[test]
    fn dev_bypass_reports_unactivated_file() {
        let dir = tempfile::tempdir().unwrap();
        let status = ensure_licensed(&dir.path().join("license.txt")).unwrap();
        assert_eq!(status.mode, "dev");
        assert!(status.licensed);
    }
}
