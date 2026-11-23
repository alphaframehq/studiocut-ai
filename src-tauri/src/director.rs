use crate::{
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
    let schema = schemars::schema_for!(Edl);
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
        let prompt =
            json!({"instruction":instruction,"untrustedProjectContext":context.data}).to_string();
        let body = request_body(&config, &prompt);
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
        let edl = edl::parse(&text)?;
        edl::compile(project, &edl)?;
        Ok(Proposal { edl, trace: Trace { request_id: uuid::Uuid::new_v4().to_string(), provider:config.provider, model:config.model, latency_ms:started.elapsed().as_millis(), context_bytes:context.bytes, usage:value.get("usage").or_else(|| value.get("usageMetadata")).cloned().unwrap_or_else(|| json!({"inputTokens":value.get("prompt_eval_count"),"outputTokens":value.get("eval_count")})), prompt_version:1, estimated_cost:None, request_body:body, tool_calls:vec![] } })
    }
}
