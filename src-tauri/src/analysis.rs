use crate::{media, timeline::Clip, Project};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

/// One detected shot, clip-relative seconds. `score` is a *heuristic*
/// shot-quality estimate from luma brightness/spread (see
/// [`score_scene`]) — it is NOT machine learning and does NOT detect
/// faces, people, or framing; it only says whether a shot looks
/// normally exposed and textured.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scene {
    pub start: f64,
    pub end: f64,
    pub score: Option<u32>,
}

/// A [`Scene`] expressed in timeline frames, as the rough-cut IPC takes it.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneCut {
    pub start: u32,
    pub end: u32,
    pub score: Option<u32>,
}

/// Result of [`rough_cut`]: ranges to remove, plus the protected hook
/// (the opening that must never be cut).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoughCut {
    pub cuts: Vec<Cut>,
    pub hook: Option<Cut>,
}

pub fn selected_video(project: &Project, id: Uuid) -> Result<(&media::Asset, &Clip), String> {
    project.validate()?;
    let clip = project
        .tracks
        .iter()
        .filter(|t| t.kind != "caption")
        .flat_map(|t| &t.clips)
        .find(|c| c.id == id)
        .ok_or("Select a media clip first")?;
    let asset = project
        .assets
        .iter()
        .find(|a| a.id == clip.asset_id)
        .ok_or("Asset not found")?;
    if asset.video_stream.is_none() {
        return Err("Selected clip has no video".into());
    }
    if clip
        .effects
        .as_ref()
        .and_then(|e| e.finishing.as_ref())
        .is_some_and(|f| f.speed != 1.0)
    {
        return Err("Analyze original-speed video before retiming".into());
    }
    Ok((asset, clip))
}

/// Parse `pts_time:` values out of ffmpeg `showinfo` stderr: one per
/// frame the `select` filter kept, i.e. one per detected scene change.
pub fn parse_showinfo(stderr: &str) -> Vec<f64> {
    let mut times: Vec<f64> = stderr
        .lines()
        .filter(|line| line.contains("showinfo"))
        .filter_map(|line| {
            let after = line.split("pts_time:").nth(1)?;
            let token = after
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-');
            let value: f64 = token.parse().ok()?;
            value.is_finite().then_some(value)
        })
        .collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    times.dedup_by(|a, b| (*a - *b).abs() < 0.001);
    times
}

/// Parse per-frame `(frame number, YAVG, spread)` out of ffmpeg
/// `signalstats` stderr. Two formats are accepted, because FFmpeg
/// versions differ:
/// * legacy log lines — `n:0 … YAVG: 118 YSTD: 60` (spread = YSTD);
/// * `signalstats,metadata=print` lines — `frame:0 pts_time:…` followed
///   by `lavfi.signalstats.YAVG=…` / `YMAX=` / `YMIN=` (ffmpeg 8 no
///   longer emits YSTD at all, so spread = YMAX − YMIN).
/// Lines without a usable YAVG are skipped.
pub fn parse_signalstats(stderr: &str) -> Vec<(u64, f64, f64)> {
    let mut frames: std::collections::BTreeMap<u64, (Option<f64>, Option<f64>)> =
        std::collections::BTreeMap::new();
    let mut maxima: std::collections::HashMap<u64, f64> = std::collections::HashMap::new();
    let mut minima: std::collections::HashMap<u64, f64> = std::collections::HashMap::new();
    let number = |line: &str, key: &str| -> Option<f64> {
        let after = line.split(key).nth(1)?;
        let token = after
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-');
        token.parse().ok()
    };
    let mut current: Option<u64> = None;
    for line in stderr.lines() {
        if line.contains("Parsed_metadata") && line.contains("frame:") {
            current = number(line, "frame:")
                .filter(|v| v.is_finite() && *v >= 0.0)
                .map(|v| v as u64);
            continue;
        }
        if line.contains("lavfi.signalstats.YAVG=") {
            if let (Some(n), Some(yavg)) = (current, number(line, "YAVG=")) {
                frames.entry(n).or_default().0.get_or_insert(yavg);
            }
            continue;
        }
        if line.contains("lavfi.signalstats.YMAX=") {
            if let (Some(n), Some(v)) = (current, number(line, "YMAX=")) {
                maxima.insert(n, v);
            }
            continue;
        }
        if line.contains("lavfi.signalstats.YMIN=") {
            if let (Some(n), Some(v)) = (current, number(line, "YMIN=")) {
                minima.insert(n, v);
            }
            continue;
        }
        if line.contains("signalstats") && line.contains("YAVG:") {
            if let (Some(n), Some(yavg), Some(ystd)) = (
                number(line, "n:"),
                number(line, "YAVG:"),
                number(line, "YSTD:"),
            ) {
                if n.is_finite() && yavg.is_finite() && ystd.is_finite() && n >= 0.0 {
                    frames.entry(n as u64).or_default().0.get_or_insert(yavg);
                    frames.entry(n as u64).or_default().1.get_or_insert(ystd);
                }
            }
        }
    }
    frames
        .into_iter()
        .filter_map(|(n, (yavg, spread))| {
            let yavg = yavg?;
            let spread = spread.or_else(|| match (maxima.get(&n), minima.get(&n)) {
                (Some(max), Some(min)) => Some((max - min).max(0.0)),
                _ => None,
            })?;
            Some((n, yavg, spread))
        })
        .collect()
}

/// Heuristic shot-quality score 0–100 from mean luma brightness
/// (`yavg`, ideal ≈ 118) and luma spread (`ystd`: the per-frame YSTD
/// on older FFmpeg, the YMAX − YMIN range on ffmpeg 8+, where more
/// spread means more texture, saturating at 60). This is a
/// brightness/texture heuristic, not ML and not face detection.
pub fn score_scene(yavg: f64, ystd: f64) -> u32 {
    let brightness = (1.0 - (yavg - 118.0).abs() / 118.0).clamp(0.0, 1.0);
    let texture = (ystd / 60.0).min(1.0).max(0.0);
    (100.0 * (0.6 * brightness + 0.4 * texture)).round() as u32
}

/// Detect scene boundaries in a video clip with ffmpeg only (no new
/// dependencies). Pass 1 finds cuts via the `scene` filter; pass 2
/// scores each scene from per-frame luma stats. Only the first 300 s
/// of the clip's source range are analysed.
pub fn detect_scenes(
    project: &Project,
    clip_id: Uuid,
    threshold: f64,
) -> Result<Vec<Scene>, String> {
    if !threshold.is_finite() || !(0.1..=0.9).contains(&threshold) {
        return Err("Scene threshold must be 0.1–0.9 (default 0.4)".into());
    }
    let (asset, clip) = selected_video(project, clip_id)?;
    let fps = project.settings.fps as f64;
    let source_in = clip.source_in as f64 / fps;
    let duration = (clip.duration as f64 / fps).min(300.0);
    if duration <= 0.0 {
        return Err("Selected clip is too short to analyse".into());
    }
    let path = media::verified_source(asset)?;
    let path = path.to_str().ok_or("Invalid media path")?;
    let source_in = source_in.to_string();
    let duration_text = duration.to_string();
    let stream = asset.video_stream.unwrap();
    // Pass 1: scene-change boundaries.
    let (_, showinfo) = media::run_text(
        "ffmpeg",
        &[
            "-nostdin",
            "-ss",
            &source_in,
            "-i",
            path,
            "-t",
            &duration_text,
            "-map",
            &format!("0:{stream}"),
            "-vf",
            &format!("select='gt(scene,{threshold})',showinfo"),
            "-f",
            "null",
            "-",
        ],
        Duration::from_secs(600),
        8_388_608,
    )?;
    let mut boundaries = vec![0.0];
    for time in parse_showinfo(&showinfo) {
        if time > 0.001 && time < duration - 0.001 {
            boundaries.push(time);
        }
    }
    boundaries.push(duration);
    // Pass 2: per-frame luma stats for the same window. `metadata=print`
    // is required: on ffmpeg 8 signalstats alone logs no per-frame
    // lines, it only attaches frame metadata (see parse_signalstats).
    let frame_fps = asset.fps.filter(|f| *f > 0.0).unwrap_or(fps);
    let (_, stats) = media::run_text(
        "ffmpeg",
        &[
            "-nostdin",
            "-ss",
            &source_in,
            "-i",
            path,
            "-t",
            &duration_text,
            "-map",
            &format!("0:{stream}"),
            "-vf",
            "signalstats,metadata=print",
            "-f",
            "null",
            "-",
        ],
        Duration::from_secs(600),
        8_388_608,
    )?;
    let frames = parse_signalstats(&stats);
    let mut scenes = vec![];
    for window in boundaries.windows(2) {
        let (start, end) = (window[0], window[1]);
        let mut yavg_sum = 0.0;
        let mut ystd_sum = 0.0;
        let mut count = 0usize;
        for (n, yavg, ystd) in &frames {
            let time = *n as f64 / frame_fps;
            if time >= start && (time < end || (end - duration).abs() < 0.001 && time <= end) {
                yavg_sum += yavg;
                ystd_sum += ystd;
                count += 1;
            }
        }
        let score = if count > 0 {
            Some(score_scene(
                yavg_sum / count as f64,
                ystd_sum / count as f64,
            ))
        } else {
            None
        };
        scenes.push(Scene { start, end, score });
    }
    Ok(scenes)
}

fn merge_cuts(mut ranges: Vec<Cut>) -> Vec<Cut> {
    ranges.sort_by_key(|c| (c.start, c.end));
    let mut merged: Vec<Cut> = vec![];
    for cut in ranges {
        if let Some(last) = merged.last_mut() {
            if cut.start <= last.end {
                last.end = last.end.max(cut.end);
                continue;
            }
        }
        merged.push(cut);
    }
    merged
}

/// Combine silence, filler and low-energy cuts into one rough cut,
/// protecting the hook: the first 5 s of the best-scored scene when
/// that scene starts within the first 15 s, otherwise the first 5 s
/// of the timeline. Scene scores only choose the hook — scene ranges
/// are never cuts (scenes tile the timeline, so cutting them would
/// cut everything). Any cut overlapping the hook is dropped.
pub fn rough_cut(
    silence: &[Cut],
    fillers: &[Cut],
    low_energy: &[Cut],
    scenes: &[SceneCut],
    fps: u32,
) -> RoughCut {
    let ranges: Vec<Cut> = silence
        .iter()
        .chain(fillers)
        .chain(low_energy)
        .copied()
        .collect();
    let best = scenes
        .iter()
        .filter(|s| s.score.is_some())
        .max_by_key(|s| s.score.unwrap_or(0));
    let hook_start = match best {
        Some(scene) if scene.start <= 15 * fps => scene.start,
        _ => 0,
    };
    let hook_end = match best {
        Some(scene) if scene.start <= 15 * fps => {
            (scene.start + 5 * fps).min(scene.end.max(scene.start))
        }
        _ => 5 * fps,
    };
    let hook = if hook_end > hook_start {
        Some(Cut {
            start: hook_start,
            end: hook_end,
        })
    } else {
        None
    };
    let cuts = merge_cuts(ranges)
        .into_iter()
        .filter(|c| match hook {
            None => true,
            Some(h) => c.end <= h.start || c.start >= h.end,
        })
        .collect();
    RoughCut { cuts, hook }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SilenceSettings {
    pub threshold_db: f64,
    pub min_silence_ms: u32,
    pub padding_ms: u32,
}
impl Default for SilenceSettings {
    fn default() -> Self {
        Self {
            threshold_db: -38.0,
            min_silence_ms: 400,
            padding_ms: 80,
        }
    }
}
impl SilenceSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !self.threshold_db.is_finite()
            || !(-96.0..=0.0).contains(&self.threshold_db)
            || !(50..=10000).contains(&self.min_silence_ms)
            || self.padding_ms > 2000
        {
            return Err(
                "Use a threshold from -96 to 0 dBFS, silence 50–10000 ms, and padding 0–2000 ms"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Cut {
    pub start: u32,
    pub end: u32,
}

pub fn selected_audio(project: &Project, id: Uuid) -> Result<(&media::Asset, &Clip), String> {
    project.validate()?;
    let clip = project
        .tracks
        .iter()
        .filter(|t| t.kind != "caption")
        .flat_map(|t| &t.clips)
        .find(|c| c.id == id)
        .ok_or("Select a media clip first")?;
    let asset = project
        .assets
        .iter()
        .find(|a| a.id == clip.asset_id)
        .ok_or("Asset not found")?;
    if asset.audio_stream.is_none() {
        return Err("Selected clip has no audio".into());
    }
    if clip
        .effects
        .as_ref()
        .and_then(|e| e.finishing.as_ref())
        .is_some_and(|f| f.speed != 1.0)
    {
        return Err("Analyze original-speed audio before retiming".into());
    }
    Ok((asset, clip))
}

pub struct SilenceDetector {
    settings: SilenceSettings,
    sum: f64,
    samples: usize,
    frames: u64,
    silent_start: Option<u64>,
    ranges: Vec<(f64, f64)>,
}
impl SilenceDetector {
    pub fn new(settings: SilenceSettings) -> Result<Self, String> {
        settings.validate()?;
        Ok(Self {
            settings,
            sum: 0.0,
            samples: 0,
            frames: 0,
            silent_start: None,
            ranges: vec![],
        })
    }
    pub fn push_stereo(&mut self, samples: &[f32]) -> Result<(), String> {
        if samples.len() % 2 != 0 {
            return Err("Incomplete stereo PCM frame".into());
        }
        for pair in samples.chunks_exact(2) {
            if pair.iter().any(|v| !v.is_finite()) {
                return Err("Invalid PCM sample".into());
            }
            self.sum += pair.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
            self.samples += 2;
            if self.samples == 4800 {
                self.window();
            }
        }
        Ok(())
    }
    fn window(&mut self) {
        let start = self.frames;
        self.frames += (self.samples / 2) as u64;
        let rms = (self.sum / self.samples as f64).sqrt();
        if rms < 10f64.powf(self.settings.threshold_db / 20.0) {
            self.silent_start.get_or_insert(start);
        } else {
            self.close(start);
        }
        self.sum = 0.0;
        self.samples = 0;
    }
    fn close(&mut self, end: u64) {
        if let Some(start) = self.silent_start.take() {
            if (end - start) * 1000 >= self.settings.min_silence_ms as u64 * 48000 {
                let padding = self.settings.padding_ms as f64 / 1000.0;
                let (a, b) = (
                    start as f64 / 48000.0 + padding,
                    end as f64 / 48000.0 - padding,
                );
                if b > a {
                    self.ranges.push((a, b));
                }
            }
        }
    }
    pub fn finish(mut self) -> Vec<(f64, f64)> {
        if self.samples > 0 {
            self.window();
        }
        self.close(self.frames);
        self.ranges
    }
}

pub fn detect(
    project: &Project,
    clip_id: Uuid,
    settings: SilenceSettings,
) -> Result<Vec<Cut>, String> {
    let (asset, clip) = selected_audio(project, clip_id)?;
    let fps = project.settings.fps as f64;
    let mut detector = SilenceDetector::new(settings)?;
    let mut offset = 0.0;
    let duration = clip.duration as f64 / fps;
    while offset < duration - 0.000001 {
        let length = (duration - offset).min(4.0);
        let bytes = media::audio_chunk(asset, clip.source_in as f64 / fps + offset, length)?;
        let samples: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        if samples.len() / 2 + 2 < (length * 48000.0).round() as usize {
            return Err("Audio ended before the selected clip; reimport the source".into());
        }
        detector.push_stereo(&samples)?;
        offset += length;
    }
    Ok(detector
        .finish()
        .into_iter()
        .filter_map(|(a, b)| {
            let start = clip.start + (a * fps).ceil() as u32;
            let end = (clip.start + (b * fps).floor() as u32).min(clip.start + clip.duration);
            (end > start).then_some(Cut { start, end })
        })
        .collect())
}

pub fn ripple(project: &Project, cuts: &[Cut], revision: u64) -> Result<Project, String> {
    project.validate()?;
    let end = project
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .map(|c| c.start + c.duration)
        .max()
        .unwrap_or(0);
    if cuts.is_empty()
        || cuts.len() > 2000
        || cuts
            .iter()
            .enumerate()
            .any(|(i, c)| c.start >= c.end || c.end > end || (i > 0 && c.start < cuts[i - 1].end))
    {
        return Err("Cuts must be sorted, non-overlapping, and within the timeline".into());
    }
    if project
        .tracks
        .iter()
        .any(|t| t.locked && t.clips.iter().any(|c| c.start + c.duration > cuts[0].start))
    {
        return Err("Unlock every affected track before ripple deleting".into());
    }
    let shift = |frame: u32| {
        cuts.iter()
            .map(|c| frame.saturating_sub(c.start).min(c.end - c.start))
            .sum::<u32>()
    };
    let mut tracks = project.tracks.clone();
    let mut links = std::collections::HashMap::new();
    for track in &mut tracks {
        let mut output = vec![];
        for clip in &track.clips {
            let mut cursor = clip.start;
            let end = clip.start + clip.duration;
            let mut pieces = vec![];
            for cut in cuts {
                if cut.end <= cursor || cut.start >= end {
                    continue;
                }
                if cut.start > cursor {
                    pieces.push((cursor, cut.start));
                }
                cursor = cut.end.min(end);
            }
            if cursor < end {
                pieces.push((cursor, end));
            }
            for (i, (a, b)) in pieces.into_iter().enumerate() {
                let mut part = clip.clone();
                part.id = if i == 0 { clip.id } else { Uuid::new_v4() };
                part.link_id = clip
                    .link_id
                    .map(|id| *links.entry((id, a, b)).or_insert_with(Uuid::new_v4));
                part.start = a - shift(a);
                part.source_in += clip.source_offset(a - clip.start)?;
                part.duration = b - a;
                output.push(part);
            }
        }
        track.clips = output;
    }
    crate::timeline::replace_tracks(project, tracks, revision)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scene_parsers_score_and_rough_cut() {
        // showinfo: only pts_time values on showinfo lines count.
        let stderr = "[Parsed_showinfo_1 @ 0x1] n: 60 pts: 120 pts_time:2.000 pos: 1 fmt:yuv420p\n\
            [Parsed_select_0 @ 0x2] n: 61 pts_time:9.999\n\
            [Parsed_showinfo_1 @ 0x1] n: 120 pts: 240 pts_time:4.000 pos: 2 fmt:yuv420p\n";
        assert_eq!(parse_showinfo(stderr), vec![2.0, 4.0]);
        assert!(parse_showinfo("").is_empty());
        // signalstats, legacy log format: only lines with n + YAVG + YSTD count.
        let stats =
            "[Parsed_signalstats_0 @ 0x1] n:0 pts:0 YAVG: 118.0 YSTD: 60.0 YMIN: 0 YMAX: 255\n\
            [Parsed_signalstats_0 @ 0x1] n:1 pts:1 YAVG: 100.0 YSTD: 30.0 YMIN: 0 YMAX: 255\n\
            [Parsed_signalstats_0 @ 0x1] n:2 pts:2 YAVG: 90.0\n";
        assert_eq!(
            parse_signalstats(stats),
            vec![(0, 118.0, 60.0), (1, 100.0, 30.0)]
        );
        // signalstats, ffmpeg 8 metadata=print format: spread is YMAX-YMIN.
        let meta = "[Parsed_metadata_1 @ 0x1] frame:0    pts:0       pts_time:0\n\
            [Parsed_metadata_1 @ 0x1] lavfi.signalstats.YMIN=90\n\
            [Parsed_metadata_1 @ 0x1] lavfi.signalstats.YAVG=118\n\
            [Parsed_metadata_1 @ 0x1] lavfi.signalstats.YMAX=150\n\
            [Parsed_metadata_1 @ 0x1] frame:1    pts:512     pts_time:0.0333333\n\
            [Parsed_metadata_1 @ 0x1] lavfi.signalstats.YMIN=80\n\
            [Parsed_metadata_1 @ 0x1] lavfi.signalstats.YAVG=100\n\
            [Parsed_metadata_1 @ 0x1] lavfi.signalstats.YMAX=110\n";
        assert_eq!(
            parse_signalstats(meta),
            vec![(0, 118.0, 60.0), (1, 100.0, 30.0)]
        );
        // Ideal brightness + full texture scores 100; black scores low.
        assert_eq!(score_scene(118.0, 60.0), 100);
        assert_eq!(score_scene(118.0, 0.0), 60);
        assert_eq!(score_scene(0.0, 60.0), 40);
        assert_eq!(score_scene(236.0, 0.0), 0);
        // Rough cut: overlapping ranges merge; the hook is protected.
        let fps = 30;
        let scenes = vec![
            SceneCut {
                start: 0,
                end: 300,
                score: Some(90),
            },
            SceneCut {
                start: 300,
                end: 600,
                score: Some(40),
            },
        ];
        let rough = rough_cut(
            &[Cut { start: 10, end: 40 }, Cut { start: 35, end: 70 }],
            &[Cut {
                start: 200,
                end: 220,
            }],
            &[Cut {
                start: 400,
                end: 450,
            }],
            &scenes,
            fps,
        );
        // Best scene starts at 0 (within 15 s): hook is its first 5 s.
        assert_eq!(rough.hook, Some(Cut { start: 0, end: 150 }));
        // The 10–70 merged range overlaps the hook so it is dropped
        // whole (cuts are dropped, never trimmed). Scene ranges are
        // never cuts, so only the filler and low-energy ranges remain.
        assert_eq!(
            rough.cuts,
            vec![
                Cut {
                    start: 200,
                    end: 220
                },
                Cut {
                    start: 400,
                    end: 450
                }
            ]
        );
        // Best scene starts after 15 s: hook falls back to the timeline start.
        let late = rough_cut(
            &[],
            &[],
            &[Cut {
                start: 200,
                end: 230,
            }],
            &[SceneCut {
                start: 600,
                end: 900,
                score: Some(95),
            }],
            fps,
        );
        assert_eq!(late.hook, Some(Cut { start: 0, end: 150 }));
        // The late best scene only picks the hook — its range is not a cut.
        assert_eq!(
            late.cuts,
            vec![Cut {
                start: 200,
                end: 230
            }]
        );
        // No scored scenes at all: same timeline-start hook.
        let unscored = rough_cut(
            &[Cut {
                start: 200,
                end: 230,
            }],
            &[],
            &[],
            &[SceneCut {
                start: 0,
                end: 300,
                score: None,
            }],
            fps,
        );
        assert_eq!(unscored.hook, Some(Cut { start: 0, end: 150 }));
        // No scored scenes: same timeline-start hook, and the silence
        // range does not overlap it, so it stays.
        assert_eq!(
            unscored.cuts,
            vec![Cut {
                start: 200,
                end: 230
            }]
        );
        // Threshold validation.
        assert!(detect_scenes(&create_project_for_test(), Uuid::new_v4(), 0.05).is_err());
    }
    fn create_project_for_test() -> Project {
        crate::create_project("test".into(), 1920, 1080, 30).unwrap()
    }
    #[test]
    fn windows_padding_minimum_and_antiphase() {
        let mut d = SilenceDetector::new(SilenceSettings::default()).unwrap();
        d.push_stereo(&vec![0.2; 48000 * 2]).unwrap();
        d.push_stereo(&vec![0.0; 48000]).unwrap();
        d.push_stereo(&[0.5, -0.5].repeat(48000)).unwrap();
        d.push_stereo(&vec![0.0; 4800 * 7]).unwrap();
        assert_eq!(d.finish(), vec![(1.08, 1.42)]);
    }
    #[test]
    fn trailing_silence_partial_window_and_invalid_settings() {
        let mut d = SilenceDetector::new(SilenceSettings::default()).unwrap();
        d.push_stereo(&vec![0.0; 48000 + 960]).unwrap();
        assert_eq!(d.finish(), vec![(0.08, 0.43)]);
        assert!(SilenceDetector::new(SilenceSettings {
            threshold_db: f64::NAN,
            ..Default::default()
        })
        .is_err());
    }
}
