//! Model Context Protocol (MCP) client and generation presets.
//!
//! The client speaks MCP over two transports: Streamable HTTP for remote
//! servers and stdio for local servers. It also provides a REST client for
//! the Higgsfield API, which is the primary generation path for that
//! preset (its hosted MCP server requires browser OAuth, which this
//! desktop client does not implement).
//!
//! # Implementation decision: minimal JSON-RPC client, not the `rmcp` SDK
//!
//! The official `rmcp` Rust SDK (client feature, Streamable HTTP and
//! child-process transports) was reviewed on docs.rs (version 0.13.0).
//! It fits the protocol surface, but it requires the Tokio async runtime
//! and several new dependencies, its client API has changed across
//! recent releases, and compilation cannot be verified on this machine
//! (no Tauri system libraries; `cargo test` fails before any crate
//! builds). A wrong guess at an `rmcp` API would therefore fail only in
//! CI. This module instead implements the small part of MCP it needs —
//! `initialize`, `tools/list`, `tools/call`, session-header handling —
//! as plain JSON-RPC over the `reqwest` blocking client and
//! `std::process` already used elsewhere in this crate (see
//! `director.rs` and `media.rs`). The protocol surface is stable and
//! fully covered by the unit tests in this module and in
//! `tests/mcp.rs`. Revisit `rmcp` only if MCP features beyond tools
//! (resources, prompts, sampling) are required.
//!
//! # Security
//!
//! - Remote server URLs must be HTTPS; plain HTTP is accepted only for
//!   loopback hosts (local development servers). URLs must not carry
//!   credentials, a query string, or a fragment.
//! - API keys live in the OS credential store or in session-only memory
//!   (see [`McpCredentials`]); they are never written to project files
//!   or preferences, and never appear in error messages or logs.
//! - stdio servers are spawned directly, never through a shell.
//! - Generation results are downloaded only over HTTPS, with a size cap,
//!   and only into the app data directory.
//!
//! # Preset provenance
//!
//! Only endpoints and tools verified against vendor documentation are
//! listed (checked 27 November 2026). The Higgsfield REST catalog is a
//! verified subset, not the full catalog; the full list is in the
//! official Higgsfield API reference. HyperFrames refers to the
//! independently operated hosted workspace at hyperframes.net, which is
//! a different product from HeyGen's open-source HyperFrames HTML-to-
//! video CLI (whose hosted MCP endpoint is mcp.heygen.com and uses
//! OAuth).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Mutex,
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// MCP protocol version negotiated at `initialize`.
pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
/// Client identification sent in `initialize`.
pub const MCP_CLIENT_NAME: &str = "StudioCut AI";
pub const MCP_CLIENT_VERSION: &str = "0.1.0";
/// Maximum size of one MCP HTTP response body (tool results can carry
/// long media URLs and structured output).
pub const MAX_MCP_RESPONSE_BYTES: u64 = 4_194_304;
/// Maximum size of a downloaded generation result.
pub const MAX_DOWNLOAD_BYTES: u64 = 524_288_000;
/// Higgsfield REST API base URL (fixed; the API has one base URL).
pub const HIGGSFIELD_API_BASE: &str = "https://api.higgsfield.ai";
/// Hosted Higgsfield MCP endpoint. OAuth only; listed as a preset for
/// documentation, not as a working path (see [`builtin_presets`]).
pub const HIGGSFIELD_MCP_URL: &str = "https://mcp.higgsfield.ai/mcp";
/// HyperFrames hosted MCP endpoint (API-key authentication).
pub const HYPERFRAMES_MCP_URL: &str = "https://hyperframes.net/api/mcp";
/// Credential-store id used when no per-call key is supplied to a
/// Higgsfield REST call.
pub const HIGGSFIELD_CREDENTIAL_ID: &str = "higgsfield-rest";

fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Cannot initialize secure HTTP client".into())
}

// --- JSON-RPC -----------------------------------------------------------

/// Build one JSON-RPC 2.0 request object. `params` is omitted when
/// `None`, matching the MCP convention for parameter-less methods.
pub fn build_request(id: u64, method: &str, params: Option<Value>) -> Value {
    let mut request = json!({"jsonrpc": "2.0", "id": id, "method": method});
    if let Some(params) = params {
        request["params"] = params;
    }
    request
}

/// Parse one JSON-RPC 2.0 response body. Returns the `result` value, or
/// the server's error as a readable message. A response carrying neither
/// is rejected rather than treated as success.
pub fn parse_response(body: &str) -> Result<Value, String> {
    if body.len() > MAX_MCP_RESPONSE_BYTES as usize {
        return Err("MCP response exceeds the 4 MB limit".into());
    }
    let value: Value =
        serde_json::from_str(body).map_err(|_| "MCP server returned invalid JSON")?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("MCP server returned a message without jsonrpc 2.0".into());
    }
    if let Some(error) = value.get("error") {
        let code = error
            .get("code")
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".into());
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("MCP request failed");
        return Err(format!("MCP error {code}: {message}"));
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| "MCP server returned neither a result nor an error".into())
}

/// Parse a Streamable HTTP response body: either a single JSON object or
/// a `text/event-stream` body whose `data:` lines carry JSON-RPC
/// messages. The last complete response message wins.
pub fn parse_streamable_body(body: &str) -> Result<Value, String> {
    if let Ok(result) = parse_response(body) {
        return Ok(result);
    }
    let mut last: Option<Value> = None;
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(result) = parse_response(data) {
            last = Some(result);
        }
    }
    last.ok_or_else(|| "MCP server returned no usable response".into())
}

// --- Validation ---------------------------------------------------------

fn is_loopback(host: &str) -> bool {
    ["localhost", "127.0.0.1", "[::1]", "::1"].contains(&host)
}

/// Validate a remote server URL. HTTPS is required; plain HTTP is
/// accepted only for loopback hosts. Credentials, query strings and
/// fragments are rejected so secrets can never hide in a stored URL.
pub fn validate_remote_url(raw: &str) -> Result<reqwest::Url, String> {
    if raw.is_empty() || raw.len() > 2048 {
        return Err("Server URL must be 1–2048 characters".into());
    }
    let url = reqwest::Url::parse(raw).map_err(|_| "Server URL is not a valid URL")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Server URL must not contain credentials, a query string, or a fragment".into(),
        );
    }
    let host = url.host_str().ok_or("Server URL needs a host")?;
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_loopback(host) => Ok(url),
        "http" => {
            Err("Remote servers require HTTPS; plain HTTP is accepted only for localhost".into())
        }
        _ => Err("Server URL must use https:// (or http:// for localhost)".into()),
    }
}

/// Validate a stdio server command line. The command is split on
/// whitespace and spawned directly, never through a shell, so shell
/// metacharacters are inert; the checks here only enforce shape.
pub fn validate_stdio_command(raw: &str) -> Result<Vec<String>, String> {
    if raw.trim().is_empty() || raw.len() > 4096 {
        return Err("Server command must be 1–4096 characters".into());
    }
    if raw.chars().any(char::is_control) {
        return Err("Server command must not contain control characters".into());
    }
    let parts: Vec<String> = raw.split_whitespace().map(str::to_owned).collect();
    if parts.is_empty() {
        return Err("Server command must name a program".into());
    }
    Ok(parts)
}

/// Validate a server id (used as the credential-store account name).
/// 1–80 characters, no whitespace or control characters.
pub fn validate_server_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 80 || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Server id must be 1–80 characters without whitespace".into());
    }
    Ok(())
}

/// Validate an API key value: non-empty, at most 8192 characters, no
/// control characters. The value is never logged.
pub fn validate_api_key(key: &str) -> Result<(), String> {
    if key.trim().is_empty() || key.len() > 8192 || key.chars().any(char::is_control) {
        return Err("Invalid credential".into());
    }
    Ok(())
}

// --- Configuration types ------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Http,
    Stdio,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerConfig {
    pub id: String,
    pub name: String,
    pub transport: TransportKind,
    pub url: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub preset_id: Option<String>,
}

impl ServerConfig {
    pub fn validate(&self) -> Result<(), String> {
        validate_server_id(&self.id)?;
        if self.name.trim().is_empty() || self.name.encode_utf16().count() > 120 {
            return Err("Server name must be 1–120 characters".into());
        }
        match self.transport {
            TransportKind::Http => {
                let url = self.url.as_deref().ok_or("A remote server needs a URL")?;
                validate_remote_url(url)?;
            }
            TransportKind::Stdio => {
                let command = self
                    .command
                    .as_deref()
                    .ok_or("A local server needs a command")?;
                validate_stdio_command(command)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTestResult {
    pub server_id: String,
    pub server_name: String,
    pub protocol_version: Option<String>,
    pub server_info: Option<Value>,
    pub tools: Vec<McpTool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaCandidate {
    pub url: String,
    pub kind: String,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub content: Vec<Value>,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// Text content items joined with newlines, for display.
    pub text: String,
    /// Downloadable media URLs found in the result, if any.
    pub media_urls: Vec<MediaCandidate>,
}

fn parse_tool_result(value: Value) -> Result<ToolResult, String> {
    let content = value
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let is_error = value
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let structured_content = value.get("structuredContent").cloned();
    let mut text_parts: Vec<String> = Vec::new();
    for item in &content {
        if item.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                text_parts.push(text.to_owned());
            }
        }
    }
    let text = text_parts.join("\n");
    let mut media_urls = Vec::new();
    scan_text_for_media_urls(&text, &mut media_urls);
    if let Some(structured) = &structured_content {
        collect_media_urls(structured, &mut media_urls);
    }
    // De-duplicate by URL, keeping the first label.
    let mut seen = std::collections::HashSet::new();
    media_urls.retain(|c| seen.insert(c.url.clone()));
    media_urls.truncate(20);
    Ok(ToolResult {
        content,
        is_error,
        structured_content,
        text,
        media_urls,
    })
}

// --- Media URL extraction ------------------------------------------------

const MEDIA_EXTENSIONS: [(&str, &str); 9] = [
    ("mp4", "video"),
    ("mov", "video"),
    ("webm", "video"),
    ("m4v", "video"),
    ("png", "image"),
    ("jpg", "image"),
    ("jpeg", "image"),
    ("webp", "image"),
    ("gif", "image"),
];

fn media_kind_for_url(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let extension = path.rsplit('.').next()?.to_lowercase();
    MEDIA_EXTENSIONS
        .iter()
        .find(|(ext, _)| *ext == extension)
        .map(|(_, kind)| *kind)
}

/// Scan free text for HTTPS URLs pointing at media files.
pub fn scan_text_for_media_urls(text: &str, out: &mut Vec<MediaCandidate>) {
    for start in text.match_indices("https://").map(|(i, _)| i) {
        let rest = &text[start..];
        let end = rest
            .find(|c: char| {
                c.is_whitespace() || ['"', '\'', '<', '>', '`', ')', ']', '}'].contains(&c)
            })
            .unwrap_or(rest.len());
        let mut url = rest[..end]
            .trim_end_matches(['.', ',', ';', ':', '!'])
            .to_owned();
        // A closing parenthesis may belong to surrounding prose.
        while url.ends_with(')') {
            url.pop();
        }
        if url.len() > 2048 {
            continue;
        }
        if let Some(kind) = media_kind_for_url(&url) {
            out.push(MediaCandidate {
                url,
                kind: kind.into(),
                label: None,
            });
        }
    }
}

/// Recursively collect media URLs from a structured tool result.
pub fn collect_media_urls(value: &Value, out: &mut Vec<MediaCandidate>) {
    match value {
        Value::String(text) => scan_text_for_media_urls(text, out),
        Value::Array(items) => {
            for item in items {
                collect_media_urls(item, out);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                if let Value::String(text) = item {
                    let key = key.to_lowercase();
                    if key.contains("url") || key.contains("download") || key.contains("media") {
                        if let Some(kind) = media_kind_for_url(text) {
                            out.push(MediaCandidate {
                                url: text.clone(),
                                kind: kind.into(),
                                label: Some(key),
                            });
                            continue;
                        }
                    }
                }
                collect_media_urls(item, out);
            }
        }
        _ => {}
    }
}

// --- Streamable HTTP transport ------------------------------------------

/// One Streamable HTTP MCP connection. A client is created per command
/// invocation: it runs `initialize`, then the requested operation, and
/// is dropped. The `Mcp-Session-Id` header is honoured within that
/// invocation so session-aware servers work end to end.
pub struct HttpClient {
    url: reqwest::Url,
    api_key: Option<Zeroizing<String>>,
    session_id: Option<String>,
    http: reqwest::blocking::Client,
    next_id: AtomicU64,
}

impl HttpClient {
    pub fn new(
        url: &str,
        api_key: Option<Zeroizing<String>>,
        timeout: Duration,
    ) -> Result<Self, String> {
        let url = validate_remote_url(url)?;
        Ok(HttpClient {
            url,
            api_key,
            session_id: None,
            http: http_client(timeout)?,
            next_id: AtomicU64::new(1),
        })
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// POST one JSON-RPC request and parse the response. Retries once
    /// without the session id when the server reports the session as
    /// gone (HTTP 404 with a session id set).
    fn post(&mut self, request: &Value) -> Result<Value, String> {
        self.post_once(request, true)
    }

    fn post_once(&mut self, request: &Value, with_session: bool) -> Result<Value, String> {
        let mut builder = self
            .http
            .post(self.url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            );
        if let Some(key) = &self.api_key {
            builder = builder.header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", key.as_str()),
            );
        }
        if with_session {
            if let Some(session) = &self.session_id {
                builder = builder.header("mcp-session-id", session.as_str());
            }
        }
        let response = builder
            .json(request)
            .send()
            .map_err(|_| "MCP server connection failed or timed out".to_string())?;
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            if !session.is_empty() {
                self.session_id = Some(session.to_owned());
            }
        }
        if response.status().as_u16() == 404 && with_session && self.session_id.is_some() {
            // The server no longer knows this session: retry once
            // without it, which starts a fresh session.
            self.session_id = None;
            return self.post_once(request, false);
        }
        if response.status().as_u16() == 401 {
            return Err("MCP server rejected the credential (HTTP 401). Check the API key.".into());
        }
        if response.status().as_u16() == 403 {
            return Err(
                "MCP server refused access (HTTP 403). Check the API key and account.".into(),
            );
        }
        if !response.status().is_success() {
            return Err(format!(
                "MCP server returned HTTP {}. Check the server URL.",
                response.status().as_u16()
            ));
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_MCP_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Failed reading MCP response")?;
        if bytes.len() as u64 > MAX_MCP_RESPONSE_BYTES {
            return Err("MCP response exceeds the 4 MB limit".into());
        }
        let text = String::from_utf8_lossy(&bytes);
        parse_streamable_body(&text)
    }

    /// Best-effort notification: no response is expected, and a failure
    /// here must not fail a connection whose `initialize` succeeded.
    fn notify(&mut self, method: &str) {
        let notification = json!({"jsonrpc": "2.0", "method": method});
        let mut builder = self
            .http
            .post(self.url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            );
        if let Some(key) = &self.api_key {
            builder = builder.header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", key.as_str()),
            );
        }
        if let Some(session) = &self.session_id {
            builder = builder.header("mcp-session-id", session.as_str());
        }
        let _ = builder.json(&notification).send();
    }

    pub fn initialize(&mut self) -> Result<Value, String> {
        let id = self.next_request_id();
        let request = build_request(
            id,
            "initialize",
            Some(json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": MCP_CLIENT_NAME, "version": MCP_CLIENT_VERSION},
            })),
        );
        let result = self.post(&request)?;
        self.notify("notifications/initialized");
        Ok(result)
    }

    pub fn list_tools(&mut self) -> Result<Vec<McpTool>, String> {
        let id = self.next_request_id();
        let request = build_request(id, "tools/list", None);
        let result = self.post(&request)?;
        parse_tool_list(&result)
    }

    pub fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<ToolResult, String> {
        validate_tool_name(tool)?;
        let id = self.next_request_id();
        let request = build_request(
            id,
            "tools/call",
            Some(json!({"name": tool, "arguments": arguments})),
        );
        let result = self.post(&request)?;
        parse_tool_result(result)
    }
}

fn parse_tool_list(result: &Value) -> Result<Vec<McpTool>, String> {
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or("MCP server returned no tool list")?;
    if tools.len() > 200 {
        return Err("MCP server listed more than 200 tools".into());
    }
    let mut out = Vec::with_capacity(tools.len());
    for item in tools {
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .ok_or("MCP tool without a name")?;
        if name.is_empty() || name.len() > 128 {
            return Err("MCP tool name is invalid".into());
        }
        out.push(McpTool {
            name: name.to_owned(),
            description: item
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            input_schema: item
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({})),
        });
    }
    Ok(out)
}

fn validate_tool_name(tool: &str) -> Result<(), String> {
    if tool.is_empty()
        || tool.len() > 128
        || !tool
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ['_', '-', '.'].contains(&c))
    {
        return Err("Tool name must be 1–128 letters, digits, underscore, hyphen, or dot".into());
    }
    Ok(())
}

// --- stdio transport ------------------------------------------------------

/// One stdio MCP server process. The child is spawned on connect and
/// terminated when the client is dropped. A reader thread forwards
/// stdout lines to a channel so reads can time out; a timed-out client
/// is marked poisoned and refuses further calls, because a late reply
/// could otherwise be attributed to the wrong request.
pub struct StdioClient {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    next_id: u64,
    poisoned: bool,
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl StdioClient {
    pub fn spawn(command: &str) -> Result<Self, String> {
        let parts = validate_stdio_command(command)?;
        let mut child = Command::new(&parts[0])
            .args(&parts[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Cannot start local MCP server {}: {e}", parts[0]))?;
        let stdin = child.stdin.take().ok_or("Local MCP server has no stdin")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Local MCP server has no stdout")?;
        let (sender, lines) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(StdioClient {
            child,
            stdin,
            lines,
            next_id: 1,
            poisoned: false,
        })
    }

    fn check_alive(&mut self) -> Result<(), String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Err(format!(
                "Local MCP server exited (status {status}); check the command"
            )),
            Ok(None) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
        if self.poisoned {
            return Err("Local MCP server did not answer in time; reconnect".into());
        }
        self.check_alive()?;
        let id = self.next_id;
        self.next_id += 1;
        let request = build_request(id, method, params);
        let line = serde_json::to_string(&request).map_err(|_| "Cannot encode MCP request")?;
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|_| "Cannot write to local MCP server")?;
        match self.lines.recv_timeout(timeout) {
            Ok(line) => {
                if line.len() > MAX_MCP_RESPONSE_BYTES as usize {
                    self.poisoned = true;
                    return Err("MCP response exceeds the 4 MB limit".into());
                }
                parse_response(&line)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.poisoned = true;
                Err("Local MCP server did not answer in time".into())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("Local MCP server closed its output".into())
            }
        }
    }

    fn notify(&mut self, method: &str) {
        let notification = json!({"jsonrpc": "2.0", "method": method});
        if let Ok(line) = serde_json::to_string(&notification) {
            let _ = self.stdin.write_all(line.as_bytes());
            let _ = self.stdin.write_all(b"\n");
            let _ = self.stdin.flush();
        }
    }

    pub fn initialize(&mut self) -> Result<Value, String> {
        let result = self.request(
            "initialize",
            Some(json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": MCP_CLIENT_NAME, "version": MCP_CLIENT_VERSION},
            })),
            Duration::from_secs(30),
        )?;
        self.notify("notifications/initialized");
        Ok(result)
    }

    pub fn list_tools(&mut self) -> Result<Vec<McpTool>, String> {
        let result = self.request("tools/list", None, Duration::from_secs(30))?;
        parse_tool_list(&result)
    }

    pub fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<ToolResult, String> {
        validate_tool_name(tool)?;
        let result = self.request(
            "tools/call",
            Some(json!({"name": tool, "arguments": arguments})),
            Duration::from_secs(120),
        )?;
        parse_tool_result(result)
    }
}

// --- Unified client -------------------------------------------------------

/// Either MCP transport behind one interface. A client is created per
/// operation: `connect` runs `initialize`, then the caller lists tools
/// or calls one tool. stdio servers are therefore spawned fresh for
/// each operation; persistent sessions are future work.
pub enum McpClient {
    Http(HttpClient),
    Stdio(StdioClient),
}

impl McpClient {
    pub fn connect(
        config: &ServerConfig,
        api_key: Option<Zeroizing<String>>,
    ) -> Result<Self, String> {
        config.validate()?;
        match config.transport {
            TransportKind::Http => {
                let mut client = HttpClient::new(
                    config.url.as_deref().unwrap_or(""),
                    api_key,
                    Duration::from_secs(60),
                )?;
                client.initialize()?;
                Ok(McpClient::Http(client))
            }
            TransportKind::Stdio => {
                let mut client = StdioClient::spawn(config.command.as_deref().unwrap_or(""))?;
                client.initialize()?;
                Ok(McpClient::Stdio(client))
            }
        }
    }

    /// Connect and run `initialize` + `tools/list` in one session.
    pub fn test(
        config: &ServerConfig,
        api_key: Option<Zeroizing<String>>,
    ) -> Result<McpTestResult, String> {
        config.validate()?;
        match config.transport {
            TransportKind::Http => {
                let mut client = HttpClient::new(
                    config.url.as_deref().unwrap_or(""),
                    api_key,
                    Duration::from_secs(60),
                )?;
                let init = client.initialize()?;
                let tools = client.list_tools()?;
                Ok(McpTestResult {
                    server_id: config.id.clone(),
                    server_name: config.name.clone(),
                    protocol_version: init
                        .get("protocolVersion")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    server_info: init.get("serverInfo").cloned(),
                    tools,
                })
            }
            TransportKind::Stdio => {
                let mut client = StdioClient::spawn(config.command.as_deref().unwrap_or(""))?;
                let init = client.initialize()?;
                let tools = client.list_tools()?;
                Ok(McpTestResult {
                    server_id: config.id.clone(),
                    server_name: config.name.clone(),
                    protocol_version: init
                        .get("protocolVersion")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    server_info: init.get("serverInfo").cloned(),
                    tools,
                })
            }
        }
    }

    pub fn list_tools(&mut self) -> Result<Vec<McpTool>, String> {
        match self {
            McpClient::Http(client) => client.list_tools(),
            McpClient::Stdio(client) => client.list_tools(),
        }
    }

    pub fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<ToolResult, String> {
        match self {
            McpClient::Http(client) => client.call_tool(tool, arguments),
            McpClient::Stdio(client) => client.call_tool(tool, arguments),
        }
    }
}

// --- Job polling ----------------------------------------------------------

/// Terminal state of an asynchronous generation job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Pending,
    Succeeded,
    Failed,
}

/// Map a provider status string to a job state. Unknown strings stay
/// pending; the poll timeout is the backstop. Covers the Higgsfield
/// statuses (`queued`, `in_progress`, `nsfw`, `failed`, `completed`)
/// and the common HyperFrames-style states.
pub fn job_state_from_status(status: &str) -> JobState {
    match status.trim().to_lowercase().as_str() {
        "completed" | "succeeded" | "success" | "done" | "finished" => JobState::Succeeded,
        "failed" | "error" | "nsfw" | "cancelled" | "canceled" | "rejected" => JobState::Failed,
        _ => JobState::Pending,
    }
}

/// Map a status string when the result already carries media URLs: an
/// otherwise unknown status with a finished file counts as succeeded.
pub fn job_state_from_status_with_media(status: &str, has_media_url: bool) -> JobState {
    let state = job_state_from_status(status);
    if state == JobState::Pending && has_media_url {
        JobState::Succeeded
    } else {
        state
    }
}

/// Wait-loop bookkeeping for polling an asynchronous job. The frontend
/// drives the loop (one status call per tick, so progress stays
/// visible and the user can cancel); this type only owns the timeout
/// and attempt count. The recommended tick is 5 seconds.
pub struct JobPoll {
    pub started: Instant,
    pub timeout: Duration,
    pub attempts: u32,
}

impl JobPoll {
    pub fn new(timeout: Duration) -> Self {
        JobPoll {
            started: Instant::now(),
            timeout,
            attempts: 0,
        }
    }

    /// Record one poll tick and map the reported status. A timed-out
    /// poll reports [`JobState::Failed`] regardless of the status.
    pub fn advance(&mut self, status: &str, has_media_url: bool) -> JobState {
        self.attempts += 1;
        if self.is_expired() {
            return JobState::Failed;
        }
        job_state_from_status_with_media(status, has_media_url)
    }

    pub fn is_expired(&self) -> bool {
        self.started.elapsed() > self.timeout
    }

    pub fn next_delay() -> Duration {
        Duration::from_secs(5)
    }
}

// --- Higgsfield REST API --------------------------------------------------

/// One Higgsfield generation model endpoint, verified against the
/// official API reference. This is a verified subset, not the full
/// catalog; see the note on [`HiggsfieldModelsResponse`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HiggsfieldModel {
    /// Model endpoint path below the API base URL, e.g.
    /// `bytedance/seedance-2.5/image-to-video`.
    pub id: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HiggsfieldModelsResponse {
    pub models: Vec<HiggsfieldModel>,
    pub note: Option<String>,
}

/// The verified subset of the Higgsfield model catalog. Model-specific
/// parameters are documented in the official API reference; the panel
/// sends them as JSON, so no parameter schema is invented here.
pub fn higgsfield_models() -> HiggsfieldModelsResponse {
    HiggsfieldModelsResponse {
        models: vec![
            HiggsfieldModel {
                id: "bytedance/seedance-2.5/image-to-video".into(),
                name: "Seedance 2.5 image-to-video".into(),
                kind: "image-to-video".into(),
                description: "Generate a video from one image URL, with an optional prompt, duration, resolution, and generated audio.".into(),
                note: None,
            },
            HiggsfieldModel {
                id: "flux-pro/kontext/max/text-to-image".into(),
                name: "Flux Pro Kontext Max text-to-image".into(),
                kind: "text-to-image".into(),
                description: "Generate or edit an image from a text prompt.".into(),
                note: None,
            },
            HiggsfieldModel {
                id: "higgsfield/genjutsu/restyle/v1.0".into(),
                name: "Genjutsu video restyle".into(),
                kind: "video-restyle".into(),
                description: "Restyle a source video with a selectable visual style while keeping its motion and composition.".into(),
                note: Some("List styles first with GET /models/higgsfield/genjutsu/restyle/v1.0/presets.".into()),
            },
            HiggsfieldModel {
                id: "higgsfield-ai/soul/v2/standard".into(),
                name: "Soul character video".into(),
                kind: "character-video".into(),
                description: "Generate a character video with the Soul model.".into(),
                note: Some("Endpoint path verified through the official SDK documentation; confirm availability on the account before submitting.".into()),
            },
        ],
        note: Some(
            "Verified subset of the Higgsfield API catalog — see the official Higgsfield API reference for the full model list. Any model endpoint from that reference can be entered directly."
                .into(),
        ),
    }
}

/// Split a `KEY_ID:KEY_SECRET` credential into its two parts.
pub fn parse_higgsfield_key(key: &str) -> Result<(String, String), String> {
    validate_api_key(key)?;
    let (id, secret) = key
        .split_once(':')
        .ok_or("Higgsfield credential must be KEY_ID:KEY_SECRET")?;
    if id.trim().is_empty()
        || secret.trim().is_empty()
        || id.contains(char::is_whitespace)
        || secret.contains(char::is_whitespace)
        || id.contains(':')
        || secret.contains(':')
    {
        return Err("Higgsfield credential must be KEY_ID:KEY_SECRET with no spaces".into());
    }
    Ok((id.to_owned(), secret.to_owned()))
}

/// Validate a Higgsfield model endpoint path: a relative path below the
/// API base URL, with no scheme, host, traversal, or query string.
pub fn validate_higgsfield_endpoint(endpoint: &str) -> Result<String, String> {
    let path = endpoint.trim().trim_start_matches('/');
    if path.is_empty() || path.len() > 200 {
        return Err("Higgsfield model endpoint must be 1–200 characters".into());
    }
    if path.contains("..")
        || path.contains('\\')
        || path.contains('?')
        || path.contains('#')
        || path.contains(':')
        || !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ['/', '-', '_', '.'].contains(&c))
    {
        return Err("Higgsfield model endpoint must be a plain model path such as bytedance/seedance-2.5/image-to-video".into());
    }
    Ok(path.to_owned())
}

fn higgsfield_auth_header(key: &Zeroizing<String>) -> Result<String, String> {
    let (id, secret) = parse_higgsfield_key(key.as_str())?;
    Ok(format!("Key {id}:{secret}"))
}

fn higgsfield_request_id(request_id: &str) -> Result<(), String> {
    if request_id.is_empty()
        || request_id.len() > 128
        || !request_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ['-', '_'].contains(&c))
    {
        return Err("Request id must be 1–128 letters, digits, hyphen, or underscore".into());
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HiggsfieldJob {
    pub request_id: String,
    pub status_url: Option<String>,
    pub cancel_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HiggsfieldStatus {
    pub request_id: String,
    pub status: String,
    pub state: JobState,
    pub media_urls: Vec<MediaCandidate>,
    /// Full status response, for inspection in the panel.
    pub detail: Value,
}

fn higgsfield_get(key: &Zeroizing<String>, url: &str) -> Result<Value, String> {
    let client = http_client(Duration::from_secs(60))?;
    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, higgsfield_auth_header(key)?)
        .send()
        .map_err(|_| "Higgsfield request failed or timed out".to_string())?;
    if response.status().as_u16() == 401 {
        return Err(
            "Higgsfield rejected the credential (HTTP 401). Check the KEY_ID:KEY_SECRET pair."
                .into(),
        );
    }
    if !response.status().is_success() {
        return Err(format!(
            "Higgsfield returned HTTP {}. Check the credential, model, and account balance.",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_MCP_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Failed reading Higgsfield response")?;
    if bytes.len() as u64 > MAX_MCP_RESPONSE_BYTES {
        return Err("Higgsfield response exceeds the 4 MB limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Higgsfield returned invalid JSON".into())
}

/// Submit a generation request to a Higgsfield model endpoint.
/// Returns the request id plus the status and cancel URLs.
pub fn higgsfield_submit(
    key: &Zeroizing<String>,
    endpoint: &str,
    params: Value,
) -> Result<HiggsfieldJob, String> {
    let path = validate_higgsfield_endpoint(endpoint)?;
    if !params.is_object() {
        return Err("Generation parameters must be a JSON object".into());
    }
    let client = http_client(Duration::from_secs(60))?;
    let response = client
        .post(format!("{HIGGSFIELD_API_BASE}/{path}"))
        .header(reqwest::header::AUTHORIZATION, higgsfield_auth_header(key)?)
        .json(&params)
        .send()
        .map_err(|_| "Higgsfield request failed or timed out".to_string())?;
    if response.status().as_u16() == 401 {
        return Err(
            "Higgsfield rejected the credential (HTTP 401). Check the KEY_ID:KEY_SECRET pair."
                .into(),
        );
    }
    if !response.status().is_success() {
        return Err(format!(
            "Higgsfield returned HTTP {}. Check the credential, model, and account balance.",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_MCP_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Failed reading Higgsfield response")?;
    if bytes.len() as u64 > MAX_MCP_RESPONSE_BYTES {
        return Err("Higgsfield response exceeds the 4 MB limit".into());
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Higgsfield returned invalid JSON")?;
    let request_id = value
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or("Higgsfield returned no request id")?;
    higgsfield_request_id(request_id)?;
    Ok(HiggsfieldJob {
        request_id: request_id.to_owned(),
        status_url: value
            .get("status_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cancel_url: value
            .get("cancel_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// Poll one Higgsfield request. The caller repeats this call until the
/// state is terminal; see [`JobPoll`].
pub fn higgsfield_status(
    key: &Zeroizing<String>,
    request_id: &str,
) -> Result<HiggsfieldStatus, String> {
    higgsfield_request_id(request_id)?;
    let value = higgsfield_get(
        key,
        &format!("{HIGGSFIELD_API_BASE}/requests/{request_id}/status"),
    )?;
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let mut media_urls = Vec::new();
    collect_media_urls(&value, &mut media_urls);
    let mut seen = std::collections::HashSet::new();
    media_urls.retain(|c| seen.insert(c.url.clone()));
    media_urls.truncate(20);
    let state = job_state_from_status_with_media(&status, !media_urls.is_empty());
    Ok(HiggsfieldStatus {
        request_id: request_id.to_owned(),
        status,
        state,
        media_urls,
        detail: value,
    })
}

/// Cancel a queued Higgsfield request. Requests already in progress
/// cannot be cancelled; the API reports that as an error.
pub fn higgsfield_cancel(key: &Zeroizing<String>, request_id: &str) -> Result<(), String> {
    higgsfield_request_id(request_id)?;
    let client = http_client(Duration::from_secs(60))?;
    let response = client
        .post(format!(
            "{HIGGSFIELD_API_BASE}/requests/{request_id}/cancel"
        ))
        .header(reqwest::header::AUTHORIZATION, higgsfield_auth_header(key)?)
        .send()
        .map_err(|_| "Higgsfield request failed or timed out".to_string())?;
    if response.status().as_u16() == 401 {
        return Err(
            "Higgsfield rejected the credential (HTTP 401). Check the KEY_ID:KEY_SECRET pair."
                .into(),
        );
    }
    if !response.status().is_success() {
        return Err(format!(
            "Higgsfield returned HTTP {}. The request may already be in progress.",
            response.status().as_u16()
        ));
    }
    Ok(())
}

// --- Download -------------------------------------------------------------

/// Validate a download file name: a plain file name (no directories)
/// with a media extension.
pub fn validate_download_file_name(file_name: &str) -> Result<&'static str, String> {
    if file_name.is_empty() || file_name.len() > 120 {
        return Err("File name must be 1–120 characters".into());
    }
    if file_name.contains('/')
        || file_name.contains('\\')
        || file_name.contains("..")
        || file_name.chars().any(char::is_control)
    {
        return Err("File name must be a plain file name without directories".into());
    }
    let extension = file_name
        .rsplit('.')
        .next()
        .map(str::to_lowercase)
        .unwrap_or_default();
    MEDIA_EXTENSIONS
        .iter()
        .find(|(ext, _)| *ext == extension)
        .map(|(_, kind)| *kind)
        .ok_or_else(|| {
            "File name needs a media extension (.mp4, .mov, .webm, .png, .jpg, .jpeg, .webp, .gif)"
                .into()
        })
}

/// Download a generated media file over HTTPS into the app data
/// directory (`mcp-downloads`). Signed result URLs may carry a query
/// string; the URL is never logged. Returns the local path. The caller
/// then imports it with the existing `import_media` command.
pub fn download_result(
    url: &str,
    file_name: &str,
    data_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    validate_download_file_name(file_name)?;
    let parsed = reqwest::Url::parse(url).map_err(|_| "Result URL is not a valid URL")?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err("Result downloads require an HTTPS URL".into());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "Cannot initialize secure HTTP client".to_string())?;
    let response = client
        .get(parsed)
        .send()
        .map_err(|_| "Result download failed or timed out".to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Result download returned HTTP {}. The link may have expired.",
            response.status().as_u16()
        ));
    }
    if let Some(content_type) = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        let content_type = content_type.to_lowercase();
        if content_type.starts_with("text/html") || content_type.starts_with("application/json") {
            return Err("Result download returned a page, not media".into());
        }
    }
    if let Some(length) = response.content_length() {
        if length > MAX_DOWNLOAD_BYTES {
            return Err("Result exceeds the 500 MB download limit".into());
        }
    }
    let dir = data_dir.join("mcp-downloads");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(file_name);
    if target.exists() {
        return Err("A downloaded file with this name already exists; choose another name".into());
    }
    let mut staging = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
    let copied = std::io::copy(&mut response.take(MAX_DOWNLOAD_BYTES + 1), &mut staging)
        .map_err(|_| "Result download was interrupted".to_string())?;
    if copied > MAX_DOWNLOAD_BYTES {
        return Err("Result exceeds the 500 MB download limit".into());
    }
    if copied == 0 {
        return Err("Result download returned no data".into());
    }
    staging.as_file().sync_all().map_err(|e| e.to_string())?;
    staging.persist(&target).map_err(|e| e.to_string())?;
    Ok(target)
}

// --- Presets --------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PresetKind {
    /// Higgsfield REST API (primary path for that preset).
    RestHiggsfield,
    /// A remote MCP server over Streamable HTTP.
    McpHttp,
    /// A local MCP server over stdio.
    McpStdio,
    /// A user-defined server (URL or stdio command).
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthKind {
    None,
    ApiKey,
    KeySecret,
    Oauth,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPreset {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: PresetKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub auth: AuthKind,
    /// `false` when the preset cannot be used yet (OAuth not
    /// implemented). Listed so the panel can explain why, not as a
    /// working option.
    pub available: bool,
    /// Tool names verified for this server. Empty when the tool list is
    /// discovered at connect time with `tools/list`.
    pub tools: Vec<String>,
    pub notes: Vec<String>,
}

/// Built-in presets. Only endpoints and tools verified against vendor
/// documentation are listed; nothing here is invented.
pub fn builtin_presets() -> Vec<McpPreset> {
    vec![
        McpPreset {
            id: "higgsfield-rest".into(),
            name: "Higgsfield (REST API)".into(),
            description: "Generate images and video with the official Higgsfield REST API, authenticated with a KEY_ID:KEY_SECRET pair from the Higgsfield Console. This is the primary Higgsfield path in StudioCut AI.".into(),
            kind: PresetKind::RestHiggsfield,
            url: Some(HIGGSFIELD_API_BASE.into()),
            command: None,
            auth: AuthKind::KeySecret,
            available: true,
            tools: vec![],
            notes: vec![
                "Billing is pay-per-generation against a prepaid dollar balance, separate from Higgsfield plan credits.".into(),
                "Generations are asynchronous: submit, then poll the request status until it completes.".into(),
                "Generated files remain available for at least 7 days after creation.".into(),
            ],
        },
        McpPreset {
            id: "higgsfield-mcp".into(),
            name: "Higgsfield (hosted MCP)".into(),
            description: "The official hosted Higgsfield MCP server. It signs in with a Higgsfield account through browser OAuth and spends plan credits.".into(),
            kind: PresetKind::McpHttp,
            url: Some(HIGGSFIELD_MCP_URL.into()),
            command: None,
            auth: AuthKind::Oauth,
            available: false,
            tools: vec![],
            notes: vec![
                "OAuth sign-in is not implemented in this release, so this preset cannot connect yet.".into(),
                "Use the Higgsfield REST API preset instead; it covers the same generation models.".into(),
            ],
        },
        McpPreset {
            id: "hyperframes".into(),
            name: "HyperFrames".into(),
            description: "HyperFrames hosted workspace: text-to-video, image-to-video, reference-to-video, text-to-image, and image-to-image generation over the remote MCP server, authenticated with an API key.".into(),
            kind: PresetKind::McpHttp,
            url: Some(HYPERFRAMES_MCP_URL.into()),
            command: None,
            auth: AuthKind::ApiKey,
            available: true,
            tools: vec![
                "list_models".into(),
                "get_credits".into(),
                "generate_video".into(),
                "generate_image".into(),
                "get_generation".into(),
            ],
            notes: vec![
                "Video duration 4–15 seconds; resolutions 480p and 720p.".into(),
                "Video generation is asynchronous: generate_video returns a task id, then poll get_generation until the media URL is ready.".into(),
                "This is the independently operated hosted workspace at hyperframes.net, not HeyGen's open-source HyperFrames HTML-to-video CLI.".into(),
            ],
        },
        McpPreset {
            id: "custom".into(),
            name: "Custom server".into(),
            description: "Any other MCP server: a remote Streamable HTTP endpoint (HTTPS, optional API key) or a local server started from a command (stdio).".into(),
            kind: PresetKind::Custom,
            url: None,
            command: None,
            auth: AuthKind::ApiKey,
            available: true,
            tools: vec![],
            notes: vec![
                "Remote servers require HTTPS; plain HTTP is accepted only for localhost.".into(),
                "The API key is optional and is sent as a Bearer token when provided.".into(),
                "Local servers run as a child process of StudioCut AI and are terminated after each operation.".into(),
            ],
        },
    ]
}

pub fn preset_by_id(id: &str) -> Option<McpPreset> {
    builtin_presets().into_iter().find(|p| p.id == id)
}

// --- Credentials ----------------------------------------------------------

/// MCP credential storage, mirroring the Director BYOK pattern in
/// `credentials.rs`: OS credential store (`keyring`, service
/// `app.studiocut.mcp`, account = server id) or session-only memory.
/// Keys are never written to project files or preferences.
#[derive(Default)]
pub struct McpCredentials(Mutex<HashMap<String, Zeroizing<String>>>);

impl McpCredentials {
    fn entry(server_id: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new("app.studiocut.mcp", server_id).map_err(|_| {
            "OS credential storage unavailable; explicitly choose session-only storage".into()
        })
    }

    pub fn set(&self, server_id: &str, key: String, storage: &str) -> Result<(), String> {
        validate_server_id(server_id)?;
        let key = Zeroizing::new(key);
        validate_api_key(&key)?;
        match storage {
            "os" => {
                Self::entry(server_id)?.set_password(&key).map_err(|_| {
                    "OS credential storage unavailable; explicitly choose session-only storage"
                })?;
                self.0
                    .lock()
                    .map_err(|_| "Credential lock unavailable")?
                    .remove(server_id);
            }
            "session" => {
                self.0
                    .lock()
                    .map_err(|_| "Credential lock unavailable")?
                    .insert(server_id.to_owned(), key);
            }
            _ => return Err("Credential storage must be \"os\" or \"session\"".into()),
        }
        Ok(())
    }

    pub fn get(&self, server_id: &str) -> Result<Zeroizing<String>, String> {
        validate_server_id(server_id)?;
        if let Some(key) = self
            .0
            .lock()
            .map_err(|_| "Credential lock unavailable")?
            .get(server_id)
        {
            return Ok(key.clone());
        }
        if let Ok(entry) = Self::entry(server_id) {
            if let Ok(key) = entry.get_password() {
                return Ok(Zeroizing::new(key));
            }
        }
        Err(format!(
            "No credential saved for server {server_id}. Save one in OS storage or explicitly select session-only."
        ))
    }

    pub fn delete(&self, server_id: &str) -> Result<(), String> {
        validate_server_id(server_id)?;
        self.0
            .lock()
            .map_err(|_| "Credential lock unavailable")?
            .remove(server_id);
        // The session entry above is already gone. The OS entry is
        // best-effort: when no OS credential exists (or no OS store is
        // available at all) there is nothing to delete, so only a failed
        // delete of an entry that actually exists is reported.
        let Ok(entry) = Self::entry(server_id) else {
            return Ok(());
        };
        if entry.get_password().is_err() {
            return Ok(());
        }
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err("Session credential cleared, but OS deletion failed.".into()),
        }
    }
}

/// Resolve the credential for one call: a per-call key wins (used for
/// that call only, never stored); otherwise the stored credential for
/// the server id is used.
pub fn resolve_key(
    credentials: &McpCredentials,
    server_id: &str,
    per_call: Option<String>,
) -> Result<Zeroizing<String>, String> {
    if let Some(key) = per_call {
        if !key.trim().is_empty() {
            validate_api_key(&key)?;
            return Ok(Zeroizing::new(key));
        }
    }
    credentials.get(server_id)
}

/// Validate an optional per-call key for servers that do not require
/// authentication. An absent or blank key means no credential is sent.
fn optional_key(per_call: Option<String>) -> Result<Option<Zeroizing<String>>, String> {
    match per_call {
        Some(key) if !key.trim().is_empty() => {
            validate_api_key(&key)?;
            Ok(Some(Zeroizing::new(key)))
        }
        _ => Ok(None),
    }
}

// --- Tauri commands -------------------------------------------------------
//
// Each command runs network or process work on a blocking thread, as in
// `main.rs`. The coordinator registers these in `invoke_handler` (see
// the report) and manages `McpCredentials` state.

/// List the built-in presets. No network, no credentials.
#[tauri::command]
pub fn mcp_list_presets() -> Vec<McpPreset> {
    builtin_presets()
}

/// List the verified Higgsfield model catalog. No network, no credentials.
#[tauri::command]
pub fn mcp_higgsfield_models() -> HiggsfieldModelsResponse {
    higgsfield_models()
}

/// Save (`key` set) or forget (`key` null) a server credential.
/// `storage` is `"os"` or `"session"` and is required when saving.
#[tauri::command]
pub async fn mcp_set_credential(
    app: tauri::AppHandle,
    server_id: String,
    key: Option<String>,
    storage: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let state = app.state::<McpCredentials>();
        match key {
            Some(key) => state.set(
                &server_id,
                key,
                storage
                    .as_deref()
                    .ok_or("Choose OS or session-only storage")?,
            ),
            None => state.delete(&server_id),
        }
    })
    .await
    .map_err(|_| "Credential task failed".to_string())?
}

/// Connect to a server, run `initialize` and `tools/list`, and return
/// the server information and tool list. The server is disconnected
/// afterwards; use `mcp_call_tool` for individual tool calls.
#[tauri::command]
pub async fn mcp_test_connection(
    app: tauri::AppHandle,
    server: ServerConfig,
    api_key: Option<String>,
) -> Result<McpTestResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let credentials = app.state::<McpCredentials>();
        let key = if server_needs_auth(&server) {
            Some(resolve_key(&credentials, &server.id, api_key)?)
        } else {
            optional_key(api_key)?
        };
        McpClient::test(&server, key)
    })
    .await
    .map_err(|_| "MCP connection task failed".to_string())?
}

/// List the tools of a server (one fresh `initialize` + `tools/list`
/// session per call).
#[tauri::command]
pub async fn mcp_list_tools(
    app: tauri::AppHandle,
    server: ServerConfig,
    api_key: Option<String>,
) -> Result<Vec<McpTool>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let credentials = app.state::<McpCredentials>();
        let key = if server_needs_auth(&server) {
            Some(resolve_key(&credentials, &server.id, api_key)?)
        } else {
            optional_key(api_key)?
        };
        let mut client = McpClient::connect(&server, key)?;
        client.list_tools()
    })
    .await
    .map_err(|_| "MCP tool list task failed".to_string())?
}

/// Call one tool on a server. `arguments` must be a JSON object.
/// Long-running tools return immediately with a task or request id;
/// poll them with the provider's status tool (HyperFrames:
/// `get_generation`) or the Higgsfield status command.
#[tauri::command]
pub async fn mcp_call_tool(
    app: tauri::AppHandle,
    server: ServerConfig,
    tool: String,
    arguments: Value,
    api_key: Option<String>,
) -> Result<ToolResult, String> {
    if !arguments.is_object() {
        return Err("Tool arguments must be a JSON object".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let credentials = app.state::<McpCredentials>();
        let key = if server_needs_auth(&server) {
            Some(resolve_key(&credentials, &server.id, api_key)?)
        } else {
            optional_key(api_key)?
        };
        let mut client = McpClient::connect(&server, key)?;
        client.call_tool(&tool, arguments)
    })
    .await
    .map_err(|_| "MCP tool call task failed".to_string())?
}

/// Whether a server configuration is expected to need a credential.
/// Presets with OAuth or key authentication need one; a custom server
/// may work without one, in which case a stored key is used only when
/// present.
fn server_needs_auth(server: &ServerConfig) -> bool {
    match server.preset_id.as_deref() {
        Some("higgsfield-mcp") | Some("hyperframes") => true,
        Some("higgsfield-rest") => false,
        _ => false,
    }
}

/// Submit a Higgsfield generation request. `endpoint` is a model path
/// such as `bytedance/seedance-2.5/image-to-video`; see
/// `mcp_higgsfield_models` for the verified catalog.
#[tauri::command]
pub async fn mcp_higgsfield_submit(
    app: tauri::AppHandle,
    endpoint: String,
    params: Value,
    api_key: Option<String>,
) -> Result<HiggsfieldJob, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let credentials = app.state::<McpCredentials>();
        let key = resolve_key(&credentials, HIGGSFIELD_CREDENTIAL_ID, api_key)?;
        higgsfield_submit(&key, &endpoint, params)
    })
    .await
    .map_err(|_| "Higgsfield submit task failed".to_string())?
}

/// Check one Higgsfield request. Poll until `state` is `succeeded` or
/// `failed`; the recommended interval is 5 seconds.
#[tauri::command]
pub async fn mcp_higgsfield_status(
    app: tauri::AppHandle,
    request_id: String,
    api_key: Option<String>,
) -> Result<HiggsfieldStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let credentials = app.state::<McpCredentials>();
        let key = resolve_key(&credentials, HIGGSFIELD_CREDENTIAL_ID, api_key)?;
        higgsfield_status(&key, &request_id)
    })
    .await
    .map_err(|_| "Higgsfield status task failed".to_string())?
}

/// Cancel a queued Higgsfield request.
#[tauri::command]
pub async fn mcp_higgsfield_cancel(
    app: tauri::AppHandle,
    request_id: String,
    api_key: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let credentials = app.state::<McpCredentials>();
        let key = resolve_key(&credentials, HIGGSFIELD_CREDENTIAL_ID, api_key)?;
        higgsfield_cancel(&key, &request_id)
    })
    .await
    .map_err(|_| "Higgsfield cancel task failed".to_string())?
}

/// Download a generated media file over HTTPS into the app data
/// directory and return its local path. Import it afterwards with the
/// existing `import_media` command.
#[tauri::command]
pub async fn mcp_download_result(
    app: tauri::AppHandle,
    url: String,
    file_name: String,
) -> Result<String, String> {
    use tauri::Manager;
    let data_dir = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        download_result(&url, &file_name, &data_dir).map(|path| path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|_| "Download task failed".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_building_and_response_parsing() {
        let request = build_request(7, "tools/list", None);
        assert_eq!(request["jsonrpc"], "2.0");
        assert_eq!(request["id"], 7);
        assert_eq!(request["method"], "tools/list");
        assert!(request.get("params").is_none());
        let with_params = build_request(8, "tools/call", Some(json!({"name": "x"})));
        assert_eq!(with_params["params"]["name"], "x");
        let result = parse_response(r#"{"jsonrpc":"2.0","id":7,"result":{"tools":[]}}"#).unwrap();
        assert_eq!(result, json!({"tools": []}));
        let error = parse_response(
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"Method not found"}}"#,
        );
        assert!(error.unwrap_err().contains("Method not found"));
        assert!(parse_response(r#"{"jsonrpc":"2.0","id":7}"#).is_err());
        assert!(parse_response(r#"{"jsonrpc":"1.0","id":7,"result":{}}"#).is_err());
        assert!(parse_response("not json").is_err());
    }

    #[test]
    fn streamable_body_parses_plain_json_and_sse() {
        let plain =
            parse_streamable_body(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).unwrap();
        assert_eq!(plain["ok"], true);
        let sse = parse_streamable_body(
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n",
        )
        .unwrap();
        assert_eq!(sse["tools"], json!([]));
        assert!(parse_streamable_body("event: ping\n\n").is_err());
    }

    #[test]
    fn url_validation_requires_https_except_loopback() {
        assert!(validate_remote_url("https://hyperframes.net/api/mcp").is_ok());
        assert!(validate_remote_url("http://localhost:8080/mcp").is_ok());
        assert!(validate_remote_url("http://127.0.0.1:8080/mcp").is_ok());
        assert!(validate_remote_url("http://example.com/mcp").is_err());
        assert!(validate_remote_url("https://user:pass@example.com/mcp").is_err());
        assert!(validate_remote_url("https://example.com/mcp?token=x").is_err());
        assert!(validate_remote_url("https://example.com/mcp#frag").is_err());
        assert!(validate_remote_url("ftp://example.com/mcp").is_err());
        assert!(validate_remote_url("").is_err());
    }

    #[test]
    fn presets_are_verified_and_honest_about_oauth() {
        let presets = builtin_presets();
        assert_eq!(presets.len(), 4);
        let rest = preset_by_id("higgsfield-rest").unwrap();
        assert!(rest.available);
        assert_eq!(rest.url.as_deref(), Some(HIGGSFIELD_API_BASE));
        assert_eq!(rest.auth, AuthKind::KeySecret);
        let mcp = preset_by_id("higgsfield-mcp").unwrap();
        assert!(!mcp.available);
        assert_eq!(mcp.auth, AuthKind::Oauth);
        assert!(mcp.notes.iter().any(|n| n.contains("OAuth")));
        let hyper = preset_by_id("hyperframes").unwrap();
        assert!(hyper.available);
        assert_eq!(hyper.url.as_deref(), Some(HYPERFRAMES_MCP_URL));
        assert!(hyper.tools.contains(&"generate_video".to_string()));
        assert!(hyper.tools.contains(&"get_generation".to_string()));
        assert!(preset_by_id("custom").unwrap().available);
        assert!(preset_by_id("unknown").is_none());
    }

    #[test]
    fn job_poll_state_machine() {
        assert_eq!(job_state_from_status("queued"), JobState::Pending);
        assert_eq!(job_state_from_status("in_progress"), JobState::Pending);
        assert_eq!(job_state_from_status("completed"), JobState::Succeeded);
        assert_eq!(job_state_from_status("nsfw"), JobState::Failed);
        assert_eq!(job_state_from_status("failed"), JobState::Failed);
        assert_eq!(job_state_from_status("something-new"), JobState::Pending);
        assert_eq!(
            job_state_from_status_with_media("unknown", true),
            JobState::Succeeded
        );
        assert_eq!(
            job_state_from_status_with_media("unknown", false),
            JobState::Pending
        );
        let mut poll = JobPoll::new(Duration::from_secs(60));
        assert_eq!(poll.advance("queued", false), JobState::Pending);
        assert_eq!(poll.attempts, 1);
        assert_eq!(poll.advance("completed", false), JobState::Succeeded);
        let mut expired = JobPoll::new(Duration::from_nanos(1));
        std::thread::sleep(Duration::from_millis(2));
        assert!(expired.is_expired());
        assert_eq!(expired.advance("completed", false), JobState::Failed);
    }

    #[test]
    fn media_urls_are_extracted_and_deduplicated() {
        let result = parse_tool_result(json!({
            "content": [
                {"type": "text", "text": "Done: https://cdn.example.com/clip.mp4 and https://cdn.example.com/frame.png."},
                {"type": "text", "text": "Also https://cdn.example.com/clip.mp4 again."}
            ],
            "structuredContent": {"video_url": "https://cdn.example.com/other.mov", "note": "no media here"},
            "isError": false
        }))
        .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.media_urls.len(), 3);
        assert!(result
            .media_urls
            .iter()
            .any(|c| c.kind == "video" && c.url.ends_with("clip.mp4")));
        assert!(result.media_urls.iter().any(|c| c.kind == "image"));
        let none =
            parse_tool_result(json!({"content": [{"type": "text", "text": "No files."}]})).unwrap();
        assert!(none.media_urls.is_empty());
        let error = parse_tool_result(json!({"content": [], "isError": true})).unwrap();
        assert!(error.is_error);
    }

    #[test]
    fn higgsfield_key_and_endpoint_validation() {
        assert_eq!(
            parse_higgsfield_key("abc123:secret-value").unwrap(),
            ("abc123".to_string(), "secret-value".to_string())
        );
        assert!(parse_higgsfield_key("no-colon").is_err());
        assert!(parse_higgsfield_key("id:").is_err());
        assert!(parse_higgsfield_key(":secret").is_err());
        assert!(parse_higgsfield_key("id :secret").is_err());
        assert_eq!(
            validate_higgsfield_endpoint("/bytedance/seedance-2.5/image-to-video").unwrap(),
            "bytedance/seedance-2.5/image-to-video"
        );
        assert!(validate_higgsfield_endpoint("https://api.higgsfield.ai/x").is_err());
        assert!(validate_higgsfield_endpoint("../escape").is_err());
        assert!(validate_higgsfield_endpoint("model?x=1").is_err());
        let models = higgsfield_models();
        assert!(models
            .models
            .iter()
            .any(|m| m.id == "bytedance/seedance-2.5/image-to-video"));
        assert!(models.note.is_some());
    }

    #[test]
    fn server_config_validation() {
        let http = ServerConfig {
            id: "hyperframes-1".into(),
            name: "HyperFrames".into(),
            transport: TransportKind::Http,
            url: Some(HYPERFRAMES_MCP_URL.into()),
            command: None,
            preset_id: Some("hyperframes".into()),
        };
        assert!(http.validate().is_ok());
        let bad_url = ServerConfig {
            url: Some("http://example.com/mcp".into()),
            ..http.clone()
        };
        assert!(bad_url.validate().is_err());
        let stdio = ServerConfig {
            id: "local-1".into(),
            name: "Local".into(),
            transport: TransportKind::Stdio,
            url: None,
            command: Some("npx -y my-mcp-server".into()),
            preset_id: Some("custom".into()),
        };
        assert!(stdio.validate().is_ok());
        let no_command = ServerConfig {
            command: None,
            ..stdio
        };
        assert!(no_command.validate().is_err());
    }

    #[test]
    fn session_credentials_round_trip_without_os_store() {
        let store = McpCredentials::default();
        store
            .set("server-1", "test-only-key".into(), "session")
            .unwrap();
        assert_eq!(&**store.get("server-1").unwrap(), "test-only-key");
        assert!(store.get("server-2").is_err());
        assert!(store.set("server-1", "bad\nkey".into(), "session").is_err());
        assert!(store.set("server-1", "key".into(), "cloud").is_err());
        store.delete("server-1").unwrap();
        assert!(store.get("server-1").is_err());
        let per_call = resolve_key(&store, "server-1", Some("one-off".into())).unwrap();
        assert_eq!(&*per_call, "one-off");
    }

    #[test]
    fn download_file_name_validation() {
        assert_eq!(validate_download_file_name("clip.mp4").unwrap(), "video");
        assert_eq!(validate_download_file_name("frame.PNG").unwrap(), "image");
        assert!(validate_download_file_name("../clip.mp4").is_err());
        assert!(validate_download_file_name("clip").is_err());
        assert!(validate_download_file_name("clip.txt").is_err());
        assert!(validate_download_file_name("a/b.mp4").is_err());
    }
}
