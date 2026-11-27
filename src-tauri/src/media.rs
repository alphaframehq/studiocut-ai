use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const MIGRATE_UP: &str = include_str!("../migrations/001_media.up.sql");
pub const MIGRATE_DOWN: &str = include_str!("../migrations/001_media.down.sql");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asset {
    pub id: Uuid,
    pub source_path: String,
    pub name: String,
    pub size_bytes: u64,
    pub modified_ns: String,
    pub kind: String,
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub video_stream: Option<u32>,
    pub audio_stream: Option<u32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub created_at: u64,
    /// Organisational bin label, set from the media library. Lives only
    /// in the project file (the media index DB has no bin column), so a
    /// reimport/relink resets it to "" (Unsorted).
    #[serde(default)]
    pub bin: String,
}

impl Asset {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.get_version_num() != 4
            || self.source_path.is_empty()
            || self.name.is_empty()
            || self.source_path.contains('\0')
            || self.size_bytes == 0
            || self.size_bytes > 9_007_199_254_740_991
            || self.created_at > 9_007_199_254_740_991
            || self.modified_ns.parse::<u128>().is_err()
            || !["video", "audio", "image"].contains(&self.kind.as_str())
            || (self.video_stream.is_none() && self.audio_stream.is_none())
            || (self.kind == "audio" && self.audio_stream.is_none())
            || (self.kind != "audio"
                && (self.video_stream.is_none() || self.width.is_none() || self.height.is_none()))
            || (self.kind != "image" && self.duration.is_none())
            || self
                .duration
                .is_some_and(|n| !n.is_finite() || n <= 0.0 || n > 21600.0)
            || self.bin.contains('\0')
            || self.bin.encode_utf16().count() > 80
            || self.fps.is_some_and(|n| !n.is_finite() || n < 0.0)
            || [self.width, self.height, self.sample_rate, self.channels]
                .iter()
                .any(|v| *v == Some(0))
        {
            return Err("Invalid media metadata (maximum duration: 6 hours)".into());
        }
        Ok(())
    }
}

pub fn open_index(root: &Path) -> Result<Connection, String> {
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let mut db = Connection::open(root.join("media.sqlite3")).map_err(|e| e.to_string())?;
    db.busy_timeout(Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")
        .map_err(|e| e.to_string())?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let version: u32 = tx
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    match version {
        0 => tx.execute_batch(MIGRATE_UP).map_err(|e| e.to_string())?,
        1 => {}
        _ => return Err("Unsupported media index version".into()),
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(db)
}

/// File name of a bundled sidecar binary. Windows sidecars carry the
/// `.exe` suffix; the other platforms ship the bare name.
fn sidecar_file_name(tool: &str, windows: bool) -> String {
    if windows {
        format!("{tool}.exe")
    } else {
        tool.to_string()
    }
}

/// Pure path logic for tool resolution, testable without a real binary or
/// environment mutation. Resolution order:
/// 1. bundled sidecar next to the app executable (`exe_dir`),
/// 2. the `FFMPEG_PATH` / `FFPROBE_PATH` override when non-empty,
/// 3. the bare tool name, resolved on PATH at spawn time.
pub fn resolve_tool_path(tool: &str, env_override: Option<&str>, exe_dir: Option<&Path>) -> String {
    resolve_tool_path_for_platform(tool, env_override, exe_dir, cfg!(windows))
}

fn resolve_tool_path_for_platform(
    tool: &str,
    env_override: Option<&str>,
    exe_dir: Option<&Path>,
    windows: bool,
) -> String {
    if let Some(dir) = exe_dir {
        let candidate = dir.join(sidecar_file_name(tool, windows));
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    if let Some(value) = env_override.filter(|v| !v.trim().is_empty()) {
        return value.to_string();
    }
    tool.to_string()
}

/// Directory holding the running executable; the bundled sidecars live
/// next to it. `None` when the executable path cannot be determined.
fn executable_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
}

/// Resolve the ffmpeg binary: bundled sidecar first, then `FFMPEG_PATH`
/// when set to a non-empty value, otherwise the `ffmpeg` on PATH.
/// (SETUP_BINARIES.md documents these variables; before this, every call
/// site spawned bare `"ffmpeg"` and the documented variables did nothing.)
pub fn ffmpeg_path() -> String {
    resolve_tool_path(
        "ffmpeg",
        std::env::var("FFMPEG_PATH").ok().as_deref(),
        executable_dir().as_deref(),
    )
}

/// Same as [`ffmpeg_path`] for ffprobe (`FFPROBE_PATH`).
pub fn ffprobe_path() -> String {
    resolve_tool_path(
        "ffprobe",
        std::env::var("FFPROBE_PATH").ok().as_deref(),
        executable_dir().as_deref(),
    )
}

/// Resolve a tool name (`"ffmpeg"` / `"ffprobe"`) to the binary that should
/// actually be spawned, honouring the env-var overrides above.
fn resolve_tool(tool: &str) -> String {
    match tool {
        "ffmpeg" => ffmpeg_path(),
        "ffprobe" => ffprobe_path(),
        other => other.to_string(),
    }
}

pub(crate) fn run(
    tool: &str,
    args: &[&str],
    timeout: Duration,
    max_bytes: u64,
) -> Result<fs::File, String> {
    let tool = resolve_tool(tool);
    let tool = tool.as_str();
    let mut output = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut errors = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut command = Command::new(tool);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(output.try_clone().map_err(|e| e.to_string())?)
        .stderr(errors.try_clone().map_err(|e| e.to_string())?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot run {tool}. Install FFmpeg and FFprobe on PATH, or set FFMPEG_PATH / FFPROBE_PATH: {e}"))?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        }
        if start.elapsed() > timeout
            || output.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > max_bytes
            || errors.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > 1_048_576
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{tool} exceeded the processing time or output limit"
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };
    if !status.success() {
        errors.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut detail = String::new();
        errors
            .take(2048)
            .read_to_string(&mut detail)
            .map_err(|e| e.to_string())?;
        return Err(format!("{tool} failed: {detail}"));
    }
    if output.metadata().map_err(|e| e.to_string())?.len() > max_bytes {
        return Err(format!("{tool} output exceeds limit"));
    }
    output.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    Ok(output)
}

/// Like [`run`], but returns stdout and stderr as text instead of a temp
/// file. Same bounded spawn: timeout, stdout capped at `max_bytes`, stderr
/// capped at 4 MB. Used by analysis passes (scene/showinfo, signalstats)
/// whose output is log text, not media bytes.
pub(crate) fn run_text(
    tool: &str,
    args: &[&str],
    timeout: Duration,
    max_bytes: u64,
) -> Result<(String, String), String> {
    let tool = resolve_tool(tool);
    let tool = tool.as_str();
    let mut output = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut errors = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut command = Command::new(tool);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(output.try_clone().map_err(|e| e.to_string())?)
        .stderr(errors.try_clone().map_err(|e| e.to_string())?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot run {tool}. Install FFmpeg and FFprobe on PATH, or set FFMPEG_PATH / FFPROBE_PATH: {e}"))?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        }
        if start.elapsed() > timeout
            || output.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > max_bytes
            || errors.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > 4_194_304
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{tool} exceeded the processing time or output limit"
            ));
        }
        thread::sleep(Duration::from_millis(25));
    };
    if output.metadata().map_err(|e| e.to_string())?.len() > max_bytes {
        return Err(format!("{tool} output exceeds limit"));
    }
    let mut stdout = String::new();
    output.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    output
        .read_to_string(&mut stdout)
        .map_err(|e| e.to_string())?;
    errors.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut stderr = String::new();
    errors
        .read_to_string(&mut stderr)
        .map_err(|e| e.to_string())?;
    if !status.success() {
        let detail: String = stderr.chars().take(2048).collect();
        return Err(format!("{tool} failed: {detail}"));
    }
    Ok((stdout, stderr))
}

pub fn verified_source(asset: &Asset) -> Result<std::path::PathBuf, String> {
    let path = std::path::PathBuf::from(&asset.source_path);
    let (size, modified) = fingerprint(&path)?;
    if size != asset.size_bytes || modified != asset.modified_ns {
        return Err("Source media changed; reimport the file".into());
    }
    Ok(path)
}

pub fn audio_chunk(asset: &Asset, start: f64, duration: f64) -> Result<Vec<u8>, String> {
    if asset.audio_stream.is_none()
        || !start.is_finite()
        || start < 0.0
        || !duration.is_finite()
        || duration <= 0.0
        || duration > 4.0
        || start + duration > asset.duration.unwrap_or(0.0) + 0.001
    {
        return Err("Invalid audio preview range".into());
    }
    let path = verified_source(asset)?;
    let path = path.to_str().ok_or("Invalid media path")?;
    let mut output = run(
        "ffmpeg",
        &[
            "-v",
            "error",
            "-nostdin",
            "-ss",
            &start.to_string(),
            "-i",
            path,
            "-t",
            &duration.to_string(),
            "-map",
            &format!("0:{}", asset.audio_stream.unwrap()),
            "-vn",
            "-ac",
            "2",
            "-ar",
            "48000",
            "-f",
            "f32le",
            "pipe:1",
        ],
        Duration::from_secs(20),
        1_600_000,
    )?;
    let mut bytes = Vec::new();
    output.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.is_empty() || bytes.len() % 8 != 0 {
        return Err("Audio decoder returned invalid samples".into());
    }
    Ok(bytes)
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_str()?.parse().ok())
}
fn integer(value: &Value) -> Option<u32> {
    value
        .as_u64()
        .and_then(|n| n.try_into().ok())
        .or_else(|| value.as_str()?.parse().ok())
}
fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}
fn fingerprint(path: &Path) -> Result<(u64, String), String> {
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("Select a nonempty local media file".into());
    }
    Ok((
        metadata.len(),
        metadata
            .modified()
            .map_err(|e| e.to_string())?
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
            .to_string(),
    ))
}

pub fn import_media(root: &Path, source: &Path) -> Result<Asset, String> {
    let source = source.canonicalize().map_err(|e| e.to_string())?;
    let path = source.to_str().ok_or("Media paths must be valid Unicode")?;
    let (size, modified) = fingerprint(&source)?;
    let db = open_index(root)?;
    let existing: Option<Asset> = db
        .query_row(
            "SELECT * FROM assets WHERE source_path=?1 AND size_bytes=?2 AND modified_ns=?3",
            params![path, size, modified],
            read_asset,
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let asset = if let Some(asset) = existing {
        asset
    } else {
        let probe = run(
            "ffprobe",
            &[
                "-v",
                "error",
                "-protocol_whitelist",
                "file",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
                path,
            ],
            Duration::from_secs(30),
            4_194_304,
        )?;
        let data: Value = serde_json::from_reader(probe).map_err(|e| e.to_string())?;
        let streams = data["streams"].as_array().ok_or("No media streams found")?;
        let video = streams.iter().find(|s| {
            s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_u64() != Some(1)
        });
        let audio = streams.iter().find(|s| s["codec_type"] == "audio");
        let v = video.unwrap_or(&Value::Null);
        let a = audio.unwrap_or(&Value::Null);
        let format = data["format"]["format_name"].as_str().unwrap_or("");
        let image =
            video.is_some() && audio.is_none() && (format.ends_with("_pipe") || format == "image2");
        let duration = number(&data["format"]["duration"])
            .or_else(|| number(&v["duration"]))
            .or_else(|| number(&a["duration"]));
        let fps = v["avg_frame_rate"]
            .as_str()
            .and_then(|s| s.split_once('/'))
            .and_then(|(n, d)| {
                let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
                (d > 0.0).then_some(n / d)
            });
        let asset = Asset {
            id: Uuid::new_v4(),
            source_path: path.into(),
            name: source
                .file_name()
                .ok_or("Invalid media filename")?
                .to_string_lossy()
                .into_owned(),
            size_bytes: size,
            modified_ns: modified.clone(),
            kind: if image {
                "image"
            } else if video.is_some() {
                "video"
            } else {
                "audio"
            }
            .into(),
            duration: if image { None } else { duration },
            width: integer(&v["width"]),
            height: integer(&v["height"]),
            fps,
            video_stream: integer(&v["index"]),
            audio_stream: integer(&a["index"]),
            video_codec: text(&v["codec_name"]),
            audio_codec: text(&a["codec_name"]),
            sample_rate: integer(&a["sample_rate"]),
            channels: integer(&a["channels"]),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis() as u64,
            bin: String::new(),
        };
        asset.validate()?;
        asset
    };
    let cache = root.join(asset.id.to_string());
    if !cache.is_dir()
        || (asset.video_stream.is_some() && !cache.join("thumbnail.jpg").is_file())
        || (asset.audio_stream.is_some() && !cache.join("peaks.json").is_file())
    {
        fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
        let staging = tempfile::tempdir_in(root).map_err(|e| e.to_string())?;
        if let Some(stream) = asset.video_stream {
            let thumbnail = staging.path().join("thumbnail.jpg");
            run(
                "ffmpeg",
                &[
                    "-v",
                    "error",
                    "-nostdin",
                    "-protocol_whitelist",
                    "file",
                    "-i",
                    path,
                    "-map",
                    &format!("0:{stream}"),
                    "-frames:v",
                    "1",
                    "-vf",
                    "scale=320:180:force_original_aspect_ratio=decrease",
                    "-threads",
                    "1",
                    "-n",
                    thumbnail.to_str().ok_or("Invalid cache path")?,
                ],
                Duration::from_secs(120),
                1024,
            )?;
        }
        if let Some(stream) = asset.audio_stream {
            let samples = run(
                "ffmpeg",
                &[
                    "-v",
                    "error",
                    "-nostdin",
                    "-protocol_whitelist",
                    "file",
                    "-i",
                    path,
                    "-map",
                    &format!("0:{stream}"),
                    "-vn",
                    "-ac",
                    "1",
                    "-ar",
                    "8000",
                    "-t",
                    "21600",
                    "-f",
                    "f32le",
                    "pipe:1",
                ],
                Duration::from_secs(600),
                691_200_000,
            )?;
            let count = samples.metadata().map_err(|e| e.to_string())?.len() / 4;
            if count == 0 {
                return Err("Audio stream contains no decodable samples".into());
            }
            let mut samples = std::io::BufReader::new(samples);
            let bucket = count.div_ceil(2000);
            let mut peaks = Vec::<f32>::new();
            let mut peak = 0.0f32;
            let mut sample = [0; 4];
            for i in 0..count {
                samples.read_exact(&mut sample).map_err(|e| e.to_string())?;
                let value = f32::from_le_bytes(sample);
                if !value.is_finite() {
                    return Err("Audio contains invalid samples".into());
                }
                peak = peak.max(value.abs().min(1.0));
                if (i + 1) % bucket == 0 || i + 1 == count {
                    peaks.push(peak);
                    peak = 0.0;
                }
            }
            fs::write(
                staging.path().join("peaks.json"),
                serde_json::to_vec(&peaks).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        if fingerprint(&source)? != (size, modified.clone()) {
            return Err("Media changed during import; retry once copying is finished".into());
        }
        for name in ["thumbnail.jpg", "peaks.json"] {
            let generated = staging.path().join(name);
            if generated.is_file() {
                let mut target =
                    tempfile::NamedTempFile::new_in(&cache).map_err(|e| e.to_string())?;
                std::io::copy(
                    &mut fs::File::open(generated).map_err(|e| e.to_string())?,
                    &mut target,
                )
                .map_err(|e| e.to_string())?;
                target.as_file().sync_all().map_err(|e| e.to_string())?;
                target
                    .persist(cache.join(name))
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    if fingerprint(&source)? != (size, modified) {
        return Err("Media changed during import".into());
    }
    db.execute("INSERT INTO assets VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17) ON CONFLICT(source_path,size_bytes,modified_ns) DO NOTHING",
        params![asset.id.to_string(), asset.source_path, asset.name, asset.size_bytes, asset.modified_ns, asset.kind, asset.duration,
        asset.width, asset.height, asset.fps, asset.video_stream, asset.audio_stream, asset.video_codec, asset.audio_codec, asset.sample_rate, asset.channels, asset.created_at]).map_err(|e| e.to_string())?;
    db.query_row(
        "SELECT * FROM assets WHERE source_path=?1 AND size_bytes=?2 AND modified_ns=?3",
        params![asset.source_path, asset.size_bytes, asset.modified_ns],
        read_asset,
    )
    .map_err(|e| e.to_string())
}

fn read_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<Asset> {
    let id: String = row.get(0)?;
    Ok(Asset {
        id: Uuid::parse_str(&id).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        source_path: row.get(1)?,
        name: row.get(2)?,
        size_bytes: row.get(3)?,
        modified_ns: row.get(4)?,
        kind: row.get(5)?,
        duration: row.get(6)?,
        width: row.get(7)?,
        height: row.get(8)?,
        fps: row.get(9)?,
        video_stream: row.get(10)?,
        audio_stream: row.get(11)?,
        video_codec: row.get(12)?,
        audio_codec: row.get(13)?,
        sample_rate: row.get(14)?,
        channels: row.get(15)?,
        created_at: row.get(16)?,
        bin: String::new(),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub thumbnail: Option<Vec<u8>>,
    pub peaks: Option<Vec<f32>>,
}

pub fn media_preview(root: &Path, asset: &Asset) -> Result<Preview, String> {
    let cache = root.join(asset.id.to_string());
    let thumbnail = if asset.video_stream.is_some() {
        Some(
            fs::read(cache.join("thumbnail.jpg"))
                .map_err(|e| format!("Thumbnail unavailable; reimport the source: {e}"))?,
        )
    } else {
        None
    };
    let peaks = if asset.audio_stream.is_some() {
        Some(
            serde_json::from_reader(
                fs::File::open(cache.join("peaks.json"))
                    .map_err(|e| format!("Waveform unavailable; reimport the source: {e}"))?,
            )
            .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    Ok(Preview { thumbnail, peaks })
}

// --- Hardware-accelerated preview support ---------------------------------
// The program monitor itself is a plain <video> element (see Preview.tsx):
// decoding is done by the operating system's WebView media pipeline,
// which already uses GPU decode where the platform provides it. There is
// no application decode path to accelerate, and no new dependency is
// introduced for one. The realistic hardware win for preview work is the
// FFmpeg encode that produces the 540p proxy: proxy generation runs once
// per clip, and a hardware encoder (NVENC / Quick Sync / AMF) makes it
// substantially faster while the preview keeps playing the original. The
// helpers below resolve which encoder proxy generation should use. They
// live here, next to the FFmpeg tool resolver, so proxy.rs can reuse
// them without duplicating the probe logic already used by export
// (render.rs). Wiring them into generate_proxy is a separate, small
// change in proxy.rs; see the gap-fix report handoff note.

/// Hardware H.264 encoders, in probe order. The order matches the export
/// auto order in render.rs, except that VideoToolbox is absent: it only
/// offers HEVC in the saved preference list, and a proxy must stay H.264
/// because the WebView preview cannot decode HEVC on every platform.
pub const PROXY_HARDWARE_ENCODERS: &[&str] = &["h264_nvenc", "h264_amf", "h264_qsv"];

/// Software H.264 encoder. Always the last candidate and the fallback.
pub const PROXY_SOFTWARE_ENCODER: &str = "libx264";

/// Candidate encoders for proxy generation, from the saved encoder
/// preference (see preferences.ts `ENCODERS`). A HEVC preference
/// (hevc_videotoolbox) resolves to the H.264 hardware order: proxies must
/// stay H.264 for WebView playback. Unknown or empty preferences resolve
/// to the automatic order.
pub fn proxy_encoder_candidates(preference: &str) -> Vec<&'static str> {
    match preference {
        "libx264" => vec![PROXY_SOFTWARE_ENCODER],
        "h264_nvenc" => vec!["h264_nvenc", PROXY_SOFTWARE_ENCODER],
        "h264_amf" => vec!["h264_amf", PROXY_SOFTWARE_ENCODER],
        "h264_qsv" => vec!["h264_qsv", PROXY_SOFTWARE_ENCODER],
        _ => {
            let mut candidates = PROXY_HARDWARE_ENCODERS.to_vec();
            candidates.push(PROXY_SOFTWARE_ENCODER);
            candidates
        }
    }
}

/// Whether an encoder name is one of the known hardware encoders.
pub fn is_hardware_encoder(encoder: &str) -> bool {
    PROXY_HARDWARE_ENCODERS.contains(&encoder)
}

/// Parse the output of `ffmpeg -hide_banner -encoders` into encoder
/// names. Only the name column is kept; the header, the flag legend and
/// the separator line are ignored.
pub fn parse_ffmpeg_encoders(output: &str) -> Vec<String> {
    let mut encoders = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        let flags = fields[0];
        let name = fields[1];
        if flags.len() != 6
            || !flags.bytes().all(|b| b.is_ascii_alphabetic() || b == b'.')
            || name == "="
        {
            continue;
        }
        encoders.push(name.to_string());
    }
    encoders
}

/// Encoder resolved for preview proxy work, as reported to the UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderSelection {
    /// Encoder name FFmpeg should be invoked with.
    pub encoder: String,
    /// True when the encoder is hardware-accelerated.
    pub hardware: bool,
    /// Human-readable note for the preview footer. It states detection
    /// only: hardware availability is a probed fact, and the CPU
    /// fallback statement describes the encoder proxy generation falls
    /// back to when no hardware encoder is available.
    pub note: String,
}

/// Select the first available candidate for a preference. Pure logic:
/// `available` is the list of candidates that actually probed
/// successfully, in preference order. When nothing is available the
/// software encoder is still reported, with a note that no working
/// encoder was found.
pub fn select_encoder(preference: &str, available: &[String]) -> EncoderSelection {
    let selected = proxy_encoder_candidates(preference)
        .into_iter()
        .find(|candidate| available.iter().any(|name| name == candidate));
    match selected {
        Some(encoder) => {
            let hardware = is_hardware_encoder(encoder);
            let note = if hardware {
                format!("Hardware encoder detected: {encoder}.")
            } else if preference == "libx264" {
                "CPU encoding (libx264) selected in preferences.".to_string()
            } else if matches!(preference, "h264_nvenc" | "h264_amf" | "h264_qsv") {
                format!(
                    "{preference} is unavailable — proxy generation uses CPU encoding (libx264)."
                )
            } else {
                "No hardware encoder detected — proxy generation uses CPU encoding (libx264)."
                    .to_string()
            };
            EncoderSelection {
                encoder: encoder.to_string(),
                hardware,
                note,
            }
        }
        None => EncoderSelection {
            encoder: PROXY_SOFTWARE_ENCODER.to_string(),
            hardware: false,
            note: "No working encoder detected. Install a compatible FFmpeg build.".to_string(),
        },
    }
}

/// Quality flags for one proxy encode with the given encoder. The
/// software encoder keeps the settings proxy generation already uses.
/// Hardware encoders use their own constant-quality controls at the same
/// target quality; `-preset` / `-crf` are libx264-only and must not be
/// passed to them.
pub fn proxy_encoder_quality_args(encoder: &str) -> Vec<&'static str> {
    match encoder {
        "h264_nvenc" => vec!["-preset", "fast", "-cq", "23"],
        "h264_qsv" => vec!["-preset", "veryfast", "-global_quality", "23"],
        "h264_amf" => vec![
            "-quality", "speed", "-rc", "cqp", "-qp_i", "23", "-qp_p", "23",
        ],
        _ => vec!["-preset", "veryfast", "-crf", "23"],
    }
}

/// Probe one encoder with a single black frame, mirroring the export
/// probe in render.rs. Listing an encoder does not prove it works (an
/// NVENC build without a GPU still lists h264_nvenc), so proxy
/// generation must probe before trusting the list.
fn probe_encoder(encoder: &str) -> bool {
    run(
        "ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=1920x1080:r=30",
            "-frames:v",
            "1",
            "-an",
            "-c:v",
            encoder,
            "-threads",
            "2",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "null",
            "-",
        ],
        Duration::from_secs(15),
        32_768,
    )
    .is_ok()
}

/// List the encoders the FFmpeg build reports, for the probe pre-filter.
/// An error (or an empty list) means the caller probes every candidate.
fn ffmpeg_encoders() -> Result<Vec<String>, String> {
    let (stdout, _stderr) = run_text(
        "ffmpeg",
        &["-hide_banner", "-encoders"],
        Duration::from_secs(15),
        1_048_576,
    )?;
    Ok(parse_ffmpeg_encoders(&stdout))
}

/// Resolve the encoder proxy generation should use for a preference:
/// probe the candidates in order and return the first that works, with
/// the software encoder as the fallback. Never fails: when nothing
/// probes successfully the selection still reports libx264 with a note
/// that no working encoder was found.
pub fn resolve_preview_encoder(preference: &str) -> EncoderSelection {
    let candidates = proxy_encoder_candidates(preference);
    let listed = ffmpeg_encoders().unwrap_or_default();
    let to_probe: Vec<&'static str> = if listed.is_empty() {
        candidates
    } else {
        candidates
            .into_iter()
            .filter(|candidate| listed.iter().any(|name| name == candidate))
            .collect()
    };
    let mut available = Vec::new();
    for candidate in to_probe {
        if probe_encoder(candidate) {
            available.push(candidate.to_string());
        }
    }
    select_encoder(preference, &available)
}

/// Status of the encoder preview proxy work uses, for the
/// `preview_encoder_status` command (registered by the coordinator in
/// main.rs). Takes the saved preference string from preferences.ts
/// (`getEncoderPreference()`); unknown values resolve to automatic.
pub fn preview_encoder_status(preference: &str) -> EncoderSelection {
    resolve_preview_encoder(preference)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temporary directory containing one fake sidecar file.
    fn sidecar_dir(name: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(name), b"fake binary").unwrap();
        dir
    }

    #[test]
    fn sidecar_present_wins_over_env_override() {
        let dir = sidecar_dir("ffmpeg");
        assert_eq!(
            resolve_tool_path("ffmpeg", Some("/custom/ffmpeg"), Some(dir.path())),
            dir.path().join("ffmpeg").to_string_lossy().into_owned()
        );
    }

    #[test]
    fn env_override_wins_over_path_fallback() {
        assert_eq!(
            resolve_tool_path("ffmpeg", Some("/custom/ffmpeg"), None),
            "/custom/ffmpeg"
        );
        assert_eq!(
            resolve_tool_path("ffprobe", Some("/custom/ffprobe"), None),
            "/custom/ffprobe"
        );
    }

    #[test]
    fn empty_env_override_is_ignored() {
        assert_eq!(resolve_tool_path("ffmpeg", Some(""), None), "ffmpeg");
        assert_eq!(resolve_tool_path("ffprobe", Some("   "), None), "ffprobe");
    }

    #[test]
    fn missing_sidecar_falls_through_to_env_override() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_tool_path("ffmpeg", Some("/custom/ffmpeg"), Some(dir.path())),
            "/custom/ffmpeg"
        );
    }

    #[test]
    fn no_sidecar_and_no_env_falls_back_to_bare_name_on_path() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_tool_path("ffprobe", None, Some(dir.path())),
            "ffprobe"
        );
        assert_eq!(resolve_tool_path("ffmpeg", None, None), "ffmpeg");
    }

    #[test]
    fn windows_sidecar_uses_exe_suffix() {
        let dir = sidecar_dir("ffmpeg.exe");
        assert_eq!(
            resolve_tool_path_for_platform(
                "ffmpeg",
                Some("/custom/ffmpeg"),
                Some(dir.path()),
                true
            ),
            dir.path().join("ffmpeg.exe").to_string_lossy().into_owned()
        );
        // A bare `ffmpeg` file does not count as the Windows sidecar.
        let plain = sidecar_dir("ffmpeg");
        assert_eq!(
            resolve_tool_path_for_platform("ffmpeg", None, Some(plain.path()), true),
            "ffmpeg"
        );
    }

    #[test]
    fn proxy_encoder_candidates_prefer_hardware_then_cpu() {
        let auto = vec!["h264_nvenc", "h264_amf", "h264_qsv", "libx264"];
        assert_eq!(proxy_encoder_candidates("auto"), auto);
        assert_eq!(proxy_encoder_candidates(""), auto);
        assert_eq!(proxy_encoder_candidates("unknown"), auto);
        assert_eq!(
            proxy_encoder_candidates("h264_nvenc"),
            vec!["h264_nvenc", "libx264"]
        );
        assert_eq!(
            proxy_encoder_candidates("h264_amf"),
            vec!["h264_amf", "libx264"]
        );
        assert_eq!(
            proxy_encoder_candidates("h264_qsv"),
            vec!["h264_qsv", "libx264"]
        );
        assert_eq!(proxy_encoder_candidates("libx264"), vec!["libx264"]);
    }

    #[test]
    fn proxy_encoder_candidates_exclude_hevc_for_webview_playback() {
        // hevc_videotoolbox is a valid export preference, but a proxy
        // must stay H.264: the WebView preview cannot decode HEVC on
        // every platform. It resolves to the automatic H.264 order.
        assert_eq!(
            proxy_encoder_candidates("hevc_videotoolbox"),
            vec!["h264_nvenc", "h264_amf", "h264_qsv", "libx264"]
        );
        assert!(!proxy_encoder_candidates("auto").contains(&"hevc_videotoolbox"));
    }

    #[test]
    fn hardware_encoder_detection() {
        assert!(is_hardware_encoder("h264_nvenc"));
        assert!(is_hardware_encoder("h264_amf"));
        assert!(is_hardware_encoder("h264_qsv"));
        assert!(!is_hardware_encoder("libx264"));
        assert!(!is_hardware_encoder("hevc_videotoolbox"));
        assert!(!is_hardware_encoder(""));
    }

    #[test]
    fn ffmpeg_encoder_list_parsing_keeps_only_names() {
        let output = "Encoders:\n V..... = Video\n A..... = Audio\n S..... = Subtitle\n ------\n V..... libx264              libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10\n V..... h264_nvenc           NVIDIA NVENC H.264 encoder\n A..... aac                  AAC (Advanced Audio Coding)\n";
        assert_eq!(
            parse_ffmpeg_encoders(output),
            vec!["libx264", "h264_nvenc", "aac"]
        );
        assert!(parse_ffmpeg_encoders("").is_empty());
        assert!(parse_ffmpeg_encoders("Encoders:\n ------\n").is_empty());
    }

    #[test]
    fn encoder_selection_prefers_first_available_hardware() {
        let available = vec!["h264_qsv".to_string(), "libx264".to_string()];
        let selection = select_encoder("auto", &available);
        assert_eq!(selection.encoder, "h264_qsv");
        assert!(selection.hardware);
        assert!(selection.note.contains("h264_qsv"));
    }

    #[test]
    fn encoder_selection_falls_back_to_cpu_with_a_clear_note() {
        let available = vec!["libx264".to_string()];
        let selection = select_encoder("auto", &available);
        assert_eq!(selection.encoder, "libx264");
        assert!(!selection.hardware);
        assert!(selection.note.contains("No hardware encoder detected"));
        let explicit = select_encoder("h264_nvenc", &available);
        assert_eq!(explicit.encoder, "libx264");
        assert!(!explicit.hardware);
        assert!(explicit.note.contains("h264_nvenc is unavailable"));
    }

    #[test]
    fn encoder_selection_honours_an_explicit_cpu_choice() {
        let available = vec!["h264_nvenc".to_string(), "libx264".to_string()];
        let selection = select_encoder("libx264", &available);
        assert_eq!(selection.encoder, "libx264");
        assert!(!selection.hardware);
        assert!(selection.note.contains("selected in preferences"));
    }

    #[test]
    fn encoder_selection_reports_no_working_encoder() {
        let selection = select_encoder("auto", &[]);
        assert_eq!(selection.encoder, "libx264");
        assert!(!selection.hardware);
        assert!(selection.note.contains("No working encoder detected"));
    }

    #[test]
    fn encoder_selection_round_trips_through_json() {
        let selection = select_encoder("auto", &["libx264".to_string()]);
        let json = serde_json::to_string(&selection).unwrap();
        assert_eq!(
            serde_json::from_str::<EncoderSelection>(&json).unwrap(),
            selection
        );
    }

    #[test]
    fn proxy_encoder_quality_args_are_encoder_specific() {
        assert_eq!(
            proxy_encoder_quality_args("libx264"),
            vec!["-preset", "veryfast", "-crf", "23"]
        );
        assert_eq!(
            proxy_encoder_quality_args("h264_nvenc"),
            vec!["-preset", "fast", "-cq", "23"]
        );
        assert_eq!(
            proxy_encoder_quality_args("h264_qsv"),
            vec!["-preset", "veryfast", "-global_quality", "23"]
        );
        assert_eq!(
            proxy_encoder_quality_args("h264_amf"),
            vec!["-quality", "speed", "-rc", "cqp", "-qp_i", "23", "-qp_p", "23"]
        );
        // Unknown encoders fall back to the software encoder flags.
        assert_eq!(
            proxy_encoder_quality_args("h264_vaapi"),
            vec!["-preset", "veryfast", "-crf", "23"]
        );
    }
}
