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

/// Resolve the ffmpeg binary: `FFMPEG_PATH` wins when set to a non-empty
/// value, otherwise fall back to the `ffmpeg` on PATH.
/// (SETUP_BINARIES.md documents these variables; before this, every call
/// site spawned bare `"ffmpeg"` and the documented variables did nothing.)
pub(crate) fn ffmpeg_path() -> String {
    std::env::var("FFMPEG_PATH")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "ffmpeg".to_string())
}

/// Same as [`ffmpeg_path`] for ffprobe (`FFPROBE_PATH`).
pub(crate) fn ffprobe_path() -> String {
    std::env::var("FFPROBE_PATH")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "ffprobe".to_string())
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
