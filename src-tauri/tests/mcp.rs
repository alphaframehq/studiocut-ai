//! MCP integration tests: pure unit tests for the public API in
//! `studiocut_ai::mcp`. No network calls, no spawned processes, no OS
//! credential store access (session-only credentials only).
//!
//! NOTE FOR THE INTEGRATOR: `src-tauri/Cargo.toml` lists explicit
//! `[[test]]` targets, which disables Cargo's automatic discovery of
//! `tests/*.rs`. This file runs in CI only after the following entry is
//! added to that manifest:
//!
//! ```toml
//! [[test]]
//! name = "mcp"
//! path = "tests/mcp.rs"
//! ```
//!
//! The same pure functions are also covered by the `#[cfg(test)]`
//! module inside `src/mcp.rs`, which runs with `cargo test --lib`
//! without any manifest change.

use serde_json::{json, Value};
use std::time::Duration;
use studiocut_ai::mcp::{
    build_oauth_authorization_url, build_refresh_token_body, build_token_exchange_body,
    builtin_presets, collect_media_urls, form_fields_from_schema, generate_pkce_pair,
    higgsfield_models, job_state_from_status, job_state_from_status_with_media,
    parse_higgsfield_key, parse_response, parse_streamable_body, percent_encode_form_value,
    pkce_challenge, preset_by_id, scan_text_for_media_urls, validate_api_key,
    validate_arguments_against_schema, validate_download_file_name, validate_higgsfield_endpoint,
    validate_hyperframes_arguments, validate_oauth_redirect_uri, validate_remote_url,
    validate_server_id, validate_stdio_command, AuthKind, JobPoll, JobState, McpCredentials,
    PresetKind, ServerConfig, TransportKind,
};

fn http_server(id: &str, url: &str) -> ServerConfig {
    ServerConfig {
        id: id.into(),
        name: "Test server".into(),
        transport: TransportKind::Http,
        url: Some(url.into()),
        command: None,
        preset_id: None,
    }
}

#[test]
fn json_rpc_request_ids_and_error_shapes() {
    let result =
        parse_response(r#"{"jsonrpc":"2.0","id":3,"result":{"protocolVersion":"2025-03-26"}}"#)
            .unwrap();
    assert_eq!(result["protocolVersion"], "2025-03-26");
    let error = parse_response(
        r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"Invalid params","data":{"field":"name"}}}"#,
    )
    .unwrap_err();
    assert!(error.contains("-32602"));
    assert!(error.contains("Invalid params"));
    // A notification-style message (no id, no result) is not a response.
    assert!(parse_response(r#"{"jsonrpc":"2.0","method":"notifications/progress"}"#).is_err());
    // Oversized bodies are rejected before parsing.
    let big = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{{\"text\":\"{}\"}}}}",
        "x".repeat(5_000_000)
    );
    assert!(parse_response(&big).is_err());
}

#[test]
fn streamable_http_accepts_sse_and_ignores_events() {
    let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[{\"name\":\"generate_video\"}]}}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n";
    let result = parse_streamable_body(body).unwrap();
    assert_eq!(result["tools"], json!([]));
    // A data line that is not JSON is skipped, not fatal.
    let mixed = "data: not-json\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n";
    assert_eq!(parse_streamable_body(mixed).unwrap()["ok"], true);
    assert!(parse_streamable_body("").is_err());
}

#[test]
fn remote_url_rules_cover_credentials_query_fragment_and_scheme() {
    assert!(validate_remote_url("https://mcp.higgsfield.ai/mcp").is_ok());
    assert!(validate_remote_url("https://hyperframes.net/api/mcp").is_ok());
    assert!(validate_remote_url("http://[::1]:8080/mcp").is_ok());
    assert!(validate_remote_url("http://localhost/mcp").is_ok());
    assert!(validate_remote_url("https://example.com:8443/a/b").is_ok());
    assert!(validate_remote_url("http://example.com/mcp").is_err());
    assert!(validate_remote_url("https://example.com/mcp?api_key=secret").is_err());
    assert!(validate_remote_url("https://example.com/mcp#section").is_err());
    assert!(validate_remote_url("https://token@example.com/mcp").is_err());
    assert!(validate_remote_url("https://example.com").is_ok());
    assert!(validate_remote_url("not a url").is_err());
    assert!(
        validate_remote_url(&("https://example.com/".to_string() + &"a".repeat(3000))).is_err()
    );
}

#[test]
fn stdio_command_rules() {
    assert_eq!(
        validate_stdio_command("npx -y @example/mcp-server --port 3000").unwrap(),
        vec!["npx", "-y", "@example/mcp-server", "--port", "3000"]
    );
    assert_eq!(
        validate_stdio_command("/usr/local/bin/server").unwrap(),
        vec!["/usr/local/bin/server"]
    );
    assert!(validate_stdio_command("").is_err());
    assert!(validate_stdio_command("   ").is_err());
    assert!(validate_stdio_command("server\ninjected").is_err());
    assert!(validate_stdio_command(&"a".repeat(5000)).is_err());
}

#[test]
fn server_ids_and_api_keys_are_validated() {
    assert!(validate_server_id("hyperframes-1").is_ok());
    assert!(validate_server_id("").is_err());
    assert!(validate_server_id("has space").is_err());
    assert!(validate_server_id(&"x".repeat(81)).is_err());
    assert!(validate_api_key("sk-live-abc123").is_ok());
    assert!(validate_api_key("").is_err());
    assert!(validate_api_key("   ").is_err());
    assert!(validate_api_key("key\nwith-newline").is_err());
    assert!(validate_api_key(&"k".repeat(8193)).is_err());
}

#[test]
fn server_config_validation_by_transport() {
    assert!(http_server("s1", "https://hyperframes.net/api/mcp")
        .validate()
        .is_ok());
    assert!(http_server("s1", "http://example.com/mcp")
        .validate()
        .is_err());
    assert!(http_server("s1", "").validate().is_err());
    let missing_url = ServerConfig {
        url: None,
        ..http_server("s1", "https://example.com/mcp")
    };
    assert!(missing_url.validate().is_err());
    let stdio = ServerConfig {
        id: "local".into(),
        name: "Local".into(),
        transport: TransportKind::Stdio,
        url: None,
        command: Some("python3 /opt/mcp/server.py".into()),
        preset_id: Some("custom".into()),
    };
    assert!(stdio.validate().is_ok());
    let bad_name = ServerConfig {
        name: "".into(),
        ..stdio.clone()
    };
    assert!(bad_name.validate().is_err());
    let bad_id = ServerConfig {
        id: "bad id".into(),
        ..stdio
    };
    assert!(bad_id.validate().is_err());
}

#[test]
fn presets_cover_both_higgsfield_paths_hyperframes_and_custom() {
    let presets = builtin_presets();
    assert_eq!(presets.len(), 4);
    let ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["higgsfield-rest", "higgsfield-mcp", "hyperframes", "custom"]
    );
    let rest = preset_by_id("higgsfield-rest").unwrap();
    assert_eq!(rest.kind, PresetKind::RestHiggsfield);
    assert_eq!(rest.auth, AuthKind::KeySecret);
    assert!(rest.available);
    let mcp = preset_by_id("higgsfield-mcp").unwrap();
    assert_eq!(mcp.kind, PresetKind::McpHttp);
    assert_eq!(mcp.auth, AuthKind::Oauth);
    assert!(!mcp.available);
    assert!(mcp.notes.join(" ").contains("untested end to end"));
    let hyperframes = preset_by_id("hyperframes").unwrap();
    assert_eq!(hyperframes.kind, PresetKind::McpHttp);
    assert_eq!(hyperframes.auth, AuthKind::ApiKey);
    assert!(hyperframes.available);
    assert_eq!(
        hyperframes.tools,
        vec![
            "list_models",
            "get_credits",
            "generate_video",
            "generate_image",
            "get_generation"
        ]
    );
    assert!(hyperframes
        .notes
        .iter()
        .any(|n| n.contains("4–15 seconds") && n.contains("480p")));
    let custom = preset_by_id("custom").unwrap();
    assert_eq!(custom.kind, PresetKind::Custom);
    assert!(custom.available);
    assert!(custom.url.is_none() && custom.command.is_none());
}

#[test]
fn higgsfield_catalog_is_a_verified_subset_with_a_note() {
    let catalog = higgsfield_models();
    assert_eq!(catalog.models.len(), 4);
    let seedance = catalog
        .models
        .iter()
        .find(|m| m.id == "bytedance/seedance-2.5/image-to-video")
        .unwrap();
    assert_eq!(seedance.kind, "image-to-video");
    assert!(catalog.models.iter().any(|m| m.kind == "text-to-image"));
    assert!(catalog.models.iter().any(|m| m.kind == "video-restyle"));
    assert!(catalog
        .note
        .as_deref()
        .unwrap_or("")
        .contains("Verified subset"));
}

#[test]
fn higgsfield_key_and_endpoint_rules() {
    assert!(parse_higgsfield_key("key-id-1:secret-2").is_ok());
    assert!(parse_higgsfield_key("only-id").is_err());
    assert!(parse_higgsfield_key("id:secret:extra").is_err());
    assert!(parse_higgsfield_key("id: secret").is_err());
    assert_eq!(
        validate_higgsfield_endpoint("bytedance/seedance-2.5/image-to-video").unwrap(),
        "bytedance/seedance-2.5/image-to-video"
    );
    assert_eq!(
        validate_higgsfield_endpoint("/flux-pro/kontext/max/text-to-image").unwrap(),
        "flux-pro/kontext/max/text-to-image"
    );
    assert!(validate_higgsfield_endpoint("").is_err());
    assert!(validate_higgsfield_endpoint("a/b/../../c").is_err());
    assert!(validate_higgsfield_endpoint("a\\b").is_err());
    assert!(validate_higgsfield_endpoint("a/b?x=1").is_err());
    assert!(validate_higgsfield_endpoint("a:b").is_err());
}

#[test]
fn job_poll_state_machine_covers_both_providers() {
    // Higgsfield statuses.
    assert_eq!(job_state_from_status("queued"), JobState::Pending);
    assert_eq!(job_state_from_status("in_progress"), JobState::Pending);
    assert_eq!(job_state_from_status("nsfw"), JobState::Failed);
    assert_eq!(job_state_from_status("failed"), JobState::Failed);
    assert_eq!(job_state_from_status("completed"), JobState::Succeeded);
    // HyperFrames-style statuses.
    assert_eq!(job_state_from_status("processing"), JobState::Pending);
    assert_eq!(job_state_from_status("succeeded"), JobState::Succeeded);
    assert_eq!(job_state_from_status("cancelled"), JobState::Failed);
    // Unknown statuses stay pending; a media URL completes the job.
    assert_eq!(job_state_from_status("mysterious"), JobState::Pending);
    assert_eq!(
        job_state_from_status_with_media("mysterious", true),
        JobState::Succeeded
    );
    assert_eq!(
        job_state_from_status_with_media("failed", true),
        JobState::Failed
    );
    let mut poll = JobPoll::new(Duration::from_secs(600));
    assert_eq!(poll.advance("queued", false), JobState::Pending);
    assert_eq!(poll.advance("in_progress", false), JobState::Pending);
    assert_eq!(poll.attempts, 2);
    assert!(!poll.is_expired());
    assert_eq!(poll.advance("completed", false), JobState::Succeeded);
    assert_eq!(JobPoll::next_delay(), Duration::from_secs(5));
    let mut instant = JobPoll::new(Duration::from_nanos(1));
    std::thread::sleep(Duration::from_millis(2));
    assert_eq!(instant.advance("queued", false), JobState::Failed);
}

#[test]
fn media_url_extraction_from_text_and_structured_results() {
    let mut found = Vec::new();
    scan_text_for_media_urls(
        "Video ready: https://cdn.example.com/a.mp4 (and a still https://cdn.example.com/b.jpg). Not media: https://example.com/page.html",
        &mut found,
    );
    assert_eq!(found.len(), 2);
    assert!(found
        .iter()
        .all(|c| c.url.starts_with("https://cdn.example.com/")));
    assert!(found.iter().any(|c| c.kind == "video"));
    assert!(found.iter().any(|c| c.kind == "image"));
    let value: Value = json!({
        "jobs": [{"results": {"raw": {"url": "https://cdn.example.com/out.mp4"}}}],
        "video": "https://cdn.example.com/v.webm",
        "unrelated": "https://example.com/readme.txt",
        "nested": {"downloadUrl": "https://cdn.example.com/final.png"}
    });
    let mut collected = Vec::new();
    collect_media_urls(&value, &mut collected);
    assert_eq!(collected.len(), 3);
    assert!(collected.iter().any(|c| c.url.ends_with("out.mp4")));
    assert!(collected.iter().any(|c| c.url.ends_with("v.webm")));
    assert!(collected.iter().any(|c| c.url.ends_with("final.png")));
}

#[test]
fn download_file_names_allow_media_only() {
    assert_eq!(validate_download_file_name("clip.mp4").unwrap(), "video");
    assert_eq!(validate_download_file_name("still.jpeg").unwrap(), "image");
    assert_eq!(validate_download_file_name("loop.WEBM").unwrap(), "video");
    assert!(validate_download_file_name("").is_err());
    assert!(validate_download_file_name("clip.mp4 ").is_err());
    assert!(validate_download_file_name("clip.exe").is_err());
    assert!(validate_download_file_name("clip.mp4/extra").is_err());
    assert!(validate_download_file_name(&"a".repeat(121)).is_err());
}

#[test]
fn session_credentials_are_scoped_per_server() {
    let store = McpCredentials::default();
    store
        .set("hyperframes", "hf-key-1".into(), "session")
        .unwrap();
    store
        .set("higgsfield-rest", "id:secret".into(), "session")
        .unwrap();
    assert_eq!(&**store.get("hyperframes").unwrap(), "hf-key-1");
    assert_eq!(&**store.get("higgsfield-rest").unwrap(), "id:secret");
    assert!(store.get("unknown-server").is_err());
    store.delete("hyperframes").unwrap();
    assert!(store.get("hyperframes").is_err());
    assert!(store.get("higgsfield-rest").is_ok());
    // Deleting a server with no stored credential is not an error when
    // the OS store has no entry either; the session entry is gone.
    assert!(store.delete("never-saved").is_ok());
}

#[test]
fn tool_schema_drives_form_fields_and_validation() {
    // The shape of a tools/list inputSchema, with the parameter names
    // documented on hyperframes.net/hyperframes-mcp (6 November 2026).
    // The names come from that page; the test proves the conversion
    // and validation, not a live server response, which has not been
    // fetched in this environment.
    let schema = json!({
        "type": "object",
        "properties": {
            "prompt": {"type": "string", "description": "Text prompt", "maxLength": 4000},
            "image_url": {"type": "string"},
            "video_urls": {"type": "array", "items": {"type": "string"}},
            "scene": {"type": "string"},
            "duration_seconds": {"type": "integer", "minimum": 4, "maximum": 15, "default": 15},
            "resolution": {"type": "string", "enum": ["480p", "720p"], "default": "720p"},
            "fast": {"type": "boolean", "default": true},
            "aspect_ratio": {"type": "string"},
            "generate_audio": {"type": "boolean"},
            "web_search": {"type": "boolean"},
            "seed": {"type": "integer"}
        },
        "required": ["prompt"]
    });
    let fields = form_fields_from_schema(&schema);
    assert_eq!(fields.len(), 11);
    let prompt = fields.iter().find(|f| f.name == "prompt").unwrap();
    assert_eq!(prompt.field_type, "string");
    assert!(prompt.required);
    assert_eq!(prompt.max_length, Some(4000));
    let duration = fields
        .iter()
        .find(|f| f.name == "duration_seconds")
        .unwrap();
    assert_eq!(duration.field_type, "integer");
    assert_eq!(duration.minimum, Some(4.0));
    assert_eq!(duration.maximum, Some(15.0));
    assert_eq!(duration.default, Some(json!(15)));
    let resolution = fields.iter().find(|f| f.name == "resolution").unwrap();
    assert_eq!(resolution.enum_values, vec![json!("480p"), json!("720p")]);
    let clips = fields.iter().find(|f| f.name == "video_urls").unwrap();
    assert_eq!(clips.field_type, "array");
    assert_eq!(clips.items_type.as_deref(), Some("string"));
    // Schema validation accepts a complete call and rejects the
    // usual mistakes.
    let valid = json!({
        "prompt": "A calm studio shot",
        "duration_seconds": 4,
        "resolution": "720p",
        "fast": true,
        "video_urls": ["https://example.com/ref.mp4"]
    });
    assert!(validate_arguments_against_schema(&schema, &valid).is_ok());
    assert!(validate_arguments_against_schema(&schema, &json!({})).is_err());
    assert!(validate_arguments_against_schema(
        &schema,
        &json!({"prompt": "x", "duration_seconds": 16})
    )
    .is_err());
    assert!(validate_arguments_against_schema(
        &schema,
        &json!({"prompt": "x", "resolution": "1080p"})
    )
    .is_err());
    // The documented HyperFrames limits agree with the schema.
    assert!(validate_hyperframes_arguments("generate_video", &valid).is_ok());
    assert!(
        validate_hyperframes_arguments("generate_video", &json!({"prompt": "x".repeat(4001)}))
            .is_err()
    );
    assert!(validate_hyperframes_arguments(
        "generate_video",
        &json!({"prompt": "x", "duration_seconds": 3})
    )
    .is_err());
    assert!(validate_hyperframes_arguments(
        "generate_image",
        &json!({"prompt": "A still", "resolution": "2K"})
    )
    .is_ok());
    assert!(validate_hyperframes_arguments(
        "generate_image",
        &json!({"prompt": "A still", "resolution": "720p"})
    )
    .is_err());
}

#[test]
fn oauth_url_pkce_and_token_bodies_are_well_formed() {
    // RFC 7636 appendix B test vector.
    assert_eq!(
        pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let pair = generate_pkce_pair();
    assert_eq!(pair.verifier.len(), 64);
    assert_eq!(pair.challenge, pkce_challenge(&pair.verifier));
    let url = build_oauth_authorization_url(
        "https://mcp.higgsfield.ai/oauth2/authorize",
        "client-123",
        "http://127.0.0.1:49152/callback",
        &pair.challenge,
        "state-xyz",
    )
    .unwrap();
    assert!(url.starts_with("https://mcp.higgsfield.ai/oauth2/authorize?"));
    assert!(url.contains("response_type=code"));
    assert!(url.contains("client_id=client-123"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("scope=openid+email+offline_access"));
    assert!(url.contains("resource=https%3A%2F%2Fmcp.higgsfield.ai%2Fmcp"));
    assert!(url.contains(&format!("code_challenge={}", pair.challenge)));
    assert!(build_oauth_authorization_url(
        "https://mcp.higgsfield.ai/oauth2/authorize",
        "client-123",
        "http://example.com/callback",
        &pair.challenge,
        "state-xyz",
    )
    .is_err());
    assert_eq!(percent_encode_form_value("a b&c=d"), "a+b%26c%3D");
    let exchange = build_token_exchange_body(
        "client-1",
        "SplxlOBeZQQ",
        "http://127.0.0.1:49152/callback",
        &"v".repeat(43),
        None,
    );
    assert!(exchange.contains("grant_type=authorization_code"));
    assert!(exchange.contains("code=SplxlOBeZQQ"));
    assert!(!exchange.contains("client_secret"));
    let refresh = build_refresh_token_body("client-1", "refresh-token-1", Some("s3cret"));
    assert!(refresh.contains("grant_type=refresh_token"));
    assert!(refresh.contains("client_secret=s3cret"));
    assert!(validate_oauth_redirect_uri("http://127.0.0.1:49152/callback").is_ok());
    assert!(validate_oauth_redirect_uri("https://example.com/oauth/callback").is_ok());
    assert!(validate_oauth_redirect_uri("http://example.com/callback").is_err());
    assert!(validate_oauth_redirect_uri("myapp:/callback").is_err());
}
