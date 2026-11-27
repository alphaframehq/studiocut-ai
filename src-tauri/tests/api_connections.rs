//! API connections integration tests: pure unit tests for the public
//! API in `studiocut_ai::api_connections`. No network calls, no OS
//! credential store access (session-only credentials only), and no
//! live calls to any third-party API.
//!
//! The same functions are also covered by the `#[cfg(test)]` module
//! inside `src/api_connections.rs`, which runs with
//! `cargo test --lib` without any manifest change. This file runs in
//! CI through the `[[test]]` entry in `src-tauri/Cargo.toml`.

use serde_json::json;
use std::path::PathBuf;
use studiocut_ai::api_connections::{
    add_api_connection_record, delete_api_connection_record, load_connections, models_url,
    save_connections, update_api_connection_record, validate_auth_header, validate_connection_id,
    validate_connection_url, ApiConnection, ApiConnectionInput, ConnectionAuthType, ConnectionKind,
    KeyStorage, API_CONNECTIONS_FILE_NAME,
};
use studiocut_ai::director::filter_director_models;
use studiocut_ai::mcp::McpCredentials;

fn input(
    name: &str,
    base_url: &str,
    kind: ConnectionKind,
    auth_type: ConnectionAuthType,
) -> ApiConnectionInput {
    ApiConnectionInput {
        name: name.into(),
        base_url: base_url.into(),
        kind,
        auth_type,
        auth_header: Some("Authorization".into()),
        key: Some("test-only-key".into()),
        key_storage: KeyStorage::Session,
    }
}

fn temp_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(API_CONNECTIONS_FILE_NAME);
    (dir, path)
}

#[test]
fn url_validation_rejects_credentials_query_fragment_and_remote_http() {
    assert!(validate_connection_url("https://api.example.com/v1").is_ok());
    assert!(validate_connection_url("http://localhost:8080/v1").is_ok());
    assert!(validate_connection_url("http://127.0.0.1:11434/v1").is_ok());
    assert!(validate_connection_url("http://[::1]:8080/v1").is_ok());
    assert!(validate_connection_url("http://api.example.com/v1").is_err());
    assert!(validate_connection_url("https://user:pass@api.example.com/v1").is_err());
    assert!(validate_connection_url("https://api.example.com/v1?api_key=x").is_err());
    assert!(validate_connection_url("https://api.example.com/v1#frag").is_err());
    assert!(validate_connection_url("").is_err());
    assert!(
        validate_connection_url(&format!("https://api.example.com/{}", "a".repeat(2030))).is_err()
    );
}

#[test]
fn models_url_handles_chat_suffix_and_trailing_slash() {
    assert_eq!(
        models_url("https://host.example/v1"),
        "https://host.example/v1/models"
    );
    assert_eq!(
        models_url("https://host.example/v1/"),
        "https://host.example/v1/models"
    );
    assert_eq!(
        models_url("https://host.example/v1/chat/completions"),
        "https://host.example/v1/models"
    );
    assert_eq!(
        models_url("https://host.example/v1/models"),
        "https://host.example/v1/models"
    );
}

#[test]
fn auth_header_defaults_to_authorization() {
    assert_eq!(validate_auth_header("").unwrap(), "Authorization");
    assert_eq!(validate_auth_header("x-api-key").unwrap(), "x-api-key");
    assert!(validate_auth_header("Bad Header").is_err());
    assert!(validate_connection_id(&uuid::Uuid::new_v4().to_string()).is_ok());
    assert!(validate_connection_id("server-1").is_err());
}

#[test]
fn file_store_round_trip_without_keys() {
    let (_dir, path) = temp_path();
    assert!(load_connections(&path).unwrap().is_empty());
    let record = ApiConnection {
        id: uuid::Uuid::new_v4().to_string(),
        name: "My API".into(),
        base_url: "https://host.example/v1".into(),
        kind: ConnectionKind::OpenaiCompatible,
        auth_type: ConnectionAuthType::ApiKey,
        auth_header: Some("Authorization".into()),
        key_storage: KeyStorage::Session,
    };
    save_connections(&path, &[record.clone()]).unwrap();
    assert_eq!(load_connections(&path).unwrap(), vec![record]);
    std::fs::write(&path, "not json").unwrap();
    assert!(load_connections(&path).is_err());
}

#[test]
fn add_update_delete_keep_keys_out_of_records() {
    let credentials = McpCredentials::default();
    let (_dir, path) = temp_path();
    let added = add_api_connection_record(
        &credentials,
        &path,
        input(
            "My API",
            "https://host.example/v1",
            ConnectionKind::OpenaiCompatible,
            ConnectionAuthType::ApiKey,
        ),
    )
    .unwrap();
    assert_eq!(added.key_storage, KeyStorage::Session);
    let listed = load_connections(&path).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!serde_json::to_string(&listed[0])
        .unwrap()
        .contains("test-only-key"));

    // Blank key on update keeps the stored key.
    let mut patch = input(
        "Renamed API",
        "https://host.example/v1",
        ConnectionKind::OpenaiCompatible,
        ConnectionAuthType::ApiKey,
    );
    patch.key = None;
    let updated = update_api_connection_record(&credentials, &path, &added.id, patch).unwrap();
    assert_eq!(updated.name, "Renamed API");
    assert_eq!(
        &*studiocut_ai::api_connections::resolve_connection_key(&credentials, &added.id).unwrap(),
        "test-only-key"
    );

    // Switching to no authentication forgets the key.
    let mut patch = input(
        "Renamed API",
        "https://host.example/v1",
        ConnectionKind::OpenaiCompatible,
        ConnectionAuthType::None,
    );
    patch.key = None;
    patch.key_storage = KeyStorage::None;
    let updated = update_api_connection_record(&credentials, &path, &added.id, patch).unwrap();
    assert_eq!(updated.key_storage, KeyStorage::None);
    assert!(
        studiocut_ai::api_connections::resolve_connection_key(&credentials, &added.id).is_err()
    );

    delete_api_connection_record(&credentials, &path, &added.id).unwrap();
    assert!(load_connections(&path).unwrap().is_empty());
    assert!(delete_api_connection_record(&credentials, &path, &added.id).is_err());
}

#[test]
fn add_requires_a_key_for_api_key_auth() {
    let credentials = McpCredentials::default();
    let (_dir, path) = temp_path();
    let mut no_key = input(
        "My API",
        "https://host.example/v1",
        ConnectionKind::OpenaiCompatible,
        ConnectionAuthType::ApiKey,
    );
    no_key.key = None;
    assert!(add_api_connection_record(&credentials, &path, no_key).is_err());
    let mut bad_storage = input(
        "My API",
        "https://host.example/v1",
        ConnectionKind::OpenaiCompatible,
        ConnectionAuthType::ApiKey,
    );
    bad_storage.key_storage = KeyStorage::None;
    assert!(add_api_connection_record(&credentials, &path, bad_storage).is_err());
    assert!(load_connections(&path).unwrap().is_empty());
}

#[test]
fn model_filter_matches_director_chat_only_rules() {
    let value = json!({"data":[
        {"id":"gpt-4o"},
        {"id":"text-embedding-3-small"},
        {"id":"whisper-1"},
        {"id":"o1-preview"},
    ]});
    let models = filter_director_models(&value, "openai");
    let kept: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(kept, vec!["gpt-4o", "o1-preview"]);
}
