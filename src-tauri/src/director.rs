use crate::{
    agents::{EdlDiff, GradeSuggestion, QaDrop},
    credentials::{Credentials, Provider},
    edl::{self, Edl},
    Project,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::Read,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const PROMPT: &str = include_str!("director-v1.txt");
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub provider: Provider,
    pub model: String,
    pub endpoint: String,
    pub max_tokens: u32,
}
impl Config {
    pub fn url(&self) -> Result<reqwest::Url, String> {
        if self.model.is_empty()
            || self.model.len() > 200
            || !(256..=8192).contains(&self.max_tokens)
        {
            return Err("Model and 256–8192 output tokens required".into());
        }
        let base = match self.provider {
            Provider::Openai => "https://api.openai.com/v1/chat/completions".to_owned(),
            Provider::Claude => "https://api.anthropic.com/v1/messages".into(),
            Provider::Gemini => format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
                self.model
            ),
            Provider::Groq => "https://api.groq.com/openai/v1/chat/completions".into(),
            Provider::Openrouter => "https://openrouter.ai/api/v1/chat/completions".into(),
            _ => self.endpoint.clone(),
        };
        let url = reqwest::Url::parse(&base).map_err(|_| "Invalid provider endpoint")?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Endpoint must not contain credentials, query or fragment".into());
        }
        let host = url.host_str().ok_or("Endpoint needs a host")?;
        match self.provider {
            Provider::Ollama
                if url.scheme() == "http"
                    && ["localhost", "127.0.0.1", "[::1]"].contains(&host)
                    && url.path() == "/api/chat" =>
            {
                ()
            }
            Provider::Ollama => {
                return Err("Ollama must use a loopback http:// endpoint ending /api/chat".into())
            }
            Provider::Bedrock
                if !host.starts_with("bedrock-runtime.")
                    || !host.ends_with(".amazonaws.com")
                    || url.path() != format!("/model/{}/converse", self.model) =>
            {
                return Err("Use the AWS Bedrock regional /model/<model>/converse endpoint".into())
            }
            Provider::Vertex
                if !(host == "aiplatform.googleapis.com"
                    || host.ends_with("-aiplatform.googleapis.com"))
                    || !url.path().ends_with(&format!(
                        "/publishers/google/models/{}:generateContent",
                        self.model
                    )) =>
            {
                return Err("Use the Vertex Google publisher generateContent endpoint".into())
            }
            _ => (),
        }
        if self.provider != Provider::Ollama && url.scheme() != "https" {
            return Err("Cloud endpoints require HTTPS".into());
        }
        Ok(url)
    }
    pub fn scope(&self) -> Result<String, String> {
        Ok(self.url()?.to_string())
    }
}
/// System prompt for `titles` mode: titles and hooks, not an EDL. Plain
/// text out, so no JSON schema is attached to the request.
pub const TITLES_SYSTEM: &str = "You are a short-form video title and hook writer. Return plain text only, no JSON, no markdown fences: use exactly these section headers, in this order: ## Titles — 5 numbered video titles (each under 60 characters); ## Hooks — 3 numbered opening hooks (each one spoken sentence, under 15 words); ## Description — one description (2 sentences, under 280 characters); ## Platform variants — one title each for YouTube long, Shorts, Reels and TikTok (each under 60 characters), one per line formatted `YouTube: ...`, `Shorts: ...`, `Reels: ...`, `TikTok: ...`. Base titles and hooks only on the supplied project context — asset names and transcript. Do not invent people, claims or footage that is not in the context. No preamble, no explanation.";

/// The Director's plan for an edit proposal: one short string per aspect.
/// The model returns it alongside the EDL; all fields default to empty
/// when the model omits them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    #[serde(default)]
    pub pace: String,
    #[serde(default)]
    pub cuts: String,
    #[serde(default)]
    pub captions: String,
    #[serde(default)]
    pub grade: String,
    #[serde(default)]
    pub music: String,
}

/// What the model returns for an edit proposal: a plan plus the EDL.
/// Ollama's format constraint may still force a bare EDL, so
/// [`parse_proposal_response`] falls back to that shape.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProposalResponse {
    #[serde(default)]
    pub plan: Option<Plan>,
    pub edl: Edl,
}

/// A previous proposal a follow-up revises: its id, its EDL and the
/// summary the user saw. The summary travels in the prompt so the model
/// can refer to what was proposed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviousProposal {
    pub id: String,
    pub edl: Edl,
    pub summary: String,
}

/// Parse a model reply into an EDL and an optional plan: try the
/// `{"plan", "edl"}` wrapper first, then fall back to a bare EDL.
pub fn parse_proposal_response(text: &str) -> Result<(Edl, Option<Plan>), String> {
    if text.len() > 262144 {
        return Err("EDL exceeds 256 KB".into());
    }
    if let Ok(wrapped) = serde_json::from_str::<ProposalResponse>(text) {
        return Ok((wrapped.edl, wrapped.plan));
    }
    let edl = edl::parse(text)?;
    Ok((edl, None))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectorListedModel {
    pub id: String,
    pub recommended: bool,
    /// Why this entry is here when it did not come from a live list
    /// (static Claude/Bedrock lists). `None` for fetched models.
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectorModelsResponse {
    pub models: Vec<DirectorListedModel>,
    /// Explains an empty list (Vertex) or a static list; `None` otherwise.
    pub note: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectorModelsRequest {
    pub provider: String,
    pub api_key: String,
    pub endpoint: String,
}

const MAX_DIRECTOR_MODELS_JSON: u64 = 2_000_000;

/// Well-known current chat models. Only flagged when actually present in
/// the fetched list — never injected into it.
const RECOMMENDED_DIRECTOR_MODELS: [&str; 7] = [
    "gpt-4o",
    "gpt-4o-mini",
    "claude-sonnet-4-5",
    "gemini-2.5-flash",
    "llama-3.3-70b-versatile",
    "openai/gpt-4o",
    "qwen/qwen-2.5-72b-instruct",
];

/// Substrings that mark a model as not-a-chat-model for the Director.
const NON_CHAT_MARKERS: [&str; 10] = [
    "embedding",
    "whisper",
    "tts",
    "dall-e",
    "moderation",
    "audio",
    "image",
    "video",
    "rerank",
    "guard",
];

/// Ids that look like chat models on providers that do not report a
/// modality (OpenRouter without `architecture.modality`).
const CHAT_FAMILIES: [&str; 12] = [
    "gpt-", "claude", "llama", "qwen", "mistral", "gemini", "deepseek", "grok", "phi-", "mixtral",
    "command", "glm-",
];

const CLAUDE_STATIC_NOTE: &str = "Anthropic publishes no list API — known models shown";
const BEDROCK_STATIC_NOTE: &str = "Bedrock needs AWS signing for a live list — known models shown";
const VERTEX_NOTE: &str =
    "Vertex model list needs a project-scoped API call — paste the full model path";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ModelsAuth {
    Bearer,
    GoogApiKey,
    None,
}

/// Derive the `/models` (or Ollama `/api/tags`) URL for a provider, plus
/// how to authenticate. Static-list providers (claude/bedrock) and Vertex
/// never reach this function — see [`list_director_models`].
pub fn director_models_endpoint(
    provider: &str,
    endpoint: &str,
) -> Result<(reqwest::Url, ModelsAuth), String> {
    let (raw, auth) = match provider {
        "openai" => (
            "https://api.openai.com/v1/models".to_string(),
            ModelsAuth::Bearer,
        ),
        "groq" => (
            "https://api.groq.com/openai/v1/models".to_string(),
            ModelsAuth::Bearer,
        ),
        "openrouter" => (
            "https://openrouter.ai/api/v1/models".to_string(),
            ModelsAuth::Bearer,
        ),
        "gemini" => (
            "https://generativelanguage.googleapis.com/v1beta/models".to_string(),
            ModelsAuth::GoogApiKey,
        ),
        "compatible" => {
            let trimmed = endpoint.trim_end_matches('/');
            let base = if let Some(stripped) = trimmed.strip_suffix("/chat/completions") {
                stripped.to_string()
            } else if trimmed.ends_with("/models") {
                trimmed.to_string()
            } else {
                return Err("Enter the full chat endpoint URL ending in /chat/completions".into());
            };
            (format!("{base}/models"), ModelsAuth::Bearer)
        }
        "ollama" => {
            let trimmed = endpoint.trim_end_matches('/');
            let base = if trimmed.is_empty() {
                "http://127.0.0.1:11434".to_string()
            } else {
                trimmed
                    .strip_suffix("/api/chat")
                    .unwrap_or(trimmed)
                    .to_string()
            };
            (format!("{base}/api/tags"), ModelsAuth::None)
        }
        _ => return Err("Unknown Director provider".into()),
    };
    let url = reqwest::Url::parse(&raw).map_err(|_| "Enter a complete models endpoint URL")?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Models endpoint must not contain credentials, query, or fragment".into());
    }
    match provider {
        "ollama" => {
            let host = url.host_str().unwrap_or("");
            if url.scheme() != "http" || !["localhost", "127.0.0.1", "[::1]"].contains(&host) {
                return Err("Ollama models must come from a loopback http:// endpoint".into());
            }
        }
        _ => {
            if url.scheme() != "https" {
                return Err("Cloud model lists require HTTPS".into());
            }
        }
    }
    Ok((url, auth))
}

/// Keep only chat/text models from an OpenAI-style `{data:[{id, ...}]}`
/// list. Drops embedding/audio/image/video/TTS and similar models.
/// OpenRouter items may carry `architecture.modality` ("text->text") or
/// `input_modalities`; without either, a known chat family id is required.
pub fn filter_director_models(value: &Value, provider: &str) -> Vec<DirectorListedModel> {
    let mut models: Vec<DirectorListedModel> = value["data"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item["id"].as_str()?.to_string();
                    let lower = id.to_lowercase();
                    if NON_CHAT_MARKERS.iter().any(|m| lower.contains(m)) {
                        return None;
                    }
                    if provider == "openrouter" {
                        let modality = item
                            .pointer("/architecture/modality")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let input_modalities = item
                            .pointer("/architecture/input_modalities")
                            .and_then(Value::as_array)
                            .map(|m| m.iter().any(|v| v.as_str() == Some("text")))
                            .unwrap_or(false);
                        let text_modality = modality.starts_with("text")
                            || modality.contains("text->")
                            || input_modalities;
                        let chat_family = CHAT_FAMILIES.iter().any(|f| lower.contains(f));
                        if !text_modality && !chat_family {
                            return None;
                        }
                    }
                    Some(DirectorListedModel {
                        recommended: RECOMMENDED_DIRECTOR_MODELS.contains(&id.as_str()),
                        id,
                        note: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.truncate(100);
    models
}

/// Gemini `{models:[{name:"models/...", supportedGenerationMethods}]}`:
/// keep only entries that support generateContent, strip the `models/`
/// prefix from the id.
pub fn filter_gemini_models(value: &Value) -> Vec<DirectorListedModel> {
    let mut models: Vec<DirectorListedModel> = value["models"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let name = item["name"].as_str()?;
                    let supports = item["supportedGenerationMethods"]
                        .as_array()
                        .is_some_and(|m| m.iter().any(|v| v.as_str() == Some("generateContent")));
                    if !supports {
                        return None;
                    }
                    let id = name.strip_prefix("models/").unwrap_or(name).to_string();
                    let lower = id.to_lowercase();
                    if NON_CHAT_MARKERS.iter().any(|m| lower.contains(m)) {
                        return None;
                    }
                    Some(DirectorListedModel {
                        recommended: RECOMMENDED_DIRECTOR_MODELS.contains(&id.as_str()),
                        id,
                        note: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.truncate(100);
    models
}

/// Ollama `{models:[{name}]}`: local model names, kept as-is (minus the
/// non-chat markers); no auth, loopback only.
pub fn filter_ollama_models(value: &Value) -> Vec<DirectorListedModel> {
    let mut models: Vec<DirectorListedModel> = value["models"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = item["name"].as_str()?.to_string();
                    let lower = id.to_lowercase();
                    if NON_CHAT_MARKERS.iter().any(|m| lower.contains(m)) {
                        return None;
                    }
                    Some(DirectorListedModel {
                        recommended: false,
                        id,
                        note: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.truncate(100);
    models
}

fn static_models(ids: &[&str], note: &str) -> Vec<DirectorListedModel> {
    ids.iter()
        .map(|id| DirectorListedModel {
            id: id.to_string(),
            recommended: RECOMMENDED_DIRECTOR_MODELS.contains(id),
            note: Some(note.to_string()),
        })
        .collect()
}

/// Fetch (or, for providers without a list API, return a labelled static
/// list of) Director chat models. The API key is used for this one call
/// only and is never stored.
pub fn list_director_models(
    request: &DirectorModelsRequest,
) -> Result<DirectorModelsResponse, String> {
    match request.provider.as_str() {
        "claude" => {
            return Ok(DirectorModelsResponse {
                models: static_models(
                    &[
                        "claude-opus-4-1",
                        "claude-sonnet-4-5",
                        "claude-3-5-sonnet-latest",
                        "claude-3-5-haiku-latest",
                        "claude-3-haiku-20240307",
                    ],
                    CLAUDE_STATIC_NOTE,
                ),
                note: Some(CLAUDE_STATIC_NOTE.into()),
            });
        }
        "bedrock" => {
            return Ok(DirectorModelsResponse {
                models: static_models(
                    &[
                        "anthropic.claude-sonnet-4-5-20250929-v1:0",
                        "anthropic.claude-3-5-sonnet-20241022-v2:0",
                        "amazon.titan-text-premier-v1:0",
                        "meta.llama3-70b-instruct-v1:0",
                        "mistral.mistral-large-2402-v1:0",
                    ],
                    BEDROCK_STATIC_NOTE,
                ),
                note: Some(BEDROCK_STATIC_NOTE.into()),
            });
        }
        "vertex" => {
            return Ok(DirectorModelsResponse {
                models: vec![],
                note: Some(VERTEX_NOTE.into()),
            });
        }
        _ => {}
    }
    if request.provider != "ollama"
        && (request.api_key.trim().is_empty() || request.api_key.len() > 4096)
    {
        return Err("Enter an API key first".into());
    }
    let (url, auth) = director_models_endpoint(&request.provider, &request.endpoint)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Cannot initialize secure Director client")?;
    let mut request_builder = client.get(url);
    request_builder = match auth {
        ModelsAuth::Bearer => request_builder.bearer_auth(request.api_key.trim()),
        ModelsAuth::GoogApiKey => request_builder.header("x-goog-api-key", request.api_key.trim()),
        ModelsAuth::None => request_builder,
    };
    let response = request_builder.send().map_err(|_| {
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
        .take(MAX_DIRECTOR_MODELS_JSON + 1)
        .read_to_end(&mut data)
        .map_err(|_| "Failed reading model list response")?;
    if data.len() as u64 > MAX_DIRECTOR_MODELS_JSON {
        return Err("Model list exceeds the 2 MB response limit".into());
    }
    let value: Value =
        serde_json::from_slice(&data).map_err(|_| "Model list returned invalid JSON")?;
    let models = match request.provider.as_str() {
        "gemini" => filter_gemini_models(&value),
        "ollama" => filter_ollama_models(&value),
        _ => filter_director_models(&value, &request.provider),
    };
    Ok(DirectorModelsResponse { models, note: None })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    pub data: Value,
    pub bytes: usize,
    pub omitted_assets: usize,
    pub omitted_clips: usize,
}
pub fn context(project: &Project) -> Context {
    let assets: Vec<_> = project.assets.iter().take(100).map(|a| json!({"id":a.id,"name":a.name.chars().take(120).collect::<String>(),"kind":a.kind,"frames":a.duration.map(|d| (d * project.settings.fps as f64).floor() as u32),"audio":a.audio_stream.is_some(),"video":a.video_stream.is_some()})).collect();
    let mut remaining = 200usize;
    let mut text_remaining = 8000usize;
    let tracks: Vec<_> = project.tracks.iter().map(|t| {
        let clips: Vec<_> = t.clips.iter().take(remaining).map(|c| {
            let mut transcript = Vec::new();
            if let Some(caption) = &c.caption { for word in &caption.words { let text: String = word.text.chars().take(text_remaining).collect(); text_remaining -= text.chars().count(); if !text.is_empty() { transcript.push(json!({"text":text,"start":word.start,"end":word.end})); } } }
            json!({"id":c.id,"assetId":c.asset_id,"linkId":c.link_id,"start":c.start,"sourceIn":c.source_in,"duration":c.duration,"transcript":transcript})
        }).collect();
        remaining -= clips.len();
        json!({"id":t.id,"kind":t.kind,"locked":t.locked,"clips":clips})
    }).collect();
    let data = json!({"projectId":project.id,"revision":project.revision,"fps":project.settings.fps,"assets":assets,"tracks":tracks});
    Context {
        bytes: data.to_string().len(),
        data,
        omitted_assets: project.assets.len().saturating_sub(100),
        omitted_clips: project
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum::<usize>()
            .saturating_sub(200),
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub edl: Edl,
    pub trace: Trace,
    /// Plain-text answer for `titles` mode; `None` for edit proposals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The model's plan (pace, cuts, captions, grade, music), if supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Plan>,
    /// Operations QA removed before compile, with reasons. Empty when
    /// nothing was dropped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub qa_dropped: Vec<QaDrop>,
    /// Index-based diff against the previous proposal's EDL, only when
    /// this proposal revises one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<EdlDiff>,
    /// Grade suggestion derived from the plan's grade text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade_suggestion: Option<GradeSuggestion>,
    /// Caption preset picked from the plan's captions text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption_preset: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Trace {
    pub request_id: String,
    pub provider: Provider,
    pub model: String,
    pub latency_ms: u128,
    pub context_bytes: usize,
    pub usage: Value,
    pub prompt_version: u32,
    pub estimated_cost: Option<f64>,
    pub request_body: Value,
    pub tool_calls: Vec<Value>,
}
#[derive(Default)]
pub struct Director(pub AtomicBool);
struct Guard<'a>(&'a AtomicBool);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub fn request_body(config: &Config, prompt: &str) -> Value {
    let schema = schemars::schema_for!(ProposalResponse);
    let system = format!(
        "{PROMPT}\nEDL JSON schema: {}",
        serde_json::to_string(&schema).unwrap()
    );
    match config.provider {
        Provider::Claude => {
            json!({"model":config.model,"max_tokens":config.max_tokens,"system":system,"messages":[{"role":"user","content":prompt}]})
        }
        Provider::Gemini | Provider::Vertex => {
            json!({"systemInstruction":{"parts":[{"text":system}]},"contents":[{"role":"user","parts":[{"text":prompt}]}],"generationConfig":{"maxOutputTokens":config.max_tokens,"responseMimeType":"application/json"}})
        }
        Provider::Bedrock => {
            json!({"system":[{"text":system}],"messages":[{"role":"user","content":[{"text":prompt}]}],"inferenceConfig":{"maxTokens":config.max_tokens}})
        }
        Provider::Ollama => {
            json!({"model":config.model,"stream":false,"format":schema,"messages":[{"role":"system","content":system},{"role":"user","content":prompt}],"options":{"num_predict":config.max_tokens,"num_ctx":32768}})
        }
        _ => {
            json!({"model":config.model,"max_tokens":config.max_tokens,"response_format":{"type":"json_object"},"messages":[{"role":"system","content":system},{"role":"user","content":prompt}]})
        }
    }
}
/// Titles mode: same provider body shapes as [`request_body`] but without
/// the EDL JSON schema — the model answers in plain text.
pub fn titles_body(config: &Config, prompt: &str) -> Value {
    match config.provider {
        Provider::Claude => {
            json!({"model":config.model,"max_tokens":config.max_tokens,"system":TITLES_SYSTEM,"messages":[{"role":"user","content":prompt}]})
        }
        Provider::Gemini | Provider::Vertex => {
            json!({"systemInstruction":{"parts":[{"text":TITLES_SYSTEM}]},"contents":[{"role":"user","parts":[{"text":prompt}]}],"generationConfig":{"maxOutputTokens":config.max_tokens}})
        }
        Provider::Bedrock => {
            json!({"system":[{"text":TITLES_SYSTEM}],"messages":[{"role":"user","content":[{"text":prompt}]}],"inferenceConfig":{"maxTokens":config.max_tokens}})
        }
        Provider::Ollama => {
            json!({"model":config.model,"stream":false,"messages":[{"role":"system","content":TITLES_SYSTEM},{"role":"user","content":prompt}],"options":{"num_predict":config.max_tokens,"num_ctx":32768}})
        }
        _ => {
            json!({"model":config.model,"max_tokens":config.max_tokens,"messages":[{"role":"system","content":TITLES_SYSTEM},{"role":"user","content":prompt}]})
        }
    }
}
pub fn extract(provider: Provider, value: &Value) -> Result<String, String> {
    let text = match provider {
        Provider::Claude => value.pointer("/content/0/text"),
        Provider::Gemini | Provider::Vertex => value.pointer("/candidates/0/content/parts/0/text"),
        Provider::Bedrock => value.pointer("/output/message/content/0/text"),
        Provider::Ollama => value.pointer("/message/content"),
        _ => value.pointer("/choices/0/message/content"),
    };
    text.and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or("Provider returned no text proposal".into())
}
impl Director {
    pub fn propose(
        &self,
        credentials: &Credentials,
        project: &Project,
        config: Config,
        instruction: String,
        consent: bool,
        mode: &str,
        previous: Option<PreviousProposal>,
        learned_preferences: Option<String>,
    ) -> Result<Proposal, String> {
        if self.0.swap(true, Ordering::AcqRel) {
            return Err("A Director request is already running".into());
        }
        let _guard = Guard(&self.0);
        let started = Instant::now();
        let url = config.url()?;
        if config.provider != Provider::Ollama && !consent {
            return Err(
                "Approve sending bounded catalog/transcript context to this provider".into(),
            );
        }
        if instruction.trim().is_empty() || instruction.len() > 4000 {
            return Err("Instruction must be 1–4000 bytes".into());
        }
        let key = credentials.get(config.provider, &config.scope()?)?;
        let context = context(project);
        if context.bytes > 96000 {
            return Err("Context exceeds 96 KB; use a smaller project".into());
        }
        let mut prompt_value =
            json!({"instruction":instruction,"untrustedProjectContext":context.data});
        if let Some(previous) = &previous {
            prompt_value["previousProposal"] =
                json!({"id":previous.id,"edl":previous.edl,"summary":previous.summary});
        }
        if let Some(preferences) = &learned_preferences {
            prompt_value["learnedPreferences"] = json!(preferences);
        }
        let prompt = prompt_value.to_string();
        let titles = mode == "titles";
        let body = if titles {
            titles_body(&config, &prompt)
        } else {
            request_body(&config, &prompt)
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "HTTP client unavailable")?;
        let mut response = None;
        for attempt in 0..2 {
            let mut request = client.post(url.clone()).json(&body);
            request = match config.provider {
                Provider::Claude => request
                    .header("x-api-key", key.as_str())
                    .header("anthropic-version", "2023-06-01"),
                Provider::Gemini => request.header("x-goog-api-key", key.as_str()),
                Provider::Ollama => request,
                _ => request.bearer_auth(key.as_str()),
            };
            let result = request
                .send()
                .map_err(|_| "Provider connection failed or timed out; no edit applied")?;
            if attempt == 0 && result.status().as_u16() == 429 {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
            if !result.status().is_success() {
                return Err(format!("Provider HTTP {}. Check credentials, model, quota and endpoint; no edit applied.", result.status().as_u16()));
            }
            response = Some(result);
            break;
        }
        let mut bytes = Vec::new();
        response
            .ok_or("Provider rate limited")?
            .take(262145)
            .read_to_end(&mut bytes)
            .map_err(|_| "Provider response interrupted")?;
        if bytes.len() > 262144 {
            return Err("Provider response exceeds 256 KB".into());
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid provider JSON")?;
        let text = extract(config.provider, &value)?;
        if titles {
            // Titles & hooks: plain text back, no EDL to apply.
            let edl = Edl {
                version: 1,
                project_id: project.id,
                base_revision: project.revision,
                summary: "Titles & hooks".into(),
                operations: vec![],
            };
            return Ok(Proposal { edl, text: Some(text), plan: None, qa_dropped: vec![], diff: None, grade_suggestion: None, caption_preset: None, trace: Trace { request_id: uuid::Uuid::new_v4().to_string(), provider:config.provider, model:config.model, latency_ms:started.elapsed().as_millis(), context_bytes:context.bytes, usage:value.get("usage").or_else(|| value.get("usageMetadata")).cloned().unwrap_or_else(|| json!({"inputTokens":value.get("prompt_eval_count"),"outputTokens":value.get("eval_count")})), prompt_version:3, estimated_cost:None, request_body:body, tool_calls:vec![] } });
        }
        let (mut edl, plan) = parse_proposal_response(&text)?;
        // QA runs after parse, before compile, on the parsed EDL.
        let (kept, qa_dropped) = crate::agents::qa_validate(project, &edl);
        edl.operations = kept;
        let diff = previous
            .as_ref()
            .map(|previous| crate::agents::diff_edls(&previous.edl, &edl));
        let caption_preset = plan
            .as_ref()
            .map(|plan| crate::agents::pick_caption_preset(&plan.captions).to_string());
        let grade_suggestion = plan
            .as_ref()
            .map(|plan| crate::agents::grade_suggestion(&plan.grade));
        if edl.operations.is_empty() {
            // QA dropped everything: still return the proposal with a
            // note, never an error.
            if !qa_dropped.is_empty() {
                edl.summary = format!(
                    "{} [QA removed {} operation(s) that would have failed validation]",
                    edl.summary,
                    qa_dropped.len()
                );
            }
        } else {
            edl::compile(project, &edl)?;
        }
        Ok(Proposal { edl, text: None, plan, qa_dropped, diff, grade_suggestion, caption_preset, trace: Trace { request_id: uuid::Uuid::new_v4().to_string(), provider:config.provider, model:config.model, latency_ms:started.elapsed().as_millis(), context_bytes:context.bytes, usage:value.get("usage").or_else(|| value.get("usageMetadata")).cloned().unwrap_or_else(|| json!({"inputTokens":value.get("prompt_eval_count"),"outputTokens":value.get("eval_count")})), prompt_version:3, estimated_cost:None, request_body:body, tool_calls:vec![] } })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn models_endpoint_derivation_and_https_rules() {
        let (url, auth) = director_models_endpoint("openai", "").unwrap();
        assert_eq!(url.as_str(), "https://api.openai.com/v1/models");
        assert_eq!(auth, ModelsAuth::Bearer);
        let (url, _) = director_models_endpoint("groq", "").unwrap();
        assert_eq!(url.as_str(), "https://api.groq.com/openai/v1/models");
        let (url, _) = director_models_endpoint("openrouter", "").unwrap();
        assert_eq!(url.as_str(), "https://openrouter.ai/api/v1/models");
        let (url, auth) = director_models_endpoint("gemini", "").unwrap();
        assert_eq!(
            url.as_str(),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
        assert_eq!(auth, ModelsAuth::GoogApiKey);
        let (url, auth) =
            director_models_endpoint("compatible", "https://host.example/v1/chat/completions")
                .unwrap();
        assert_eq!(url.as_str(), "https://host.example/v1/models");
        assert_eq!(auth, ModelsAuth::Bearer);
        assert!(director_models_endpoint("compatible", "https://host.example/v1/other").is_err());
        assert!(
            director_models_endpoint("compatible", "http://host.example/v1/chat/completions")
                .is_err()
        );
        let (url, auth) = director_models_endpoint("ollama", "").unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:11434/api/tags");
        assert_eq!(auth, ModelsAuth::None);
        let (url, _) =
            director_models_endpoint("ollama", "http://127.0.0.1:11434/api/chat").unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:11434/api/tags");
        assert!(director_models_endpoint("ollama", "http://192.168.1.2:11434/api/chat").is_err());
        assert!(director_models_endpoint("unknown", "").is_err());
    }
    #[test]
    fn filter_director_models_drops_non_chat_and_marks_recommended() {
        let value = json!({"data":[
            {"id":"gpt-4o"},
            {"id":"gpt-4o-mini"},
            {"id":"text-embedding-3-small"},
            {"id":"whisper-1"},
            {"id":"dall-e-3"},
            {"id":"gpt-4o-audio-preview"},
            {"id":"o1-preview"},
        ]});
        let models = filter_director_models(&value, "openai");
        let kept: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(kept, vec!["gpt-4o", "gpt-4o-mini", "o1-preview"]);
        assert!(
            models
                .iter()
                .find(|m| m.id == "gpt-4o")
                .unwrap()
                .recommended
        );
        assert!(
            !models
                .iter()
                .find(|m| m.id == "o1-preview")
                .unwrap()
                .recommended
        );
    }
    #[test]
    fn filter_director_models_openrouter_modality_and_families() {
        let value = json!({"data":[
            {"id":"openai/gpt-4o","architecture":{"modality":"text->text"}},
            {"id":"meta-llama/llama-3.3-70b-instruct","architecture":{"modality":"text->text"}},
            {"id":"openai/text-embedding-3-small","architecture":{"modality":"text->text"}},
            {"id":"some-vendor/whisper-large","architecture":{"modality":"audio->text"}},
            {"id":"qwen/qwen-2.5-72b-instruct"},
            {"id":"mystery/image-gen-1","architecture":{"modality":"text->image"}},
        ]});
        let models = filter_director_models(&value, "openrouter");
        let kept: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(kept.contains(&"openai/gpt-4o"));
        assert!(kept.contains(&"meta-llama/llama-3.3-70b-instruct"));
        assert!(kept.contains(&"qwen/qwen-2.5-72b-instruct"));
        assert!(!kept
            .iter()
            .any(|id| id.contains("embedding") || id.contains("whisper")));
    }
    #[test]
    fn filter_gemini_models_keeps_generate_content_only() {
        let value = json!({"models":[
            {"name":"models/gemini-2.5-flash","supportedGenerationMethods":["generateContent"]},
            {"name":"models/embedding-001","supportedGenerationMethods":["embedContent"]},
            {"name":"models/gemini-2.5-pro","supportedGenerationMethods":["generateContent","countTokens"]},
        ]});
        let models = filter_gemini_models(&value);
        let kept: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(kept, vec!["gemini-2.5-flash", "gemini-2.5-pro"]);
    }
    #[test]
    fn filter_ollama_models_keeps_names() {
        let value = json!({"models":[
            {"name":"llama3.3:70b"},
            {"name":"qwen2.5:72b"},
        ]});
        let models = filter_ollama_models(&value);
        let kept: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(kept, vec!["llama3.3:70b", "qwen2.5:72b"]);
    }
    #[test]
    fn static_lists_are_labelled_and_vertex_is_empty() {
        let claude = list_director_models(&DirectorModelsRequest {
            provider: "claude".into(),
            api_key: "".into(),
            endpoint: "".into(),
        })
        .unwrap();
        assert_eq!(claude.models.len(), 5);
        assert!(claude.models.iter().all(|m| m.note.is_some()));
        assert!(claude
            .models
            .iter()
            .any(|m| m.id == "claude-sonnet-4-5" && m.recommended));
        let bedrock = list_director_models(&DirectorModelsRequest {
            provider: "bedrock".into(),
            api_key: "".into(),
            endpoint: "".into(),
        })
        .unwrap();
        assert_eq!(bedrock.models.len(), 5);
        let vertex = list_director_models(&DirectorModelsRequest {
            provider: "vertex".into(),
            api_key: "".into(),
            endpoint: "".into(),
        })
        .unwrap();
        assert!(vertex.models.is_empty());
        assert!(vertex.note.unwrap().contains("Vertex"));
        let missing_key = list_director_models(&DirectorModelsRequest {
            provider: "openai".into(),
            api_key: "".into(),
            endpoint: "".into(),
        });
        assert!(missing_key.is_err());
    }
    #[test]
    fn parse_proposal_response_wrapper_and_bare_edl_fallback() {
        let project_id = uuid::Uuid::new_v4();
        let bare = format!(
            r#"{{"version":1,"projectId":"{project_id}","baseRevision":0,"summary":"Trim the silence","operations":[]}}"#
        );
        let (edl, plan) = parse_proposal_response(&bare).unwrap();
        assert_eq!(edl.summary, "Trim the silence");
        assert!(plan.is_none());
        let wrapped = format!(
            r#"{{"plan":{{"pace":"tight 2s shots","cuts":"remove dead air","captions":"bold hormozi","grade":"warm, more contrast","music":"duck under voice"}},"edl":{bare}}}"#
        );
        let (edl, plan) = parse_proposal_response(&wrapped).unwrap();
        assert_eq!(edl.summary, "Trim the silence");
        let plan = plan.unwrap();
        assert_eq!(plan.pace, "tight 2s shots");
        assert_eq!(plan.grade, "warm, more contrast");
        assert!(parse_proposal_response("not json at all").is_err());
        assert!(parse_proposal_response("").is_err());
    }
    #[test]
    fn titles_body_has_no_edl_schema() {
        let config = Config {
            provider: Provider::Openai,
            model: "gpt-4o".into(),
            endpoint: "".into(),
            max_tokens: 512,
        };
        let body = titles_body(&config, "prompt");
        assert!(!body.to_string().contains("baseRevision"));
        assert!(body.to_string().contains("title and hook writer"));
        let edit = request_body(&config, "prompt");
        assert!(edit.to_string().contains("baseRevision"));
    }
}
