use crate::{media, timeline::Clip, Project};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
