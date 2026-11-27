//! Saved API connections: a base URL plus an optional API key, so a
//! feature (AI Director, custom MCP server) can reuse one connection
//! instead of asking for the URL and key on every call.
//!
//! # Storage
//!
//! Connection records live in `api-connections.json` in the app local
//! data directory. A record never contains a key value: it only carries
//! the key's storage reference — the keyring service
//! [`API_CONNECTIONS_KEYRING_SERVICE`], the account (the connection
//! id), and the storage choice (`os` or `session`). Key values live in
//! the OS credential store or in session-only memory, reusing
//! [`McpCredentials`] exactly as the MCP servers do (see
//! `mcp::McpCredentials::set_for_service`); no second secret store
//! exists. Keys are never returned to the frontend, never written to
//! project files or preferences, and never appear in error messages or
//! logs.
//!
//! # Validation
//!
//! Base URLs follow the same rules as `mcp::validate_remote_url`:
//! HTTPS is required, plain HTTP is accepted only for loopback hosts,
//! the URL is capped at 2048 characters, and credentials, query
//! strings and fragments are rejected so secrets can never hide in a
//! stored URL.
//!
//! # Probing
//!
//! `test_api_connection` performs one live GET: `{base_url}/models`
//! for OpenAI-compatible connections, the base URL itself for generic
//! connections. It reports the HTTP status, latency in milliseconds,
//! and (for OpenAI-compatible connections) the number of chat models
//! in the response, using the same chat-only filter as the Director
//! (`director::filter_director_models` with the OpenAI provider
//! rules). Response bodies are never logged; only the status and the
//! model count are reported. A failed probe is reported in the result
//! (`ok: false` with an error message), not as a command error, so
//! the panel can show it next to the connection that was tested.

use crate::{
    director::{filter_director_models, DirectorModelsResponse},
    mcp::{validate_api_key, McpCredentials},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use zeroize::Zeroizing;

/// File name for saved connections inside the app local data directory.
pub const API_CONNECTIONS_FILE_NAME: &str = "api-connections.json";
/// Keyring service (namespace) for API-connection keys. Separate from
/// the MCP service (`app.studiocut.mcp`) so a connection key can never
/// collide with an MCP server key.
pub const API_CONNECTIONS_KEYRING_SERVICE: &str = "app.studiocut.api-connections";
/// Maximum number of saved connections.
pub const MAX_API_CONNECTIONS: usize = 100;
/// Maximum size of the saved-connections file.
pub const MAX_CONNECTIONS_FILE_BYTES: u64 = 1_048_576;
/// Maximum size of a `/models` response body, mirroring the Director.
pub const MAX_CONNECTION_MODELS_JSON: u64 = 2_000_000;
/// Default header name for API-key authentication.
pub const DEFAULT_AUTH_HEADER: &str = "Authorization";

/// The kind of API a connection talks to. `openai-compatible` serves
/// an OpenAI-shape `/models` list; `generic` is a base URL plus a key
/// only, with no model calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionKind {
    OpenaiCompatible,
    Generic,
}

/// How a connection authenticates: no credential, or an API key sent
/// in one header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectionAuthType {
    None,
    ApiKey,
}

/// Where a connection's key is kept. `None` means no key is stored
/// (used when the connection has no authentication).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyStorage {
    Os,
    Session,
    None,
}

/// One saved API connection. The record stores only the key's storage
/// reference (`key_storage`, together with the keyring service and the
/// connection id as the account); it never contains the key value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiConnection {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub kind: ConnectionKind,
    pub auth_type: ConnectionAuthType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    pub key_storage: KeyStorage,
}

/// Input for adding or updating a connection, as sent by the panel.
/// `key` is sent only when the user types one: on update, an absent
/// or blank key keeps the stored key unchanged. `key_storage` is
/// `none` when the connection has no authentication.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiConnectionInput {
    pub name: String,
    pub base_url: String,
    pub kind: ConnectionKind,
    pub auth_type: ConnectionAuthType,
    #[serde(default)]
    pub auth_header: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    pub key_storage: KeyStorage,
}

/// Result of testing one connection. A failed probe is reported here
/// (`ok: false` with an error message) rather than as a command
/// error, so the panel can show it next to the connection tested.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiConnectionTestResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    pub latency_ms: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// --- Validation ---------------------------------------------------------

fn is_loopback(host: &str) -> bool {
    ["localhost", "127.0.0.1", "[::1]", "::1"].contains(&host)
}

/// Validate a connection base URL, mirroring `mcp::validate_remote_url`:
/// HTTPS is required, plain HTTP is accepted only for loopback hosts,
/// the URL is capped at 2048 characters, and credentials, query
/// strings and fragments are rejected.
pub fn validate_connection_url(raw: &str) -> Result<reqwest::Url, String> {
    if raw.is_empty() || raw.len() > 2048 {
        return Err("Connection URL must be 1–2048 characters".into());
    }
    let url = reqwest::Url::parse(raw).map_err(|_| "Connection URL is not a valid URL")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Connection URL must not contain credentials, a query string, or a fragment".into(),
        );
    }
    let host = url.host_str().ok_or("Connection URL needs a host")?;
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_loopback(host) => Ok(url),
        "http" => Err(
            "Remote connections require HTTPS; plain HTTP is accepted only for localhost".into(),
        ),
        _ => Err("Connection URL must use https:// (or http:// for localhost)".into()),
    }
}

fn validate_connection_name(name: &str) -> Result<String, String> {
    let name = name.trim().to_owned();
    if name.is_empty() || name.encode_utf16().count() > 120 {
        return Err("Connection name must be 1–120 characters".into());
    }
    Ok(name)
}

/// Validate an API-key header name. A blank name falls back to the
/// default (`Authorization`). Otherwise the name must be a valid HTTP
/// header token of 1–120 characters.
pub fn validate_auth_header(header: &str) -> Result<String, String> {
    // Control characters are never valid in a header name, even at the
    // edges: check before trimming, so a value like "X-Header\n" is
    // rejected rather than silently cleaned to a valid name.
    if header.chars().any(|c| c.is_control()) {
        return Err("Authorization header must be a valid header name of 1–120 characters".into());
    }
    let header = header.trim();
    if header.is_empty() {
        return Ok(DEFAULT_AUTH_HEADER.to_owned());
    }
    if header.len() > 120
        || !header
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c))
    {
        return Err("Authorization header must be a valid header name of 1–120 characters".into());
    }
    Ok(header.to_owned())
}

/// Validate a connection id: it must be a UUID, as generated on add.
pub fn validate_connection_id(id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(id).map_err(|_| "Connection id is not a valid identifier".to_string())?;
    Ok(())
}

/// Derive the `/models` URL for an OpenAI-compatible base URL. A base
/// ending in `/chat/completions` has that suffix replaced (mirroring
/// the Director's compatible-provider derivation); a base already
/// ending in `/models` is used as-is; otherwise `/models` is appended.
pub fn models_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if let Some(stripped) = trimmed.strip_suffix("/chat/completions") {
        format!("{stripped}/models")
    } else if trimmed.ends_with("/models") {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/models")
    }
}

/// Build the header carrying an API key. The `Authorization` header
/// carries a Bearer token; any other header name carries the key as-is.
pub fn auth_header_for(connection: &ApiConnection, key: &Zeroizing<String>) -> (String, String) {
    let name = connection
        .auth_header
        .clone()
        .unwrap_or_else(|| DEFAULT_AUTH_HEADER.to_owned());
    if name.eq_ignore_ascii_case(DEFAULT_AUTH_HEADER) {
        (name, format!("Bearer {}", key.as_str()))
    } else {
        (name, key.as_str().to_owned())
    }
}

/// Validate the shared fields of an input and build the stored record.
/// The key itself is handled by the caller (see
/// [`store_connection_key`]); this function only records where the key
/// is kept.
fn validated_record(
    id: &str,
    input: &ApiConnectionInput,
    key_storage: KeyStorage,
) -> Result<ApiConnection, String> {
    let name = validate_connection_name(&input.name)?;
    let url = validate_connection_url(&input.base_url)?;
    let base_url = url.as_str().trim_end_matches('/').to_owned();
    let auth_header = match input.auth_type {
        ConnectionAuthType::None => None,
        ConnectionAuthType::ApiKey => Some(validate_auth_header(
            input.auth_header.as_deref().unwrap_or(""),
        )?),
    };
    Ok(ApiConnection {
        id: id.to_owned(),
        name,
        base_url,
        kind: input.kind,
        auth_type: input.auth_type,
        auth_header,
        key_storage,
    })
}

// --- Key storage (McpCredentials reuse) ---------------------------------
//
// Keys are stored through `McpCredentials`, the same OS-keyring-or-
// session-only store the MCP servers use, under the separate keyring
// service `app.studiocut.api-connections` with the connection id as
// the account. See `mcp::McpCredentials::set_for_service`.

/// Store a connection key in OS storage or session-only memory.
pub fn store_connection_key(
    credentials: &McpCredentials,
    connection_id: &str,
    key: String,
    storage: KeyStorage,
) -> Result<(), String> {
    match storage {
        KeyStorage::Os => {
            credentials.set_for_service(API_CONNECTIONS_KEYRING_SERVICE, connection_id, key, "os")
        }
        KeyStorage::Session => credentials.set_for_service(
            API_CONNECTIONS_KEYRING_SERVICE,
            connection_id,
            key,
            "session",
        ),
        KeyStorage::None => Err("Choose OS or session-only storage".into()),
    }
}

/// Resolve a stored connection key (session memory first, then the OS
/// credential store).
pub fn resolve_connection_key(
    credentials: &McpCredentials,
    connection_id: &str,
) -> Result<Zeroizing<String>, String> {
    credentials.get_for_service(API_CONNECTIONS_KEYRING_SERVICE, connection_id)
}

/// Forget a stored connection key (session memory and OS store).
pub fn delete_connection_key(
    credentials: &McpCredentials,
    connection_id: &str,
) -> Result<(), String> {
    credentials.delete_for_service(API_CONNECTIONS_KEYRING_SERVICE, connection_id)
}

// --- File-backed record store -------------------------------------------

/// Path of the saved-connections file inside the app local data directory.
pub fn connections_file_path(data_dir: &Path) -> PathBuf {
    data_dir.join(API_CONNECTIONS_FILE_NAME)
}

/// Load saved connections. A missing file means no connections yet.
pub fn load_connections(path: &Path) -> Result<Vec<ApiConnection>, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.len() as u64 > MAX_CONNECTIONS_FILE_BYTES {
                return Err("Saved API connections exceed the 1 MB limit".into());
            }
            serde_json::from_slice(&bytes)
                .map_err(|_| "Saved API connections could not be read".to_string())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(error) => Err(error.to_string()),
    }
}

/// Save connections atomically (write to a temporary file in the same
/// directory, then replace the saved file).
pub fn save_connections(path: &Path, connections: &[ApiConnection]) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(connections).map_err(|e| e.to_string())?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut staging = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    staging.write_all(&bytes).map_err(|e| e.to_string())?;
    staging.as_file().sync_all().map_err(|e| e.to_string())?;
    staging.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

// --- Record operations (pure apart from the key store and the file) -----

/// Add a connection: validate the input, store the key (when the
/// connection uses one), then save the record. The returned record
/// never contains the key value.
pub fn add_api_connection_record(
    credentials: &McpCredentials,
    path: &Path,
    input: ApiConnectionInput,
) -> Result<ApiConnection, String> {
    let mut connections = load_connections(path)?;
    if connections.len() >= MAX_API_CONNECTIONS {
        return Err("Maximum 100 API connections".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let key_storage = match input.auth_type {
        ConnectionAuthType::None => KeyStorage::None,
        ConnectionAuthType::ApiKey => {
            let key = input
                .key
                .as_deref()
                .filter(|key| !key.trim().is_empty())
                .ok_or("Enter an API key, or choose \"No authentication\"")?;
            validate_api_key(key)?;
            match input.key_storage {
                KeyStorage::Os | KeyStorage::Session => input.key_storage,
                KeyStorage::None => {
                    return Err("Choose OS or session-only storage".into());
                }
            }
        }
    };
    // Validate the shared fields before touching the key store.
    let record = validated_record(&id, &input, key_storage)?;
    if input.auth_type == ConnectionAuthType::ApiKey {
        let key = input.key.clone().unwrap_or_default();
        store_connection_key(credentials, &id, key, key_storage)?;
    }
    connections.push(record.clone());
    if let Err(error) = save_connections(path, &connections) {
        if input.auth_type == ConnectionAuthType::ApiKey {
            let _ = delete_connection_key(credentials, &id);
        }
        return Err(error);
    }
    Ok(record)
}

/// Update a connection. An absent or blank key keeps the stored key
/// unchanged (moving it between OS and session storage when the
/// storage choice changed); a new key replaces it; switching to no
/// authentication deletes it.
pub fn update_api_connection_record(
    credentials: &McpCredentials,
    path: &Path,
    id: &str,
    input: ApiConnectionInput,
) -> Result<ApiConnection, String> {
    validate_connection_id(id)?;
    let mut connections = load_connections(path)?;
    let index = connections
        .iter()
        .position(|connection| connection.id == id)
        .ok_or("Connection not found")?;
    let existing = connections[index].clone();
    let mut key_storage = KeyStorage::None;
    match input.auth_type {
        ConnectionAuthType::None => {
            if existing.key_storage != KeyStorage::None {
                delete_connection_key(credentials, id)?;
            }
        }
        ConnectionAuthType::ApiKey => {
            let new_key = input.key.as_deref().filter(|key| !key.trim().is_empty());
            match new_key {
                Some(key) => {
                    let storage = match input.key_storage {
                        KeyStorage::Os | KeyStorage::Session => input.key_storage,
                        KeyStorage::None => {
                            return Err("Choose OS or session-only storage".into());
                        }
                    };
                    validate_api_key(key)?;
                    // Remove the old entry first so changing storage
                    // leaves no orphaned key behind.
                    if existing.key_storage != KeyStorage::None {
                        delete_connection_key(credentials, id)?;
                    }
                    store_connection_key(credentials, id, key.to_owned(), storage)?;
                    key_storage = storage;
                }
                None => {
                    if existing.key_storage == KeyStorage::None {
                        return Err("Save an API key for this connection first".into());
                    }
                    key_storage = existing.key_storage;
                    if input.key_storage != KeyStorage::None
                        && input.key_storage != existing.key_storage
                    {
                        let key = resolve_connection_key(credentials, id)?;
                        delete_connection_key(credentials, id)?;
                        store_connection_key(credentials, id, key.to_string(), input.key_storage)?;
                        key_storage = input.key_storage;
                    }
                }
            }
        }
    }
    let record = validated_record(id, &input, key_storage)?;
    connections[index] = record.clone();
    save_connections(path, &connections)?;
    Ok(record)
}

/// Delete a connection and forget its stored key. The key deletion is
/// best-effort: the record is already gone, so an orphaned key entry
/// for a deleted connection id is unreachable.
pub fn delete_api_connection_record(
    credentials: &McpCredentials,
    path: &Path,
    id: &str,
) -> Result<(), String> {
    validate_connection_id(id)?;
    let mut connections = load_connections(path)?;
    let before = connections.len();
    connections.retain(|connection| connection.id != id);
    if connections.len() == before {
        return Err("Connection not found".into());
    }
    save_connections(path, &connections)?;
    let _ = delete_connection_key(credentials, id);
    Ok(())
}

// --- Probing and model listing ------------------------------------------

fn probe_http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Cannot initialize secure HTTP client".to_string())
}

/// Probe one connection with a single GET: `{base_url}/models` for
/// OpenAI-compatible connections, the base URL itself for generic
/// ones. Reports the HTTP status, latency, and — for OpenAI-compatible
/// connections whose response parses as a model list — the number of
/// chat models, using the Director's chat-only filter. Response bodies
/// are never logged; only the status and the model count are reported.
/// A failed probe is reported in the result, not as an error.
pub fn probe_connection(
    connection: &ApiConnection,
    key: Option<&Zeroizing<String>>,
) -> ApiConnectionTestResult {
    let started = Instant::now();
    let failed = |error: String| ApiConnectionTestResult {
        ok: false,
        status: None,
        latency_ms: started.elapsed().as_millis(),
        model_count: None,
        error: Some(error),
    };
    let url = match connection.kind {
        ConnectionKind::OpenaiCompatible => models_url(&connection.base_url),
        ConnectionKind::Generic => connection.base_url.clone(),
    };
    let client = match probe_http_client() {
        Ok(client) => client,
        Err(error) => return failed(error),
    };
    let mut request = client.get(url);
    if let Some(key) = key {
        let (name, value) = auth_header_for(connection, key);
        request = request.header(name, value);
    }
    let response = match request.send() {
        Ok(response) => response,
        Err(_) => {
            return failed(
                "Connection test failed. Check the base URL, network, and TLS certificate; no automatic retry was made"
                    .into(),
            );
        }
    };
    let status = response.status().as_u16();
    let latency_ms = started.elapsed().as_millis();
    if !response.status().is_success() {
        let detail = match status {
            401 => "Check the API key.",
            403 => "Check the API key and account.",
            _ => "Check the base URL and API key.",
        };
        return ApiConnectionTestResult {
            ok: false,
            status: Some(status),
            latency_ms,
            model_count: None,
            error: Some(format!("Connection test returned HTTP {status}. {detail}")),
        };
    }
    let mut model_count = None;
    if connection.kind == ConnectionKind::OpenaiCompatible {
        let mut data = Vec::new();
        if response
            .take(MAX_CONNECTION_MODELS_JSON + 1)
            .read_to_end(&mut data)
            .is_ok()
            && (data.len() as u64) <= MAX_CONNECTION_MODELS_JSON
        {
            if let Ok(value) = serde_json::from_slice::<Value>(&data) {
                model_count = Some(filter_director_models(&value, "openai").len());
            }
        }
    }
    ApiConnectionTestResult {
        ok: true,
        status: Some(status),
        latency_ms,
        model_count,
        error: None,
    }
}

/// Resolve the key for a connection call: a per-call key wins (used
/// for that call only, never stored); otherwise the stored key is
/// used. Connections without authentication need no key.
fn resolve_key_for_call(
    credentials: &McpCredentials,
    connection: &ApiConnection,
    per_call: Option<String>,
) -> Result<Option<Zeroizing<String>>, String> {
    if let Some(key) = per_call {
        if !key.trim().is_empty() {
            validate_api_key(&key)?;
            return Ok(Some(Zeroizing::new(key)));
        }
    }
    match connection.auth_type {
        ConnectionAuthType::None => Ok(None),
        ConnectionAuthType::ApiKey => {
            Ok(Some(resolve_connection_key(credentials, &connection.id)?))
        }
    }
}

/// Test one saved connection. Only a missing or unreadable connection
/// fails the command; probe failures are reported in the result.
pub fn test_api_connection_record(
    credentials: &McpCredentials,
    path: &Path,
    id: &str,
) -> Result<ApiConnectionTestResult, String> {
    validate_connection_id(id)?;
    let connections = load_connections(path)?;
    let connection = connections
        .iter()
        .find(|connection| connection.id == id)
        .ok_or("Connection not found")?;
    let key = match resolve_key_for_call(credentials, connection, None) {
        Ok(key) => key,
        Err(error) => {
            return Ok(ApiConnectionTestResult {
                ok: false,
                status: None,
                latency_ms: 0,
                model_count: None,
                error: Some(error),
            });
        }
    };
    Ok(probe_connection(connection, key.as_ref()))
}

/// Fetch the chat-model list of an OpenAI-compatible connection,
/// mirroring the Director's `/models` fetch: the same OpenAI-shape
/// parsing, the same chat-only filter, and the same 100-model cap.
/// Generic connections provide no model list.
pub fn list_connection_models_record(
    credentials: &McpCredentials,
    path: &Path,
    id: &str,
    api_key: Option<String>,
) -> Result<DirectorModelsResponse, String> {
    validate_connection_id(id)?;
    let connections = load_connections(path)?;
    let connection = connections
        .iter()
        .find(|connection| connection.id == id)
        .ok_or("Connection not found")?;
    if connection.kind != ConnectionKind::OpenaiCompatible {
        return Err("Connection kind \"generic\" does not provide a model list".into());
    }
    let key = resolve_key_for_call(credentials, connection, api_key)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Cannot initialize secure Director client".to_string())?;
    let mut request = client.get(models_url(&connection.base_url));
    if let Some(key) = &key {
        let (name, value) = auth_header_for(connection, key);
        request = request.header(name, value);
    }
    let response = request.send().map_err(|_| {
        "Model list request failed. Check the endpoint, network, and TLS certificate; no automatic retry was made"
    })?;
    if !response.status().is_success() {
        return Err(format!(
            "Model list request returned HTTP {}. Check the API key and provider",
            response.status().as_u16()
        ));
    }
    let mut data = Vec::new();
    response
        .take(MAX_CONNECTION_MODELS_JSON + 1)
        .read_to_end(&mut data)
        .map_err(|_| "Failed reading model list response")?;
    if data.len() as u64 > MAX_CONNECTION_MODELS_JSON {
        return Err("Model list exceeds the 2 MB response limit".into());
    }
    let value: Value =
        serde_json::from_slice(&data).map_err(|_| "Model list returned invalid JSON")?;
    let models = filter_director_models(&value, "openai");
    Ok(DirectorModelsResponse { models, note: None })
}

// --- Tauri commands -------------------------------------------------------
//
// Each command runs file or network work on a blocking thread, as in
// `main.rs` and `mcp.rs`. The coordinator registers these in
// `invoke_handler`; `McpCredentials` state is already managed there.

fn data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_local_data_dir().map_err(|e| e.to_string())
}

/// List saved connections. Records never contain a key value.
#[tauri::command]
pub async fn list_api_connections(app: AppHandle) -> Result<Vec<ApiConnection>, String> {
    let data_dir = data_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        load_connections(&connections_file_path(&data_dir))
    })
    .await
    .map_err(|_| "Connection list task failed".to_string())?
}

/// Add a connection and store its key (when it uses one).
#[tauri::command]
pub async fn add_api_connection(
    app: AppHandle,
    connection: ApiConnectionInput,
) -> Result<ApiConnection, String> {
    let data_dir = data_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let credentials = app.state::<McpCredentials>();
        add_api_connection_record(&credentials, &connections_file_path(&data_dir), connection)
    })
    .await
    .map_err(|_| "Connection add task failed".to_string())?
}

/// Update a connection. The argument is named `patch` to match the
/// panel: it carries the full form, and an absent or blank key keeps
/// the stored key unchanged.
#[tauri::command]
pub async fn update_api_connection(
    app: AppHandle,
    id: String,
    patch: ApiConnectionInput,
) -> Result<ApiConnection, String> {
    let data_dir = data_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let credentials = app.state::<McpCredentials>();
        update_api_connection_record(&credentials, &connections_file_path(&data_dir), &id, patch)
    })
    .await
    .map_err(|_| "Connection update task failed".to_string())?
}

/// Delete a connection and forget its stored key.
#[tauri::command]
pub async fn delete_api_connection(app: AppHandle, id: String) -> Result<(), String> {
    let data_dir = data_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let credentials = app.state::<McpCredentials>();
        delete_api_connection_record(&credentials, &connections_file_path(&data_dir), &id)
    })
    .await
    .map_err(|_| "Connection delete task failed".to_string())?
}

/// Test one saved connection. Only a missing or unreadable connection
/// fails the command; probe failures are reported in the result.
#[tauri::command]
pub async fn test_api_connection(
    app: AppHandle,
    id: String,
) -> Result<ApiConnectionTestResult, String> {
    let data_dir = data_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let credentials = app.state::<McpCredentials>();
        test_api_connection_record(&credentials, &connections_file_path(&data_dir), &id)
    })
    .await
    .map_err(|_| "Connection test task failed".to_string())?
}

/// List the chat models of an OpenAI-compatible connection. Generic
/// connections provide no model list.
#[tauri::command]
pub async fn list_connection_models(
    app: AppHandle,
    id: String,
    api_key: Option<String>,
) -> Result<DirectorModelsResponse, String> {
    let data_dir = data_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let credentials = app.state::<McpCredentials>();
        list_connection_models_record(
            &credentials,
            &connections_file_path(&data_dir),
            &id,
            api_key,
        )
    })
    .await
    .map_err(|_| "Connection model list task failed".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::McpCredentials;

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
            // Session-only storage: OS keyring writes need a running
            // secret service, which neither this VM nor CI provides
            // (the same reason the MCP credential tests use session
            // storage). The OS path uses the same `set_for_service`
            // code as the MCP servers.
            key_storage: KeyStorage::Session,
        }
    }

    fn temp_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(API_CONNECTIONS_FILE_NAME);
        (dir, path)
    }

    #[test]
    fn connection_url_rules_mirror_remote_url_rules() {
        assert!(validate_connection_url("https://example.com/v1").is_ok());
        assert!(validate_connection_url("https://example.com:8443/a/b").is_ok());
        assert!(validate_connection_url("http://localhost:8080/v1").is_ok());
        assert!(validate_connection_url("http://127.0.0.1:11434/v1").is_ok());
        assert!(validate_connection_url("http://[::1]:8080/v1").is_ok());
        assert!(validate_connection_url("http://example.com/v1").is_err());
        assert!(validate_connection_url("http://192.168.1.2/v1").is_err());
        assert!(validate_connection_url("ftp://example.com/v1").is_err());
        assert!(validate_connection_url("https://user:secret@example.com/v1").is_err());
        assert!(validate_connection_url("https://example.com/v1?api_key=x").is_err());
        assert!(validate_connection_url("https://example.com/v1#section").is_err());
        assert!(validate_connection_url("").is_err());
        assert!(validate_connection_url("https://example.com").is_ok());
        assert!(
            validate_connection_url(&format!("https://example.com/{}", "a".repeat(2030))).is_err()
        );
    }

    #[test]
    fn models_url_derivation() {
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
    fn auth_header_defaults_and_bearer_rule() {
        assert_eq!(validate_auth_header("").unwrap(), "Authorization");
        assert_eq!(validate_auth_header("  ").unwrap(), "Authorization");
        assert_eq!(validate_auth_header("x-api-key").unwrap(), "x-api-key");
        assert!(validate_auth_header("Bad Header").is_err());
        assert!(validate_auth_header("X-Header\n").is_err());
        let connection = ApiConnection {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Test".into(),
            base_url: "https://host.example/v1".into(),
            kind: ConnectionKind::OpenaiCompatible,
            auth_type: ConnectionAuthType::ApiKey,
            auth_header: None,
            key_storage: KeyStorage::Session,
        };
        let key = Zeroizing::new("secret".to_string());
        let (name, value) = auth_header_for(&connection, &key);
        assert_eq!(name, "Authorization");
        assert_eq!(value, "Bearer secret");
        let custom = ApiConnection {
            auth_header: Some("x-api-key".into()),
            ..connection
        };
        let (name, value) = auth_header_for(&custom, &key);
        assert_eq!(name, "x-api-key");
        assert_eq!(value, "secret");
    }

    #[test]
    fn record_validation_rejects_bad_fields() {
        let id = uuid::Uuid::new_v4().to_string();
        let mut good = input(
            "My API",
            "https://host.example/v1",
            ConnectionKind::OpenaiCompatible,
            ConnectionAuthType::ApiKey,
        );
        let record = validated_record(&id, &good, KeyStorage::Os).unwrap();
        assert_eq!(record.base_url, "https://host.example/v1");
        assert_eq!(record.auth_header.as_deref(), Some("Authorization"));
        good.name = "   ".into();
        assert!(validated_record(&id, &good, KeyStorage::Os).is_err());
        good.name = "My API".into();
        good.base_url = "http://host.example/v1".into();
        assert!(validated_record(&id, &good, KeyStorage::Os).is_err());
        good.base_url = "https://host.example/v1?api_key=x".into();
        assert!(validated_record(&id, &good, KeyStorage::Os).is_err());
        good.base_url = "https://host.example/v1".into();
        good.auth_header = Some("Bad Header".into());
        assert!(validated_record(&id, &good, KeyStorage::Os).is_err());
        good.auth_header = None;
        let record = validated_record(&id, &good, KeyStorage::Os).unwrap();
        assert_eq!(record.auth_header.as_deref(), Some("Authorization"));
        let mut none = input(
            "Plain",
            "https://host.example/v1",
            ConnectionKind::Generic,
            ConnectionAuthType::None,
        );
        none.key = None;
        none.key_storage = KeyStorage::None;
        let record = validated_record(&id, &none, KeyStorage::None).unwrap();
        assert_eq!(record.auth_type, ConnectionAuthType::None);
        assert!(record.auth_header.is_none());
        assert_eq!(record.key_storage, KeyStorage::None);
    }

    #[test]
    fn file_store_round_trip_and_missing_file() {
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
        let loaded = load_connections(&path).unwrap();
        assert_eq!(loaded, vec![record]);
        // The saved record never contains the key value.
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("test-only-key"));
        std::fs::write(&path, "not json").unwrap();
        assert!(load_connections(&path).is_err());
    }

    #[test]
    fn add_update_delete_with_session_keys() {
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
        // The key is retrievable through the key store, never from the record.
        assert_eq!(
            &*resolve_connection_key(&credentials, &added.id).unwrap(),
            "test-only-key"
        );
        let listed = load_connections(&path).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, added.id);
        let serialized = serde_json::to_string(&listed[0]).unwrap();
        assert!(!serialized.contains("test-only-key"));
        assert!(serialized.contains("\"keyStorage\":\"session\""));

        // Update with a blank key keeps the stored key.
        let mut patch = input(
            "Renamed API",
            "https://host.example/v1",
            ConnectionKind::OpenaiCompatible,
            ConnectionAuthType::ApiKey,
        );
        patch.key = None;
        patch.key_storage = KeyStorage::Session;
        let updated = update_api_connection_record(&credentials, &path, &added.id, patch).unwrap();
        assert_eq!(updated.name, "Renamed API");
        assert_eq!(
            &*resolve_connection_key(&credentials, &added.id).unwrap(),
            "test-only-key"
        );

        // Update with a new key rotates it.
        let mut patch = input(
            "Renamed API",
            "https://host.example/v1",
            ConnectionKind::OpenaiCompatible,
            ConnectionAuthType::ApiKey,
        );
        patch.key = Some("rotated-key".into());
        patch.key_storage = KeyStorage::Session;
        let updated = update_api_connection_record(&credentials, &path, &added.id, patch).unwrap();
        assert_eq!(updated.key_storage, KeyStorage::Session);
        assert_eq!(
            &*resolve_connection_key(&credentials, &added.id).unwrap(),
            "rotated-key"
        );

        // Update to no authentication deletes the key.
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
        assert!(resolve_connection_key(&credentials, &added.id).is_err());

        // Delete removes the record.
        delete_api_connection_record(&credentials, &path, &added.id).unwrap();
        assert!(load_connections(&path).unwrap().is_empty());
        assert!(delete_api_connection_record(&credentials, &path, &added.id).is_err());
        assert!(update_api_connection_record(
            &credentials,
            &path,
            &added.id,
            input(
                "Gone",
                "https://host.example/v1",
                ConnectionKind::Generic,
                ConnectionAuthType::None,
            ),
        )
        .is_err());
    }

    #[test]
    fn add_rejects_missing_key_and_bad_storage() {
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
    fn generic_connection_has_no_model_list() {
        let credentials = McpCredentials::default();
        let (_dir, path) = temp_path();
        let mut generic = input(
            "Plain",
            "https://host.example/v1",
            ConnectionKind::Generic,
            ConnectionAuthType::None,
        );
        generic.key = None;
        generic.key_storage = KeyStorage::None;
        let added = add_api_connection_record(&credentials, &path, generic).unwrap();
        let listed =
            list_connection_models_record(&credentials, &path, &added.id, None).unwrap_err();
        assert!(listed.contains("generic"));
        // Unknown ids fail before any network is attempted.
        assert!(list_connection_models_record(
            &credentials,
            &path,
            &uuid::Uuid::new_v4().to_string(),
            None,
        )
        .is_err());
        assert!(
            test_api_connection_record(&credentials, &path, &uuid::Uuid::new_v4().to_string(),)
                .is_err()
        );
    }

    #[test]
    fn connection_ids_must_be_uuids() {
        assert!(validate_connection_id(&uuid::Uuid::new_v4().to_string()).is_ok());
        assert!(validate_connection_id("server-1").is_err());
        assert!(validate_connection_id("").is_err());
    }

    #[test]
    fn model_filter_matches_director_chat_only_rules() {
        let value = serde_json::json!({"data":[
            {"id":"gpt-4o"},
            {"id":"text-embedding-3-small"},
            {"id":"whisper-1"},
            {"id":"o1-preview"},
        ]});
        let models = filter_director_models(&value, "openai");
        let kept: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(kept, vec!["gpt-4o", "o1-preview"]);
    }
}
