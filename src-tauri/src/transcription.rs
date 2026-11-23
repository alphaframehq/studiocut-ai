use crate::{
    analysis::selected_audio,
    media,
    timeline::{Caption, CaptionWord, Clip, Track},
    Project,
};
use reqwest::blocking::{multipart, Client};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, io::Read, path::Path, time::Duration};
use uuid::Uuid;

const MAX_AUDIO: u64 = 20_000_000;
const MAX_JSON: u64 = 4_000_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptionOptions {
    pub provider: String,
    pub binary: String,
    pub model: String,
    pub api_key: String,
    pub endpoint: String,
    pub language: String,
    pub allow_upload: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimedText {
    pub text: String,
    pub start: f64,
    pub end: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transcript {
    pub timing: String,
    pub items: Vec<TimedText>,
}

pub fn extract_wav(asset: &media::Asset, start: f64, duration: f64) -> Result<Vec<u8>, String> {
    if !duration.is_finite()
        || duration <= 0.0
        || duration > 600.0
        || !start.is_finite()
        || start < 0.0
    {
        return Err(
            "Transcribe a selection of at most 10 minutes; split longer clips first".into(),
        );
    }
    media::verified_source(asset)?;
    let stream = asset.audio_stream.ok_or("Selected clip has no audio")?;
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let output_path = temp.path().join("selection.wav");
    media::run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-nostdin",
            "-ss",
            &start.to_string(),
            "-i",
            &asset.source_path,
            "-t",
            &duration.to_string(),
            "-map",
            &format!("0:{stream}"),
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "-f",
            "wav",
            output_path.to_str().ok_or("Invalid temporary audio path")?,
        ],
        Duration::from_secs(120),
        MAX_AUDIO,
    )?;
    let mut bytes = Vec::new();
    fs::File::open(&output_path)
        .map_err(|e| e.to_string())?
        .take(MAX_AUDIO + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_AUDIO {
        return Err("Extracted audio exceeds 20 MB".into());
    }
    if bytes.len() < 44 {
        return Err("No audio was decoded".into());
    }
    Ok(bytes)
}

pub fn local_available(binary: &str, model: &str) -> Result<(), String> {
    if model.is_empty() || !Path::new(model).is_file() {
        return Err(
            "Local transcription unavailable: choose an installed whisper.cpp model file".into(),
        );
    }
    if binary.is_empty() {
        return Err("Local transcription unavailable: set the whisper-cli executable path".into());
    }
    media::run(binary, &["--help"], Duration::from_secs(10), 1_048_576)
        .map_err(|_| "Local transcription unavailable: whisper-cli could not run; install whisper.cpp or correct its executable path".to_string())?;
    Ok(())
}

pub fn parse(value: &Value, local: bool, duration: f64) -> Result<Transcript, String> {
    let mut words = vec![];
    let mut segments = vec![];
    if local {
        let entries = value["transcription"]
            .as_array()
            .ok_or("whisper-cli JSON has no transcription array")?;
        for segment in entries {
            let text = segment["text"].as_str().unwrap_or("").trim();
            if text.is_empty() {
                continue;
            }
            let time = |v: &Value, field: &str| {
                v["offsets"][field]
                    .as_f64()
                    .map(|n| n / 1000.0)
                    .ok_or("Missing whisper-cli timestamp".to_string())
            };
            let item = TimedText {
                text: text.into(),
                start: time(segment, "from")?,
                end: time(segment, "to")?,
            };
            segments.push(item);
            if let Some(tokens) = segment["tokens"].as_array() {
                let mut group: Option<TimedText> = None;
                for token in tokens {
                    let text = token["text"].as_str().unwrap_or("");
                    if text.trim().is_empty() || text.trim().starts_with("[_") {
                        continue;
                    }
                    let (Ok(start), Ok(end)) = (time(token, "from"), time(token, "to")) else {
                        continue;
                    };
                    if start < 0.0 || end <= start {
                        continue;
                    }
                    if text.starts_with(char::is_whitespace) || group.is_none() {
                        if let Some(previous) = group.take() {
                            words.push(previous);
                        }
                        group = Some(TimedText {
                            text: text.trim().into(),
                            start,
                            end,
                        });
                    } else if let Some(previous) = &mut group {
                        previous.text.push_str(text);
                        previous.end = end;
                    }
                }
                if let Some(previous) = group {
                    words.push(previous);
                }
            }
        }
        let normalized = |items: &[TimedText]| {
            items
                .iter()
                .map(|i| i.text.split_whitespace().collect::<String>())
                .collect::<String>()
        };
        if normalized(&words) != normalized(&segments) {
            words.clear();
        }
    } else {
        for (key, target) in [("words", &mut words), ("segments", &mut segments)] {
            if let Some(items) = value[key].as_array() {
                for item in items {
                    target.push(TimedText {
                        text: item[if key == "words" { "word" } else { "text" }]
                            .as_str()
                            .ok_or("Missing transcript text")?
                            .trim()
                            .into(),
                        start: item["start"].as_f64().ok_or("Missing start timestamp")?,
                        end: item["end"].as_f64().ok_or("Missing end timestamp")?,
                    });
                }
            }
        }
    }
    let result = if !words.is_empty() {
        Transcript {
            timing: "word".into(),
            items: words,
        }
    } else {
        Transcript {
            timing: "segment".into(),
            items: segments,
        }
    };
    validate(&result, duration)?;
    Ok(result)
}

pub fn validate(transcript: &Transcript, duration: f64) -> Result<(), String> {
    if !["word", "segment"].contains(&transcript.timing.as_str())
        || transcript.items.is_empty()
        || transcript.items.len() > 12000
    {
        return Err("No usable timestamped speech returned. Choose a timestamp-capable model (e.g. whisper-1); text-only results cannot create captions".into());
    }
    let mut end = 0.0;
    for item in &transcript.items {
        if item.text.trim().is_empty()
            || item.text.encode_utf16().count() > 500
            || !item.start.is_finite()
            || !item.end.is_finite()
            || item.start < end
            || item.end <= item.start
            || item.end > duration + 0.05
        {
            return Err("Transcript has invalid, overlapping, or out-of-range timestamps".into());
        }
        end = item.end;
    }
    Ok(())
}

pub fn cloud_endpoint(options: &TranscriptionOptions) -> Result<reqwest::Url, String> {
    let raw = match options.provider.as_str() {
        "groq" => "https://api.groq.com/openai/v1/audio/transcriptions",
        "openai" => "https://api.openai.com/v1/audio/transcriptions",
        "openrouter" => "https://openrouter.ai/api/v1/audio/transcriptions",
        "openaicompatible" => options.endpoint.as_str(),
        _ => return Err("Unknown transcription provider".into()),
    };
    let url = reqwest::Url::parse(raw)
        .map_err(|_| "Enter a complete HTTPS transcription endpoint URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Transcription endpoints must use HTTPS without URL credentials, query, or fragment"
                .into(),
        );
    }
    Ok(url)
}

pub fn cloud_request(
    client: &Client,
    endpoint: reqwest::Url,
    options: &TranscriptionOptions,
    wav: Vec<u8>,
) -> Result<reqwest::blocking::RequestBuilder, String> {
    let request = client.post(endpoint).bearer_auth(options.api_key.trim());
    if options.provider == "openrouter" {
        use base64::Engine;
        let mut body = serde_json::json!({
            "model": options.model,
            "input_audio": { "data": base64::engine::general_purpose::STANDARD.encode(wav), "format": "wav" },
            "response_format": "verbose_json",
            "timestamp_granularities": ["word", "segment"]
        });
        if !options.language.is_empty() {
            body["language"] = Value::String(options.language.clone());
        }
        Ok(request.json(&body))
    } else {
        let mut form = multipart::Form::new()
            .part(
                "file",
                multipart::Part::bytes(wav)
                    .file_name("selection.wav")
                    .mime_str("audio/wav")
                    .map_err(|e| e.to_string())?,
            )
            .text("model", options.model.clone())
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "word")
            .text("timestamp_granularities[]", "segment");
        if !options.language.is_empty() {
            form = form.text("language", options.language.clone());
        }
        Ok(request.multipart(form))
    }
}

pub fn transcribe(
    project: &Project,
    id: Uuid,
    options: TranscriptionOptions,
) -> Result<Transcript, String> {
    let (asset, clip) = selected_audio(project, id)?;
    let duration = clip.duration as f64 / project.settings.fps as f64;
    if options.language.len() > 8
        || !options
            .language
            .chars()
            .all(|c| c.is_ascii_alphabetic() || c == '-')
    {
        return Err(
            "Use a language code such as en, or leave it empty for automatic detection".into(),
        );
    }
    let local = options.provider == "local";
    let endpoint = if local {
        local_available(&options.binary, &options.model)?;
        None
    } else {
        if !options.allow_upload {
            return Err(
                "Confirm uploading the selected audio to the transcription provider".into(),
            );
        }
        if options.api_key.trim().is_empty()
            || options.api_key.len() > 4096
            || options.model.trim().is_empty()
            || options.model.len() > 200
        {
            return Err("Enter an API key and timestamp-capable model".into());
        }
        Some(cloud_endpoint(&options)?)
    };
    let wav = extract_wav(
        asset,
        clip.source_in as f64 / project.settings.fps as f64,
        duration,
    )?;
    let bytes = if local {
        let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let input = temp.path().join("audio.wav");
        let output = temp.path().join("transcript");
        fs::write(&input, wav).map_err(|e| e.to_string())?;
        let input = input.to_str().ok_or("Invalid temporary path")?;
        let output = output.to_str().ok_or("Invalid temporary path")?;
        media::run(
            &options.binary,
            &[
                "-m",
                &options.model,
                "-f",
                input,
                "-ojf",
                "-of",
                output,
                "-sow",
                "-ml",
                "1",
                "-l",
                if options.language.is_empty() {
                    "auto"
                } else {
                    &options.language
                },
            ],
            Duration::from_secs(1200),
            MAX_JSON,
        )?;
        let file = fs::File::open(format!("{output}.json")).map_err(|_| {
            "whisper-cli did not produce JSON; use a whisper.cpp build supporting -ojf"
        })?;
        let mut data = Vec::new();
        file.take(MAX_JSON + 1)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        data
    } else {
        let client = Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "Cannot initialize secure transcription client")?;
        let request = cloud_request(&client, endpoint.unwrap(), &options, wav)?;
        let response = request.send()
            .map_err(|_| "Transcription request failed. Check the endpoint, network, and TLS certificate; no automatic retry was made")?;
        if !response.status().is_success() {
            return Err(format!("Transcription provider returned HTTP {}. Check the API key, quota, model and verbose timestamp support", response.status().as_u16()));
        }
        let mut data = Vec::new();
        response
            .take(MAX_JSON + 1)
            .read_to_end(&mut data)
            .map_err(|_| "Failed reading transcription response")?;
        data
    };
    if bytes.len() as u64 > MAX_JSON {
        return Err("Transcript exceeds the 4 MB response limit".into());
    }
    let value =
        serde_json::from_slice(&bytes).map_err(|_| "Transcription returned invalid JSON")?;
    parse(&value, local, duration)
}

pub fn caption_track(
    project: &Project,
    id: Uuid,
    transcript: &Transcript,
    preset: &str,
) -> Result<Track, String> {
    if !["hormozi", "minimal", "documentary", "neon"].contains(&preset) {
        return Err("Unknown caption preset".into());
    }
    let (_, source) = selected_audio(project, id)?;
    let fps = project.settings.fps as f64;
    validate(transcript, source.duration as f64 / fps)?;
    let mut words: Vec<CaptionWord> = vec![];
    for item in &transcript.items {
        let start = (item.start * fps).round() as u32;
        let end = ((item.end * fps).round() as u32).min(source.duration);
        if end <= start {
            continue;
        }
        words.push(CaptionWord {
            text: item.text.clone(),
            start: source.source_in + start,
            end: source.source_in + end,
        });
    }
    let size = if transcript.timing == "segment" {
        1
    } else if preset == "documentary" {
        10
    } else {
        4
    };
    let clips = words
        .chunks(size)
        .map(|group| {
            let start = group[0].start;
            let end = group.last().unwrap().end;
            Clip {
                id: Uuid::new_v4(),
                asset_id: source.asset_id,
                link_id: None,
                start: source.start + start - source.source_in,
                source_in: start,
                duration: end - start,
                effects: None,
                caption: Some(Caption {
                    preset: preset.into(),
                    timing: transcript.timing.clone(),
                    words: group.to_vec(),
                }),
            }
        })
        .collect::<Vec<_>>();
    if clips.is_empty() {
        return Err("No speech spans at least one project frame".into());
    }
    Ok(Track {
        id: Uuid::new_v4(),
        name: "Captions".into(),
        kind: "caption".into(),
        muted: false,
        solo: false,
        locked: false,
        clips,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn provider_request_contracts_and_endpoint_security() {
        for provider in ["openai", "groq", "openrouter", "openaicompatible"] {
            let options = TranscriptionOptions {
                provider: provider.into(),
                binary: String::new(),
                model: "test-model".into(),
                api_key: "test-only-key".into(),
                endpoint: "https://example.com/v1/audio/transcriptions".into(),
                language: "en".into(),
                allow_upload: true,
            };
            let url = cloud_endpoint(&options).unwrap();
            let request =
                cloud_request(&Client::new(), url, &options, b"RIFF-test-fixture".to_vec())
                    .unwrap()
                    .build()
                    .unwrap();
            assert_eq!(request.method(), reqwest::Method::POST);
            assert_eq!(request.headers()["authorization"], "Bearer test-only-key");
            if provider == "openrouter" {
                assert_eq!(request.headers()["content-type"], "application/json");
                let body: Value =
                    serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
                assert_eq!(body["input_audio"]["format"], "wav");
                assert_eq!(body["timestamp_granularities"], json!(["word", "segment"]));
            } else {
                assert!(request.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("multipart/form-data;"));
            }
        }
        let mut options = TranscriptionOptions {
            provider: "openaicompatible".into(),
            binary: String::new(),
            model: "x".into(),
            api_key: String::new(),
            endpoint: String::new(),
            language: String::new(),
            allow_upload: false,
        };
        for url in [
            "http://example.com/v1",
            "https://key@example.com/v1",
            "https://example.com/v1?key=secret",
        ] {
            options.endpoint = url.into();
            assert!(cloud_endpoint(&options).is_err());
        }
    }
    #[test]
    fn parses_cloud_words_and_segments_without_inventing_timing() {
        let words = parse(
            &json!({"words":[{"word":"Hello", "start":0.1,"end":0.5}]}),
            false,
            1.0,
        )
        .unwrap();
        assert_eq!(words.timing, "word");
        assert_eq!(
            parse(
                &json!({"segments":[{"text":"Hello there", "start":0.1,"end":0.5}]}),
                false,
                1.0
            )
            .unwrap()
            .timing,
            "segment"
        );
        assert!(parse(&json!({"text":"No timestamps"}), false, 1.0).is_err());
        assert!(parse(
            &json!({"words":[{"word":"bad", "start":0.5,"end":0.1}]}),
            false,
            1.0
        )
        .is_err());
    }
    #[test]
    fn parses_whisper_offsets_and_merges_subword_tokens() {
        let result = parse(&json!({"transcription":[{"text":" Hello world", "offsets":{"from":0,"to":900},"tokens":[
            {"text":" Hello","offsets":{"from":0,"to":300}}, {"text":" wor","offsets":{"from":300,"to":500}}, {"text":"ld","offsets":{"from":500,"to":900}}
        ]}]}), true, 1.0).unwrap();
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[1].text, "world");
        assert_eq!(result.items[1].end, 0.9);
        assert!(
            local_available("nonexistent-whisper-binary", "/nonexistent-model")
                .unwrap_err()
                .contains("unavailable")
        );
    }
}
