use crate::{edl::Lut, finishing, timeline::Track, Project};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

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
    /// Render from a clip's 540p proxy file when one is ready, falling
    /// back to the original media otherwise. Defaults to `false`: an
    /// export never silently uses low-resolution proxies. Old payloads
    /// omit this field and keep the original-only behaviour.
    #[serde(default)]
    pub use_proxy: bool,
    /// Per-track mixer gain in dB, keyed by track id, as stored by the
    /// audio mixer. Applied to the export audio through FFmpeg volume
    /// filters. Defaults to empty (no mixer gain). Values are clamped
    /// to the mixer range at use; see `scopes::audio_gain`.
    #[serde(default)]
    pub track_gains: BTreeMap<String, f64>,
    /// Face tracks for `cover` exports, keyed by clip id: when a clip
    /// has a face-tracked reframe plan (see
    /// [`crate::ai_gaps::reframe_face_track_clip`]), the cover crop
    /// follows the track instead of staying centred. The keyframe
    /// shape mirrors [`crate::ai_gaps::ReframeKeyframe`]. Defaults to
    /// empty (centre crop); old payloads omit this field. Tracks are
    /// ignored for `fit` exports and for clips whose crop already
    /// fills the frame.
    #[serde(default)]
    pub face_tracks: BTreeMap<String, Vec<FaceTrackKeyframe>>,
}

/// One keyframe of a face track for export: at `t_seconds` into the
/// clip the crop is centred on (`focus_x`, `focus_y`) in normalised
/// frame coordinates. The shape mirrors
/// [`crate::ai_gaps::ReframeKeyframe`] (the plan type); it is defined
/// here so the export layer does not depend on the analysis layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FaceTrackKeyframe {
    pub t_seconds: f64,
    pub focus_x: f64,
    pub focus_y: f64,
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
            use_proxy: false,
            track_gains: BTreeMap::new(),
            face_tracks: BTreeMap::new(),
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
            || self.track_gains.values().any(|gain| !gain.is_finite())
            || self.face_tracks.values().flatten().any(|point| {
                !point.t_seconds.is_finite()
                    || point.t_seconds < 0.0
                    || !point.focus_x.is_finite()
                    || !(0.0..=1.0).contains(&point.focus_x)
                    || !point.focus_y.is_finite()
                    || !(0.0..=1.0).contains(&point.focus_y)
            })
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
    /// Clips whose source resolved to a ready 540p proxy file.
    pub proxy_clips: usize,
    /// Media clips compiled into this export (video and audio alike).
    pub total_clips: usize,
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

/// Time-varying `cover` crop for a clip with a face track, or `None`
/// when no usable track exists for that clip. The crop window follows
/// the face: `x = focus_x * iw - ow / 2`, clamped into the frame (and
/// likewise for `y`). Keyframe times are clip-relative seconds; they
/// are converted to source frame numbers (`source_in + t_seconds *
/// fps`) in the same domain as the `time` expression the zoom
/// keyframes use, so the curve stays correct at any clip speed.
/// Returns `None` when the track is empty, its points are out of
/// range, or the crop already fills the frame (no room to move).
fn face_cover_crop(
    tracks: &BTreeMap<String, Vec<FaceTrackKeyframe>>,
    clip_id: &str,
    source_in: u32,
    fps: f64,
    out_w: u32,
    out_h: u32,
    canvas_w: u32,
    canvas_h: u32,
    time: &str,
) -> Option<String> {
    let points = tracks.get(clip_id)?;
    if points.is_empty() || (canvas_w <= out_w && canvas_h <= out_h) {
        return None;
    }
    let mut points: Vec<&FaceTrackKeyframe> = points.iter().collect();
    points.sort_by(|a, b| {
        a.t_seconds
            .partial_cmp(&b.t_seconds)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // The curve helper needs strictly increasing keys; when two
    // keyframes share a time, the later one wins.
    let mut deduped: Vec<&FaceTrackKeyframe> = Vec::with_capacity(points.len());
    for point in points {
        if let Some(last) = deduped.last_mut() {
            if last.t_seconds == point.t_seconds {
                *last = point;
                continue;
            }
        }
        deduped.push(point);
    }
    let points = deduped;
    if points.is_empty() {
        return None;
    }
    let keys = |get: fn(&FaceTrackKeyframe) -> f64| -> Vec<(u32, f64)> {
        points
            .iter()
            .map(|point| {
                let frame = source_in as f64 + point.t_seconds * fps;
                (
                    (frame.round() as u64).min(u32::MAX as u64) as u32,
                    get(point),
                )
            })
            .collect()
    };
    let x = curve(&keys(|point| point.focus_x), time, 0.5);
    let y = curve(&keys(|point| point.focus_y), time, 0.5);
    Some(format!(
        "crop={out_w}:{out_h}:x='min(max(({x})*iw-ow/2,0),iw-ow)':y='min(max(({y})*ih-oh/2,0),ih-oh)'"
    ))
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

/// Compile the export filtergraph from the original media only.
/// Proxy-aware callers use [`compile_with_media_root`].
pub fn compile(project: &Project, settings: &ExportSettings) -> Result<Compiled, String> {
    compile_with_media_root(project, settings, None)
}

/// Compile the export filtergraph. When `media_root` is given and
/// `settings.use_proxy` is set, a clip reads from its ready 540p proxy
/// file and falls back to the original media when no proxy is ready
/// (see `proxy::export_source`); the original files are never written.
/// With `media_root` unset, proxy export is rejected rather than
/// silently falling back to originals for every clip.
pub fn compile_with_media_root(
    project: &Project,
    settings: &ExportSettings,
    media_root: Option<&Path>,
) -> Result<Compiled, String> {
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
        proxy_clips: 0,
        total_clips: 0,
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
            // Proxy export resolves the source through the proxy
            // module, which only serves a ready proxy whose recorded
            // fingerprint still matches the original; anything else
            // falls back to the original media. Without a media root
            // there is no proxy to resolve, so a proxy request fails
            // instead of silently exporting originals.
            let (source, used_proxy) = match media_root {
                Some(root) => match crate::proxy::export_source(root, asset, settings.use_proxy)? {
                    crate::proxy::ExportSource::Original(path) => (path, false),
                    crate::proxy::ExportSource::Proxy(path) => (path, true),
                },
                None => {
                    if settings.use_proxy {
                        return Err("Proxy export needs the media cache location".into());
                    }
                    (std::path::PathBuf::from(&asset.source_path), false)
                }
            };
            let source = std::fs::canonicalize(&source)
                .map_err(|e| format!("Missing media {}: {e}", asset.name))?;
            if used_proxy {
                out.proxy_clips += 1;
            }
            out.total_clips += 1;
            out.inputs
                .extend(["-i".into(), source.to_string_lossy().into_owned()]);
            let time = format!("(t*{}+{})", number(fps * f.speed), clip.source_in);
            if track.kind == "video" {
                let stream = asset.video_stream.ok_or("Video stream missing")?;
                // `cover` fills the canvas and crops the overflow. With
                // a face track for this clip, the crop window follows
                // the tracked face over time; otherwise it stays
                // centred.
                let fit = if cover {
                    face_cover_crop(
                        &settings.face_tracks,
                        &clip.id.to_string(),
                        clip.source_in,
                        fps,
                        w,
                        h,
                        canvas_w,
                        canvas_h,
                        &time,
                    )
                    .unwrap_or_else(|| format!("crop={w}:{h}:(iw-ow)/2:(ih-oh)/2"))
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
                    chain.push_str(&format!(",scale=w='max(2,trunc({w}*({scale})/2)*2)':h='max(2,trunc({h}*({scale})/2)*2)':eval=frame,format=yuva420p,geq=lum='lum(X,Y)':cb='cb(X,Y)':cr='cr(X,Y)':a='255*({opacity})'"));
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
                // Mixer gain for this audio track, in dB as stored by the
                // audio mixer. Clamped to the mixer range with the shared
                // helper and converted to a linear multiplier, so the
                // export contains what the mixer shows. A 0 dB gain
                // leaves the volume expression unchanged.
                let mixer_db = crate::scopes::audio_gain::clamp_gain_db(
                    settings
                        .track_gains
                        .get(&track.id.to_string())
                        .copied()
                        .unwrap_or(0.0),
                );
                let mixer_factor = if mixer_db == 0.0 {
                    String::new()
                } else {
                    format!(
                        "*{}",
                        number(crate::scopes::audio_gain::gain_db_to_linear(mixer_db))
                    )
                };
                chain.push_str(&format!(",volume='{}*({gain}){mixer_factor}':eval=frame,apad,atrim=duration={},asetpts=N/SR/TB+{}/TB,aresample=48000:first_pts=0[a{input}]",number(f.gain),number(length),number(start)));
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
    use crate::timeline::Clip;
    use uuid::Uuid;
    fn settings(aspect: &str, fill: &str, height: u32) -> ExportSettings {
        ExportSettings {
            container: "mp4".into(),
            height,
            fps: 30,
            encoder: "libx264".into(),
            aspect: aspect.into(),
            fill: fill.into(),
            use_proxy: false,
            track_gains: BTreeMap::new(),
            face_tracks: BTreeMap::new(),
        }
    }
    /// One-clip audio project over a real (placeholder) media file, so
    /// `compile` can canonicalize the source. Returns the project and
    /// the audio track id.
    fn audio_project(root: &std::path::Path) -> (Project, Uuid) {
        let path = root.join("take.wav");
        std::fs::write(&path, [0u8; 64]).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let asset = crate::media::Asset {
            id: Uuid::new_v4(),
            source_path: path.to_string_lossy().into_owned(),
            name: "take.wav".into(),
            size_bytes: meta.len(),
            modified_ns: meta
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .to_string(),
            kind: "audio".into(),
            duration: Some(2.0),
            width: None,
            height: None,
            fps: None,
            video_stream: None,
            audio_stream: Some(0),
            video_codec: None,
            audio_codec: Some("pcm".into()),
            sample_rate: Some(48000),
            channels: Some(2),
            created_at: 0,
            bin: String::new(),
        };
        let mut project = crate::create_project("Mixer gain".into(), 1920, 1080, 30).unwrap();
        project.schema_version = 6;
        let track_id = Uuid::new_v4();
        project.tracks.push(Track {
            id: track_id,
            name: "Audio 1".into(),
            kind: "audio".into(),
            muted: false,
            solo: false,
            locked: false,
            clips: vec![Clip {
                id: Uuid::new_v4(),
                asset_id: asset.id,
                link_id: None,
                start: 0,
                source_in: 0,
                duration: 30,
                caption: None,
                effects: None,
            }],
        });
        project.assets.push(asset);
        project.validate().unwrap();
        (project, track_id)
    }
    /// One-clip video project over a real (placeholder) media file.
    /// Returns the project and the asset id.
    fn video_project(root: &std::path::Path) -> (Project, Uuid) {
        let path = root.join("take.mp4");
        std::fs::write(&path, [0u8; 64]).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let asset = crate::media::Asset {
            id: Uuid::new_v4(),
            source_path: path.to_string_lossy().into_owned(),
            name: "take.mp4".into(),
            size_bytes: meta.len(),
            modified_ns: meta
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .to_string(),
            kind: "video".into(),
            duration: Some(2.0),
            width: Some(160),
            height: Some(90),
            fps: Some(30.0),
            video_stream: Some(0),
            audio_stream: None,
            video_codec: Some("h264".into()),
            audio_codec: None,
            sample_rate: None,
            channels: None,
            created_at: 0,
            bin: String::new(),
        };
        let mut project = crate::create_project("Proxy export".into(), 1920, 1080, 30).unwrap();
        project.schema_version = 6;
        project.tracks.push(Track {
            id: Uuid::new_v4(),
            name: "Video 1".into(),
            kind: "video".into(),
            muted: false,
            solo: false,
            locked: false,
            clips: vec![Clip {
                id: Uuid::new_v4(),
                asset_id: asset.id,
                link_id: None,
                start: 0,
                source_in: 0,
                duration: 30,
                caption: None,
                effects: None,
            }],
        });
        project.assets.push(asset.clone());
        project.validate().unwrap();
        (project, asset.id)
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
    #[test]
    fn old_export_payloads_without_proxy_or_gains_still_parse() {
        let parsed: ExportSettings =
            serde_json::from_str(r#"{"container":"mp4","height":1080,"fps":30,"encoder":"auto"}"#)
                .unwrap();
        assert!(!parsed.use_proxy);
        assert!(parsed.track_gains.is_empty());
        assert!(parsed.validate().is_ok());
        let parsed: ExportSettings = serde_json::from_str(
            r#"{"container":"mp4","height":1080,"fps":30,"encoder":"auto","useProxy":true,"trackGains":{"123e4567-e89b-42d3-a456-426614174000":6.0}}"#,
        )
        .unwrap();
        assert!(parsed.use_proxy);
        assert_eq!(
            parsed.track_gains["123e4567-e89b-42d3-a456-426614174000"],
            6.0
        );
        assert!(parsed.validate().is_ok());
    }
    #[test]
    fn mixer_gain_is_applied_to_export_audio_as_a_volume_factor() {
        let root = tempfile::tempdir().unwrap();
        let (project, track_id) = audio_project(root.path());
        // +6 dB is a linear factor of 10^(6/20) = 1.99526231.
        let mut config = settings("landscape", "fit", 1080);
        config.track_gains.insert(track_id.to_string(), 6.0);
        let graph = compile(&project, &config).unwrap().graph;
        assert!(
            graph.contains("volume='1.00000000*(1.00000000)*1.99526231'"),
            "{graph}"
        );
        // No stored gain leaves the volume expression unchanged.
        let plain = compile(&project, &settings("landscape", "fit", 1080))
            .unwrap()
            .graph;
        assert!(
            plain.contains("volume='1.00000000*(1.00000000)':eval=frame"),
            "{plain}"
        );
        // A gain stored for another track id is ignored.
        let mut other = settings("landscape", "fit", 1080);
        other.track_gains.insert(Uuid::new_v4().to_string(), 6.0);
        assert_eq!(compile(&project, &other).unwrap().graph, plain);
    }
    #[test]
    fn mixer_gain_is_clamped_to_the_shared_mixer_range() {
        let root = tempfile::tempdir().unwrap();
        let (project, track_id) = audio_project(root.path());
        // +99 dB clamps to +12 dB (10^0.6 = 3.98107171); -99 dB clamps
        // to -60 dB (10^-3 = 0.00100000). The clamp helper is the one
        // shared with the mixer in scopes::audio_gain.
        let mut config = settings("landscape", "fit", 1080);
        config.track_gains.insert(track_id.to_string(), 99.0);
        let graph = compile(&project, &config).unwrap().graph;
        assert!(graph.contains("*3.98107171'"), "{graph}");
        config.track_gains.insert(track_id.to_string(), -99.0);
        let graph = compile(&project, &config).unwrap().graph;
        assert!(graph.contains("*0.00100000'"), "{graph}");
        config.track_gains.insert(track_id.to_string(), f64::NAN);
        assert!(config.validate().is_err());
    }
    #[test]
    fn proxy_export_without_a_media_root_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let (project, _) = video_project(root.path());
        let mut config = settings("landscape", "fit", 1080);
        config.use_proxy = true;
        let error = compile(&project, &config).unwrap_err();
        assert!(error.contains("media cache"), "{error}");
    }
    #[test]
    fn proxy_export_falls_back_to_originals_without_a_ready_proxy() {
        let root = tempfile::tempdir().unwrap();
        let (project, _) = video_project(root.path());
        let mut config = settings("landscape", "fit", 1080);
        config.use_proxy = true;
        let compiled = compile_with_media_root(&project, &config, Some(root.path())).unwrap();
        assert_eq!(compiled.proxy_clips, 0);
        assert_eq!(compiled.total_clips, 1);
        let original = std::fs::canonicalize(root.path().join("take.mp4"))
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(compiled.inputs, vec!["-i".to_string(), original]);
    }
    #[test]
    fn proxy_export_uses_a_ready_proxy_and_counts_it() {
        let root = tempfile::tempdir().unwrap();
        let (project, asset_id) = video_project(root.path());
        let asset = project
            .assets
            .iter()
            .find(|a| a.id == asset_id)
            .unwrap()
            .clone();
        // Hand-craft the ready proxy state the generator would record:
        // a proxy file plus a sidecar whose fingerprint matches the
        // original.
        let proxy_path = crate::proxy::proxy_path_for(root.path(), asset_id);
        std::fs::create_dir_all(proxy_path.parent().unwrap()).unwrap();
        std::fs::write(&proxy_path, [0u8; 128]).unwrap();
        let record = crate::proxy::ProxyRecord {
            state: crate::proxy::ProxyState::Ready,
            width: Some(96),
            height: Some(54),
            size_bytes: Some(128),
            encoder: Some("libx264".into()),
            source_size_bytes: asset.size_bytes,
            source_modified_ns: asset.modified_ns.clone(),
            error: None,
            updated_at_ms: 1,
        };
        std::fs::write(
            crate::proxy::proxy_sidecar_path(root.path(), asset_id),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let mut config = settings("landscape", "fit", 1080);
        config.use_proxy = true;
        let compiled = compile_with_media_root(&project, &config, Some(root.path())).unwrap();
        assert_eq!(compiled.proxy_clips, 1);
        assert_eq!(compiled.total_clips, 1);
        assert_eq!(
            compiled.inputs,
            vec![
                "-i".to_string(),
                std::fs::canonicalize(&proxy_path)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            ]
        );
        // With the option off, the same project exports the original.
        config.use_proxy = false;
        let compiled = compile_with_media_root(&project, &config, Some(root.path())).unwrap();
        assert_eq!(compiled.proxy_clips, 0);
        assert_eq!(compiled.total_clips, 1);
    }
    #[test]
    fn face_track_cover_crop_follows_the_track() {
        let mut tracks = BTreeMap::new();
        tracks.insert(
            "clip-1".into(),
            vec![
                FaceTrackKeyframe {
                    t_seconds: 0.0,
                    focus_x: 0.2,
                    focus_y: 0.5,
                },
                FaceTrackKeyframe {
                    t_seconds: 1.0,
                    focus_x: 0.8,
                    focus_y: 0.5,
                },
            ],
        );
        let crop = face_cover_crop(
            &tracks, "clip-1", 0, 30.0, 608, 1080, 1920, 1080, "(t*30+0)",
        )
        .unwrap();
        // Time-varying crop: the window follows the face through the
        // keyframe curve (frames 0 and 30), clamped into the frame.
        assert!(crop.starts_with("crop=608:1080:x='min(max("), "{crop}");
        assert!(crop.contains("if(lt((t*30+0),30)"), "{crop}");
        assert!(crop.contains("iw-ow"), "{crop}");
        assert!(crop.contains("ih-oh"), "{crop}");
    }
    #[test]
    fn face_track_cover_crop_ignores_missing_or_full_frame_tracks() {
        let tracks: BTreeMap<String, Vec<FaceTrackKeyframe>> = BTreeMap::new();
        assert!(
            face_cover_crop(&tracks, "clip-1", 0, 30.0, 608, 1080, 1920, 1080, "(t*30+0)")
                .is_none()
        );
        let mut tracks = BTreeMap::new();
        tracks.insert(
            "clip-1".into(),
            vec![FaceTrackKeyframe {
                t_seconds: 0.0,
                focus_x: 0.2,
                focus_y: 0.5,
            }],
        );
        // The crop already fills the frame: no room for the window to move.
        assert!(
            face_cover_crop(&tracks, "clip-1", 0, 30.0, 1920, 1080, 1920, 1080, "(t*30+0)")
                .is_none()
        );
    }
    #[test]
    fn cover_export_with_face_track_emits_time_varying_crop() {
        let root = tempfile::tempdir().unwrap();
        let (project, _) = video_project(root.path());
        let clip_id = project.tracks[0].clips[0].id.to_string();
        let mut config = settings("vertical", "cover", 1080);
        config.face_tracks.insert(
            clip_id,
            vec![
                FaceTrackKeyframe {
                    t_seconds: 0.0,
                    focus_x: 0.2,
                    focus_y: 0.5,
                },
                FaceTrackKeyframe {
                    t_seconds: 1.0,
                    focus_x: 0.8,
                    focus_y: 0.5,
                },
            ],
        );
        let graph = compile(&project, &config).unwrap().graph;
        assert!(graph.contains("crop=608:1080:x='min(max("), "{graph}");
        assert!(graph.contains("if(lt("), "{graph}");
        assert!(
            !graph.contains("crop=608:1080:(iw-ow)/2:(ih-oh)/2"),
            "{graph}"
        );
        // Without the track, the same export stays centre-cropped.
        let plain = compile(&project, &settings("vertical", "cover", 1080))
            .unwrap()
            .graph;
        assert!(
            plain.contains("crop=608:1080:(iw-ow)/2:(ih-oh)/2"),
            "{plain}"
        );
    }
    #[test]
    fn face_track_settings_validate_and_old_payloads_still_parse() {
        let mut config = settings("vertical", "cover", 1080);
        config.face_tracks.insert(
            "clip-1".into(),
            vec![FaceTrackKeyframe {
                t_seconds: 0.0,
                focus_x: 1.5,
                focus_y: 0.5,
            }],
        );
        assert!(config.validate().is_err());
        let parsed: ExportSettings =
            serde_json::from_str(r#"{"container":"mp4","height":1080,"fps":30,"encoder":"auto"}"#)
                .unwrap();
        assert!(parsed.face_tracks.is_empty());
        assert!(parsed.validate().is_ok());
    }
}
