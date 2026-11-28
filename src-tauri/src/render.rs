use crate::{
    filtergraph::{self, ExportSettings},
    media::{ffmpeg_path, ffprobe_path},
    Project,
};
use serde::Serialize;
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub version: u32,
    pub id: Uuid,
    pub sequence: u64,
    pub timestamp: u64,
    pub state: String,
    pub progress: f64,
    pub encoder: Option<String>,
    pub disclosure: String,
    pub output: String,
    pub error: Option<String>,
}
impl Snapshot {
    fn active(&self) -> bool {
        ["probing", "rendering", "validating"].contains(&self.state.as_str())
    }
}
struct Job {
    snapshot: Snapshot,
    cancel: Arc<AtomicBool>,
    channel: crate::progress_channel::ProgressChannel,
}
#[derive(Clone, Default)]
pub struct ExportManager {
    job: Arc<Mutex<Option<Job>>>,
    readers: Arc<crate::progress_channel::ReaderLimit>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Parse the major version from `ffmpeg -version` output, e.g.
/// `ffmpeg version 9.1 ...` -> `Some(9)`. Git master builds report
/// `ffmpeg version N-...`, which carries no release number -> `None`.
fn parse_ffmpeg_major_version(banner: &str) -> Option<u32> {
    let version = banner.split("version ").nth(1)?;
    let digits: String = version.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Whether the FFmpeg binary understands the generic read-from-file
/// option syntax `-/filter_complex <file>`.
/// FFmpeg 9.0 removed `-filter_complex_script` (and `-filter_script`);
/// the `-/` syntax that replaces it was added in FFmpeg 7.1. The
/// bundled sidecars are FFmpeg 9+, but a system FFmpeg can be older,
/// so the flag is picked from the detected major version. A git master
/// build (`N-...`) is newer than any release, and an unparseable banner
/// or a failed probe is treated as current: every bundled build is
/// FFmpeg 9+.
fn ffmpeg_supports_slash_file_option() -> bool {
    let mut command = Command::new(ffmpeg_path());
    command.args(["-hide_banner", "-version"]);
    let cancel = AtomicBool::new(false);
    match bounded_command(command, &cancel, Duration::from_secs(10), 65536) {
        Ok(bytes) => match parse_ffmpeg_major_version(&String::from_utf8_lossy(&bytes)) {
            Some(major) => major >= 7,
            None => true,
        },
        Err(_) => true,
    }
}

/// The two arguments that hand the compiled filtergraph file to FFmpeg,
/// chosen for the FFmpeg version in use. The file contents are identical
/// either way; only the flag differs.
fn filter_complex_file_args() -> [&'static str; 2] {
    if ffmpeg_supports_slash_file_option() {
        ["-/filter_complex", "graph.txt"]
    } else {
        ["-filter_complex_script", "graph.txt"]
    }
}
impl ExportManager {
    pub fn start(
        &self,
        project: Project,
        output: PathBuf,
        settings: ExportSettings,
    ) -> Result<Snapshot, String> {
        project.validate()?;
        settings.validate()?;
        if !output.is_absolute()
            || output.extension().and_then(|s| s.to_str()) != Some(settings.container.as_str())
        {
            return Err("Use an absolute output path with the selected container extension".into());
        }
        let parent = output.parent().ok_or("Output directory missing")?;
        if !parent.is_dir() || output.exists() || fs::symlink_metadata(&output).is_ok() {
            return Err(
                "Output must be a new file in an existing directory; overwrite is disabled".into(),
            );
        }
        if project.tracks.iter().all(|t| t.clips.is_empty()) {
            return Err("Add clips before exporting".into());
        }
        let mut guard = self.job.lock().map_err(|e| e.to_string())?;
        if guard.as_ref().is_some_and(|j| j.snapshot.active()) {
            return Err("An export is already running".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let snapshot = Snapshot {
            version: 1,
            id: Uuid::new_v4(),
            sequence: 1,
            timestamp: now(),
            state: "probing".into(),
            progress: 0.0,
            encoder: None,
            disclosure: "Testing encoder availability; CPU fallback will be reported here.".into(),
            output: output.to_string_lossy().into_owned(),
            error: None,
        };
        *guard = Some(Job {
            snapshot: snapshot.clone(),
            cancel: cancel.clone(),
            channel: crate::progress_channel::ProgressChannel::new(snapshot.clone()),
        });
        let manager = self.clone();
        let id = snapshot.id;
        thread::spawn(move || {
            let result = manager.run(id, &project, &output, &settings, &cancel);
            if let Err(error) = result {
                manager.update(id, |s| {
                    s.state = if cancel.load(Ordering::SeqCst) {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                    s.error = Some(error);
                });
            }
        });
        Ok(snapshot)
    }
    pub fn status(&self, id: Option<Uuid>, after: Option<u64>) -> Result<Option<Snapshot>, String> {
        let guard = self.job.lock().map_err(|e| e.to_string())?;
        let Some(job) = guard.as_ref() else {
            return Ok(None);
        };
        if id.is_some_and(|id| id != job.snapshot.id) {
            return Err("Export job is no longer retained".into());
        }
        if after.is_some_and(|n| n > job.snapshot.sequence) {
            return Err("Invalid future export sequence".into());
        }
        Ok(Some(job.snapshot.clone()))
    }
    pub fn progress(
        &self,
        id: Option<Uuid>,
        after: Option<u64>,
    ) -> Result<Option<crate::progress_channel::ProgressBatch>, String> {
        let _permit = self.readers.acquire()?;
        let guard = self.job.lock().map_err(|e| e.to_string())?;
        let Some(job) = guard.as_ref() else {
            return Ok(None);
        };
        let cursor = if id == Some(job.snapshot.id) {
            after
        } else {
            None
        };
        job.channel.replay(cursor).map(Some)
    }
    pub fn cancel(&self, id: Uuid) -> Result<Snapshot, String> {
        let mut guard = self.job.lock().map_err(|e| e.to_string())?;
        let job = guard
            .as_mut()
            .filter(|j| j.snapshot.id == id)
            .ok_or("Export job not found")?;
        if job.snapshot.active() {
            job.cancel.store(true, Ordering::SeqCst);
        }
        Ok(job.snapshot.clone())
    }
    fn update(&self, id: Uuid, change: impl FnOnce(&mut Snapshot)) {
        if let Ok(mut guard) = self.job.lock() {
            if let Some(job) = guard.as_mut().filter(|j| j.snapshot.id == id) {
                change(&mut job.snapshot);
                job.snapshot.sequence += 1;
                job.snapshot.timestamp = now();
                job.channel.publish(job.snapshot.clone());
            }
        }
    }
    fn run(
        &self,
        id: Uuid,
        project: &Project,
        output: &Path,
        settings: &ExportSettings,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let directory = tempfile::Builder::new()
            .prefix(".studiocut-export-")
            .tempdir_in(output.parent().ok_or("Missing output parent")?)
            .map_err(|e| e.to_string())?;
        let root = directory.path();
        for asset in project.assets.iter().filter(|a| {
            project.tracks.iter().any(|t| {
                t.kind != "caption"
                    && filtergraph::enabled(t, &project.tracks)
                    && t.clips.iter().any(|c| c.asset_id == a.id)
            })
        }) {
            crate::media::verified_source(asset)?;
        }
        let compiled = filtergraph::compile(project, settings)?;
        for (name, text) in &compiled.resources {
            fs::write(root.join(name), text).map_err(|e| e.to_string())?;
        }
        fs::write(root.join("graph.txt"), &compiled.graph).map_err(|e| e.to_string())?;
        let candidates: Vec<&str> = if settings.encoder == "auto" {
            vec![
                "h264_nvenc",
                "hevc_videotoolbox",
                "h264_amf",
                "h264_qsv",
                "libx264",
            ]
        } else if settings.encoder == "libx264" {
            vec!["libx264"]
        } else {
            vec![settings.encoder.as_str(), "libx264"]
        };
        let mut selected = None;
        for candidate in candidates {
            if cancel.load(Ordering::SeqCst) {
                return Err("Export cancelled".into());
            }
            let mut command = Command::new(ffmpeg_path());
            command.current_dir(root).args([
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
                candidate,
                "-threads",
                "2",
                "-pix_fmt",
                "yuv420p",
                "-f",
                "null",
                "-",
            ]);
            if bounded_command(command, cancel, Duration::from_secs(15), 32768).is_ok() {
                selected = Some(candidate.to_string());
                break;
            }
        }
        let encoder = selected
            .ok_or("No working encoder, including libx264. Install a compatible FFmpeg build.")?;
        self.update(id, |s| {
            s.encoder = Some(encoder.clone());
            s.state = "rendering".into();
            s.disclosure = if encoder == "libx264" && settings.encoder != "libx264" {
                "Hardware encoder unavailable. Export is using libx264 CPU fallback.".into()
            } else {
                format!("Export encoder: {encoder}")
            };
        });
        let temporary = root.join(format!("output.{}", settings.container));
        let filter_complex = filter_complex_file_args();
        let mut command = Command::new(ffmpeg_path());
        command
            .current_dir(root)
            .args([
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-n",
                "-filter_complex_threads",
                "1",
            ])
            .args(&compiled.inputs)
            .args(filter_complex)
            .args([
                "-map", "[vout]", "-map", "[aout]", "-c:v", &encoder, "-threads", "2", "-pix_fmt",
                "yuv420p",
            ]);
        if encoder == "libx264" {
            command.args(["-preset", "veryfast", "-crf", "20"]);
        }
        if encoder == "hevc_videotoolbox" {
            command.args(["-tag:v", "hvc1"]);
        }
        command
            .args([
                "-c:a",
                "aac",
                "-b:a",
                "192k",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-t",
                &format!("{:.8}", compiled.duration),
                "-r",
                &settings.fps.to_string(),
                "-movflags",
                "+faststart",
                "-progress",
                "pipe:1",
                "-stats_period",
                "0.1",
                "-nostats",
            ])
            .arg(&temporary)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| format!("Cannot launch FFmpeg: {e}"))?;
        let stdout = child.stdout.take().ok_or("Missing progress pipe")?;
        let stderr = child.stderr.take().ok_or("Missing diagnostics pipe")?;
        let manager = self.clone();
        let duration = compiled.duration;
        let progress = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut buffer = Vec::new();
            loop {
                buffer.clear();
                let read = reader.by_ref().take(4097).read_until(b'\n', &mut buffer);
                if !matches!(read,Ok(n) if n>0) || buffer.len() > 4096 {
                    break;
                }
                if let Ok(line) = std::str::from_utf8(&buffer) {
                    if let Some(value) = parse_progress(line, duration) {
                        manager.update(id, |s| s.progress = s.progress.max(value));
                    }
                }
            }
        });
        let errors = thread::spawn(move || bounded_drain(stderr, 65536));
        let started = Instant::now();
        let status = loop {
            if cancel.load(Ordering::SeqCst) || started.elapsed() > Duration::from_secs(86400) {
                let _ = child.kill();
                break child.wait();
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => thread::sleep(Duration::from_millis(25)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(e);
                }
            }
        };
        let _ = progress.join();
        let diagnostics = errors.join().unwrap_or_default();
        if cancel.load(Ordering::SeqCst) {
            return Err("Export cancelled; temporary files removed".into());
        }
        if !status.map_err(|e| e.to_string())?.success() {
            return Err(format!(
                "FFmpeg export failed: {}",
                String::from_utf8_lossy(&diagnostics)
            ));
        }
        self.update(id, |s| s.state = "validating".into());
        validate_output(&temporary, settings, compiled.duration, cancel)?;
        // sync_all is FlushFileBuffers on Windows, which needs a handle
        // opened with write access; a read-only handle fails there with
        // "Access is denied" (os error 5).
        fs::OpenOptions::new()
            .write(true)
            .open(&temporary)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        let mut guard = self.job.lock().map_err(|e| e.to_string())?;
        let job = guard
            .as_mut()
            .filter(|j| j.snapshot.id == id)
            .ok_or("Export job missing")?;
        if cancel.load(Ordering::SeqCst) {
            return Err("Export cancelled; temporary files removed".into());
        }
        fs::hard_link(&temporary, output)
            .map_err(|e| format!("Cannot atomically publish output (never overwrites): {e}"))?;
        drop(directory);
        job.snapshot.state = "completed".into();
        job.snapshot.progress = 1.0;
        job.snapshot.sequence += 1;
        job.snapshot.timestamp = now();
        job.channel.publish(job.snapshot.clone());
        Ok(())
    }
}
fn bounded_drain(mut reader: impl Read, limit: usize) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buffer = [0; 8192];
    while let Ok(n) = reader.read(&mut buffer) {
        if n == 0 {
            break;
        }
        let remaining = limit.saturating_sub(result.len());
        result.extend_from_slice(&buffer[..n.min(remaining)]);
    }
    result
}
pub fn bounded_command(
    mut command: Command,
    cancel: &AtomicBool,
    timeout: Duration,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("Missing stdout")?;
    let stderr = child.stderr.take().ok_or("Missing stderr")?;
    let out = thread::spawn(move || bounded_drain(stdout, limit + 1));
    let err = thread::spawn(move || bounded_drain(stderr, 32768));
    let start = Instant::now();
    let status = loop {
        if cancel.load(Ordering::SeqCst) || start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out.join();
            let _ = err.join();
            return Err("Process cancelled or timed out".into());
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out.join();
                let _ = err.join();
                return Err(e.to_string());
            }
        }
    };
    let bytes = out.join().map_err(|_| "Output reader failed")?;
    let errors = err.join().unwrap_or_default();
    if !status.success() {
        return Err(String::from_utf8_lossy(&errors).into_owned());
    }
    if bytes.len() > limit {
        return Err("Process output exceeded limit".into());
    }
    Ok(bytes)
}
pub fn parse_progress(line: &str, duration: f64) -> Option<f64> {
    let value = line
        .trim()
        .strip_prefix("out_time_us=")?
        .parse::<f64>()
        .ok()?;
    if !value.is_finite() || value < 0.0 || duration <= 0.0 {
        return None;
    }
    Some((value / 1_000_000.0 / duration).clamp(0.0, 0.99))
}
pub fn validate_output(
    path: &Path,
    settings: &ExportSettings,
    duration: f64,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut command = Command::new(ffprobe_path());
    command
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path);
    let bytes = bounded_command(command, cancel, Duration::from_secs(30), 262144)?;
    let data: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let streams = data["streams"]
        .as_array()
        .ok_or("FFprobe streams missing")?;
    let video = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .ok_or("Export has no video")?;
    let audio = streams
        .iter()
        .find(|s| s["codec_type"] == "audio")
        .ok_or("Export has no audio")?;
    let seconds = data["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or("Export duration missing")?;
    for (kind, stream) in [("video", video), ("audio", audio)] {
        let stream_duration = stream["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or_else(|| format!("Export {kind} stream duration missing"))?;
        if !stream_duration.is_finite() || (stream_duration - duration).abs() > 0.15 {
            return Err(format!(
                "FFprobe validation failed: {kind} stream duration {stream_duration}s differs from expected {duration}s"
            ));
        }
    }
    let frame_rate = video["avg_frame_rate"]
        .as_str()
        .and_then(|s| s.split_once('/'))
        .and_then(|(a, b)| Some(a.parse::<f64>().ok()? / b.parse::<f64>().ok()?))
        .ok_or("Export frame rate missing")?;
    if video["width"] != settings.width()
        || video["height"] != settings.height
        || !frame_rate.is_finite()
        || (frame_rate - settings.fps as f64).abs() > 0.01
        || audio["codec_name"] != "aac"
        || audio["sample_rate"] != "48000"
        || !seconds.is_finite()
        || (seconds - duration).abs() > 0.15
        || ![Some("h264"), Some("hevc")].contains(&video["codec_name"].as_str())
    {
        return Err(
            "FFprobe validation failed: resolution, codec, audio, FPS or duration mismatch".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod qualification_tests {
    use super::*;

    #[test]
    fn ffmpeg_major_version_parses_release_and_master_banners() {
        assert_eq!(
            parse_ffmpeg_major_version("ffmpeg version 9.1 Copyright (c) 2000-2026"),
            Some(9)
        );
        assert_eq!(
            parse_ffmpeg_major_version("ffmpeg version 8.1.2 Copyright (c) 2000-2026"),
            Some(8)
        );
        assert_eq!(
            parse_ffmpeg_major_version("ffmpeg version 6.1.1 Copyright (c) 2000-2023"),
            Some(6)
        );
        assert_eq!(
            parse_ffmpeg_major_version("ffmpeg version 4.4.2-0ubuntu0.22.04.1"),
            Some(4)
        );
        // Git master builds carry no release number; they are newer than
        // any release, so the caller treats them as current.
        assert_eq!(
            parse_ffmpeg_major_version("ffmpeg version N-120123-gabcdef Copyright (c)"),
            None
        );
        assert_eq!(parse_ffmpeg_major_version("not ffmpeg at all"), None);
        assert_eq!(parse_ffmpeg_major_version(""), None);
    }

    #[test]
    fn slash_file_probe_agrees_with_the_local_ffmpeg_version() {
        let mut command = Command::new(ffmpeg_path());
        command.args(["-hide_banner", "-version"]);
        let cancel = AtomicBool::new(false);
        let bytes = bounded_command(command, &cancel, Duration::from_secs(10), 65536).unwrap();
        let major = parse_ffmpeg_major_version(&String::from_utf8_lossy(&bytes));
        assert_eq!(
            ffmpeg_supports_slash_file_option(),
            major.map_or(true, |m| m >= 7)
        );
    }

    #[test]
    fn rejects_truncated_audio_despite_full_container_duration() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("truncated.mp4");
        let mut command = Command::new(ffmpeg_path());
        command
            .args([
                "-v",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "color=black:s=1920x1080:r=30:d=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=0.5",
                "-c:v",
                "libx264",
                "-threads",
                "2",
                "-preset",
                "veryfast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-ar",
                "48000",
                "-ac",
                "2",
            ])
            .arg(&output);
        let cancel = AtomicBool::new(false);
        bounded_command(command, &cancel, Duration::from_secs(30), 4096).unwrap();
        let error = validate_output(
            &output,
            &ExportSettings {
                container: "mp4".into(),
                height: 1080,
                fps: 30,
                encoder: "libx264".into(),
                ..Default::default()
            },
            2.0,
            &cancel,
        )
        .unwrap_err();
        assert!(error.contains("audio stream duration"), "{error}");
    }
}
