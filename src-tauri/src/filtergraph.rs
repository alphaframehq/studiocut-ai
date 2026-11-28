use crate::{edl::Lut, finishing, timeline::Track, Project};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportSettings {
    pub container: String,
    pub height: u32,
    pub fps: u32,
    pub encoder: String,
    /// Canvas shape: `landscape` (16:9), `vertical` (9:16), `square` (1:1).
    /// Defaults keep old payloads (and old clients) working.
    #[serde(default = "default_aspect")]
    pub aspect: String,
    /// `fit` letterboxes/pillarbars into the canvas; `cover` fills the
    /// frame edge to edge and center-crops the overflow.
    #[serde(default = "default_fill")]
    pub fill: String,
}
fn default_aspect() -> String {
    "landscape".into()
}
fn default_fill() -> String {
    "fit".into()
}
impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            container: "mp4".into(),
            height: 1080,
            fps: 30,
            encoder: "auto".into(),
            aspect: default_aspect(),
            fill: default_fill(),
        }
    }
}
impl ExportSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !["mp4", "mov"].contains(&self.container.as_str())
            || ![1080, 2160].contains(&self.height)
            || ![24, 30, 60].contains(&self.fps)
            || ![
                "auto",
                "libx264",
                "h264_nvenc",
                "hevc_videotoolbox",
                "h264_amf",
                "h264_qsv",
            ]
            .contains(&self.encoder.as_str())
            || !["landscape", "vertical", "square"].contains(&self.aspect.as_str())
            || !["fit", "cover"].contains(&self.fill.as_str())
        {
            return Err("Unsupported export settings".into());
        }
        Ok(())
    }
    pub fn width(&self) -> u32 {
        match self.aspect.as_str() {
            "vertical" => {
                // 1080 -> 607.5 -> 608; 2160 -> 1215 -> 1216 (even for yuv420p).
                let w = (self.height as f64 * 9.0 / 16.0).round() as u32;
                if w % 2 == 1 {
                    w + 1
                } else {
                    w
                }
            }
            "square" => self.height,
            _ => self.height * 16 / 9,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Compiled {
    pub inputs: Vec<String>,
    pub graph: String,
    pub resources: BTreeMap<String, String>,
    pub duration: f64,
}
pub fn enabled(track: &Track, tracks: &[Track]) -> bool {
    !track.muted && (!tracks.iter().any(|t| t.kind == track.kind && t.solo) || track.solo)
}
fn number(n: f64) -> String {
    format!("{n:.8}")
}
fn curve(keys: &[(u32, f64)], time: &str, default: f64) -> String {
    let Some(last) = keys.last() else {
        return number(default);
    };
    let mut result = number(last.1);
    for pair in keys.windows(2).rev() {
        let (a, b) = (pair[0], pair[1]);
        result = format!(
            "if(lt({time},{}),{}+({}-{})*({time}-{})/({}-{}),{result})",
            b.0,
            number(a.1),
            number(b.1),
            number(a.1),
            a.0,
            b.0,
            a.0
        );
    }
    format!(
        "if(lt({time},{}),{},{result})",
        keys[0].0,
        number(keys[0].1)
    )
}
fn ass_time(seconds: f64) -> String {
    let cs = (seconds * 100.0).round() as u64;
    format!(
        "{}:{:02}:{:02}.{:02}",
        cs / 360000,
        (cs / 6000) % 60,
        (cs / 100) % 60,
        cs % 100
    )
}
fn ass_text(text: &str) -> String {
    text.replace('\\', "＼")
        .replace('{', "｛")
        .replace('}', "｝")
        .replace(['\r', '\n'], " ")
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}
fn captions(project: &Project, width: u32, height: u32) -> String {
    let font = height as f64 / 24.0;
    let mut result = format!("[Script Info]\nScriptType: v4.00+\nPlayResX: {width}\nPlayResY: {height}\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    for (name, face, bold, color, border) in [
        ("hormozi", "DejaVu Sans", -1, "&H0000FFFF", 1),
        ("minimal", "DejaVu Sans", 0, "&H00FFFFFF", 1),
        ("documentary", "DejaVu Serif", 0, "&H00FFFFFF", 3),
        ("neon", "DejaVu Sans", -1, "&H00FFFF00", 1),
    ] {
        result.push_str(&format!("Style: {name},{face},{font},&H00FFFFFF,{color},&H00000000,&H80000000,{bold},0,0,0,100,100,0,0,{border},2,1,2,80,80,80,1\n"));
    }
    result.push_str("\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
    let fps = project.settings.fps as f64;
    for track in project
        .tracks
        .iter()
        .filter(|t| t.kind == "caption" && enabled(t, &project.tracks))
    {
        for clip in &track.clips {
            let Some(caption) = &clip.caption else {
                continue;
            };
            let mut boundaries = vec![clip.source_in, clip.source_in + clip.duration];
            for word in &caption.words {
                for n in [word.start, word.end] {
                    if n > clip.source_in && n < clip.source_in + clip.duration {
                        boundaries.push(n);
                    }
                }
            }
            boundaries.sort_unstable();
            boundaries.dedup();
            for pair in boundaries.windows(2) {
                let text = caption
                    .words
                    .iter()
                    .map(|word| {
                        let active =
                            word.start <= pair[0] && word.end > pair[0] && caption.timing == "word";
                        let color = if active {
                            if caption.preset == "neon" {
                                "&HFFFF00&"
                            } else {
                                "&H00FFFF&"
                            }
                        } else {
                            "&HFFFFFF&"
                        };
                        format!("{{\\c{color}}}{}", ass_text(&word.text))
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                result.push_str(&format!(
                    "Dialogue: 0,{},{},{},,0,0,0,,{}\n",
                    ass_time((clip.start + pair[0] - clip.source_in) as f64 / fps),
                    ass_time((clip.start + pair[1] - clip.source_in) as f64 / fps),
                    caption.preset,
                    text
                ));
            }
        }
    }
    result
}

pub fn compile(project: &Project, settings: &ExportSettings) -> Result<Compiled, String> {
    project.validate()?;
    settings.validate()?;
    let end = project
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .map(|c| c.start + c.duration)
        .max()
        .unwrap_or(0);
    if end == 0 {
        return Err("Add clips before exporting".into());
    }
    let fps = project.settings.fps as f64;
    let duration = end as f64 / fps;
    let (w, h, r) = (settings.width(), settings.height, settings.fps);
    // `fit` shrinks the project canvas inside the export canvas (bars);
    // `cover` grows it until it fills the canvas, then center-crops.
    let cover = settings.fill == "cover";
    let ratio = if cover {
        (w as f64 / project.settings.width as f64).max(h as f64 / project.settings.height as f64)
    } else {
        (w as f64 / project.settings.width as f64).min(h as f64 / project.settings.height as f64)
    };
    let canvas_w = ((project.settings.width as f64 * ratio / 2.0).floor() as u32 * 2).max(2);
    let canvas_h = ((project.settings.height as f64 * ratio / 2.0).floor() as u32 * 2).max(2);
    let active_clips: usize = project
        .tracks
        .iter()
        .filter(|t| t.kind != "caption" && enabled(t, &project.tracks))
        .map(|t| t.clips.len())
        .sum();
    if active_clips > 128 {
        return Err(
            "Export supports at most 128 active media clips per render to bound decoder resources"
                .into(),
        );
    }
    let mut out = Compiled {
        inputs: vec![],
        graph: String::new(),
        resources: BTreeMap::new(),
        duration,
    };
    let mut nodes = vec![format!(
        "color=c=black:s={w}x{h}:r={r}:d={},format=yuv420p[base0]",
        number(duration)
    )];
    let mut input = 0;
    let mut layer = 0;
    let mut voices = vec![];
    let mut music = vec![];
    let mut ordinary = vec![];
    for track in project
        .tracks
        .iter()
        .rev()
        .filter(|t| t.kind != "caption" && enabled(t, &project.tracks))
    {
        for clip in &track.clips {
            let asset = project
                .assets
                .iter()
                .find(|a| a.id == clip.asset_id)
                .ok_or("Missing clip asset")?;
            let effects = clip.effects.clone().unwrap_or_default();
            let f = effects.finishing.clone().unwrap_or_default();
            let start = clip.start as f64 / fps;
            let length = clip.duration as f64 / fps;
            if asset.kind == "image" {
                out.inputs.extend([
                    "-loop".into(),
                    "1".into(),
                    "-framerate".into(),
                    r.to_string(),
                ]);
            }
            let source = std::fs::canonicalize(&asset.source_path)
                .map_err(|e| format!("Missing media {}: {e}", asset.name))?;
            out.inputs
                .extend(["-i".into(), source.to_string_lossy().into_owned()]);
            let time = format!("(t*{}+{})", number(fps * f.speed), clip.source_in);
            if track.kind == "video" {
                let stream = asset.video_stream.ok_or("Video stream missing")?;
                let fit = if cover {
                    format!("crop={w}:{h}:(iw-ow)/2:(ih-oh)/2")
                } else {
                    format!("pad={w}:{h}:(ow-iw)/2:(oh-ih)/2")
                };
                let mut chain = format!("[{input}:{stream}]trim=start={}:duration={},setpts=(PTS-STARTPTS)/{},fps={r},scale={canvas_w}:{canvas_h}:force_original_aspect_ratio=decrease,pad={canvas_w}:{canvas_h}:(ow-iw)/2:(oh-ih)/2,{fit},setsar=1",number(if asset.kind == "image" {0.0} else {clip.source_in as f64/fps}),number(length*f.speed),number(f.speed));
                if let Some(cube) = &f.cube {
                    finishing::validate_cube(cube)?;
                    let name = format!("lut{input}.cube");
                    out.resources.insert(name.clone(), cube.clone());
                    chain.push_str(&format!(",lut3d=file={name}:interp=tetrahedral"));
                } else if let Some(lut) = &effects.lut {
                    let preset = match lut {
                        Lut::Identity => "identity",
                        Lut::Warm => "warm",
                        Lut::Cool => "cool",
                    };
                    let name = format!("{preset}.cube");
                    out.resources
                        .insert(name.clone(), finishing::preset(preset)?);
                    chain.push_str(&format!(",lut3d=file={name}:interp=tetrahedral"));
                }
                chain.push_str(&format!(
                    ",eq=brightness={}:contrast={}:saturation={},colorbalance=rs={}:gs={}:bs={}",
                    number(f.brightness),
                    number(f.contrast),
                    number(f.saturation),
                    number(f.balance[0]),
                    number(f.balance[1]),
                    number(f.balance[2])
                ));
                if !effects.zoom.is_empty() {
                    let z = curve(
                        &effects
                            .zoom
                            .iter()
                            .map(|k| (k.frame, k.scale))
                            .collect::<Vec<_>>(),
                        &time,
                        1.0,
                    );
                    let x = curve(
                        &effects
                            .zoom
                            .iter()
                            .map(|k| (k.frame, k.x))
                            .collect::<Vec<_>>(),
                        &time,
                        0.5,
                    );
                    let y = curve(
                        &effects
                            .zoom
                            .iter()
                            .map(|k| (k.frame, k.y))
                            .collect::<Vec<_>>(),
                        &time,
                        0.5,
                    );
                    chain.push_str(&format!(",scale=w='trunc({w}*({z})/2)*2':h='trunc({h}*({z})/2)*2':eval=frame,crop={w}:{h}:x='(iw-ow)*({x})':y='(ih-oh)*({y})'"));
                }
                let keys =
                    |get: fn(&crate::finishing::TransformKey) -> f64, clock: &str, default: f64| {
                        curve(
                            &f.transforms
                                .iter()
                                .map(|k| (k.frame, get(k)))
                                .collect::<Vec<_>>(),
                            clock,
                            default,
                        )
                    };
                if !f.transforms.is_empty() {
                    let scale = keys(|k| k.scale, &time, 1.0);
                    let opacity = keys(|k| k.opacity, &time.replace('t', "T"), 1.0);
                    // geq runs BEFORE the animated scale: geq requires
                    // constant-size input frames (it declares no dynamic
                    // frame-size support), while scale with eval=frame
                    // changes the frame size on every frame. FFmpeg 9
                    // rejects that size change at geq ("Frame size or
                    // format changed without filter graph
                    // reinitialization"). The overlay downstream accepts
                    // varying sizes, so the scale stays last. The visual
                    // result is unchanged: geq only copies the colour
                    // planes and sets a uniform alpha, which scale then
                    // scales along with the frame.
                    chain.push_str(&format!(",format=yuva420p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='255*({opacity})',scale=w='max(2,trunc({w}*({scale})/2)*2)':h='max(2,trunc({h}*({scale})/2)*2)':eval=frame"));
                }
                chain.push_str(&format!(",setpts=PTS+{}/TB[v{input}]", number(start)));
                nodes.push(chain);
                let global = format!(
                    "((t-{})*{}+{})",
                    number(start),
                    number(fps * f.speed),
                    clip.source_in
                );
                let x = keys(|k| k.x, &global, 0.0);
                let y = keys(|k| k.y, &global, 0.0);
                nodes.push(format!("[base{layer}][v{input}]overlay=x='(W-w)/2+W*({x})':y='(H-h)/2+H*({y})':eof_action=pass:repeatlast=0:enable='gte(t,{})*lt(t,{})'[base{}]",number(start),number(start+length),layer+1));
                layer += 1;
            } else {
                let stream = asset.audio_stream.ok_or("Audio stream missing")?;
                let mut chain = format!("[{input}:{stream}]atrim=start={}:duration={},asetpts=PTS-STARTPTS,aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo",number(clip.source_in as f64/fps),number(length*f.speed));
                let mut speed = f.speed;
                while speed > 2.0 {
                    chain.push_str(",atempo=2");
                    speed /= 2.0;
                }
                while speed < 0.5 {
                    chain.push_str(",atempo=0.5");
                    speed /= 0.5;
                }
                if speed != 1.0 {
                    chain.push_str(&format!(",atempo={}", number(speed)));
                }
                if f.denoise {
                    chain.push_str(",afftdn=nr=12:nf=-40");
                }
                if f.level {
                    chain.push_str(
                        ",acompressor=threshold=0.125:ratio=3:attack=10:release=150:makeup=2",
                    );
                }
                let gain = curve(
                    &effects
                        .duck
                        .iter()
                        .map(|k| (k.frame, k.gain))
                        .collect::<Vec<_>>(),
                    &time,
                    1.0,
                );
                chain.push_str(&format!(",volume='{}*({gain})':eval=frame,apad,atrim=duration={},asetpts=N/SR/TB+{}/TB,aresample=48000:first_pts=0[a{input}]",number(f.gain),number(length),number(start)));
                nodes.push(chain);
                let label = format!("a{input}");
                if f.role == "voice" {
                    voices.push(label);
                } else if f.role == "music" && f.auto_duck {
                    music.push(label);
                } else {
                    ordinary.push(label);
                }
            }
            input += 1;
        }
    }
    let mix = |labels: &[String], output: &str, nodes: &mut Vec<String>| {
        if labels.is_empty() {
            nodes.push(format!(
                "anullsrc=r=48000:cl=stereo,atrim=duration={}[{output}]",
                number(duration)
            ));
        } else {
            nodes.push(format!("{}amix=inputs={}:normalize=0:duration=longest:dropout_transition=0,apad,atrim=duration={}[{output}]",labels.iter().map(|l|format!("[{l}]")).collect::<String>(),labels.len(),number(duration)));
        }
    };
    if !music.is_empty() && !voices.is_empty() {
        mix(&voices, "voice", &mut nodes);
        mix(&music, "music", &mut nodes);
        nodes.push("[voice]asplit=2[voiceout][sidechain]".into());
        nodes.push("[music][sidechain]sidechaincompress=threshold=0.03:ratio=8:attack=15:release=300[ducked]".into());
        ordinary.extend(["voiceout".into(), "ducked".into()]);
    } else {
        ordinary.extend(voices);
        ordinary.extend(music);
    }
    if ordinary.is_empty() {
        mix(&ordinary, "mixed", &mut nodes);
    } else {
        nodes.push(format!(
            "{}amix=inputs={}:normalize=0:duration=longest:dropout_transition=0[mixed]",
            ordinary
                .iter()
                .map(|l| format!("[{l}]"))
                .collect::<String>(),
            ordinary.len()
        ));
    }
    nodes.push(format!(
        "[mixed]alimiter=limit=0.95:level=0:latency=1,apad,atrim=duration={}[aout]",
        number(duration)
    ));
    if project
        .tracks
        .iter()
        .any(|t| t.kind == "caption" && enabled(t, &project.tracks) && !t.clips.is_empty())
    {
        out.resources
            .insert("captions.ass".into(), captions(project, w, h));
        nodes.push(format!(
            "[base{layer}]ass=filename=captions.ass,format=yuv420p[vout]"
        ));
    } else {
        nodes.push(format!("[base{layer}]format=yuv420p[vout]"));
    }
    out.graph = nodes.join(";\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn settings(aspect: &str, fill: &str, height: u32) -> ExportSettings {
        ExportSettings {
            container: "mp4".into(),
            height,
            fps: 30,
            encoder: "libx264".into(),
            aspect: aspect.into(),
            fill: fill.into(),
        }
    }
    #[test]
    fn export_width_is_derived_per_aspect_and_stays_even() {
        assert_eq!(settings("landscape", "fit", 1080).width(), 1920);
        assert_eq!(settings("landscape", "fit", 2160).width(), 3840);
        // 1080 * 9/16 = 607.5 -> 608; 2160 * 9/16 = 1215 -> 1216.
        assert_eq!(settings("vertical", "fit", 1080).width(), 608);
        assert_eq!(settings("vertical", "cover", 2160).width(), 1216);
        assert_eq!(settings("square", "fit", 1080).width(), 1080);
        assert_eq!(settings("square", "cover", 2160).width(), 2160);
    }
    #[test]
    fn export_settings_reject_unknown_aspect_or_fill() {
        assert!(settings("landscape", "fit", 1080).validate().is_ok());
        assert!(settings("vertical", "cover", 2160).validate().is_ok());
        assert!(settings("portrait", "fit", 1080).validate().is_err());
        assert!(settings("landscape", "stretch", 1080).validate().is_err());
        assert!(settings("landscape", "fit", 720).validate().is_err());
    }
    #[test]
    fn old_export_payloads_without_aspect_still_parse() {
        let parsed: ExportSettings =
            serde_json::from_str(r#"{"container":"mp4","height":1080,"fps":30,"encoder":"auto"}"#)
                .unwrap();
        assert_eq!(parsed.aspect, "landscape");
        assert_eq!(parsed.fill, "fit");
        assert_eq!(parsed.width(), 1920);
    }
}
