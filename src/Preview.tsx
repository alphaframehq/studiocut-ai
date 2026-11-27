import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { native, useProject } from './store';
import { getEncoderPreference } from './preferences';
import { audible, timelineEnd } from './timeline';
import { mediaUrl, TimelinePlayer } from './playback';
import { zoomAt } from './effects';
import { EffectPreview } from './EffectPreview';
import { PROXY_TOGGLED_EVENT, isProxyPreferred } from './ProxyToggle';

export function Preview() {
  const { project, playing, frame } = useProject();
  const video = useRef<HTMLVideoElement>(null);
  const image = useRef<HTMLImageElement>(null);
  const [url, setUrl] = useState('');
  const [sourceError, setSourceError] = useState('');
  const [attachmentVersion, setAttachmentVersion] = useState(0);
  const [proxyVersion, setProxyVersion] = useState(0);
  // Hardware-encoder status for preview proxy work (see media.rs
  // preview_encoder_status). The program monitor itself is a plain
  // <video> element decoded by the WebView, so there is nothing to
  // accelerate there; this note only covers the FFmpeg proxy encode.
  // Shown only when no hardware encoder is available. Probing is
  // advisory: preview works unchanged when it fails or is unavailable.
  type EncoderSelection = { encoder: string; hardware: boolean; note: string };
  const [encoderNote, setEncoderNote] = useState('');
  useEffect(() => {
    if (!native) return;
    let cancelled = false;
    void invoke<EncoderSelection>('preview_encoder_status', { preference: getEncoderPreference() })
      .then(selection => { if (!cancelled && !selection.hardware) setEncoderNote(selection.note); })
      .catch(() => { /* Encoder probing is advisory only; preview works without it. */ });
    return () => { cancelled = true; };
  }, []);
  useEffect(() => {
    const refresh = () => setAttachmentVersion(v => v + 1);
    window.addEventListener('studiocut-media-attached', refresh);
    return () => window.removeEventListener('studiocut-media-attached', refresh);
  }, []);
  useEffect(() => {
    const refresh = () => setProxyVersion(v => v + 1);
    window.addEventListener(PROXY_TOGGLED_EVENT, refresh);
    return () => window.removeEventListener(PROXY_TOGGLED_EVENT, refresh);
  }, []);
  const active = project?.tracks.filter(t => t.kind === 'video' && audible(t, project.tracks)).flatMap(t => t.clips.filter(c => c.start <= frame && c.start + c.duration > frame))[0];
  const asset = project?.assets.find(a => a.id === active?.assetId);
  // Whether an audible audio-track clip covers this asset at the
  // playhead: TimelinePlayer already plays that audio, so the video
  // element must stay muted then. Only when no audio-track clip covers
  // the asset is the element the sole audio path.
  const audioCovered = project?.tracks.some(t => t.kind === 'audio' && audible(t, project.tracks) && t.clips.some(c => c.assetId === asset?.id && c.start <= frame && c.start + c.duration > frame)) ?? false;
  const zoom = zoomAt(active?.effects?.zoom ?? [], frame - (active?.start ?? 0) + (active?.sourceIn ?? 0));
  const transform = { transform: `scale(${zoom.scale})`, transformOrigin: `${zoom.x * 100}% ${zoom.y * 100}%` };
  // CSS-filter approximation of the finishing grade (brightness/contrast/
  // saturation) on the media element only — the LUT canvas overlay is a
  // sibling and is not filtered. Approximation only: the exact grade is
  // applied by FFmpeg on export.
  const finishing = active?.effects?.finishing;
  const grade = finishing && (finishing.brightness !== 0 || finishing.contrast !== 1 || finishing.saturation !== 1)
    ? `brightness(${1 + finishing.brightness}) contrast(${finishing.contrast}) saturate(${finishing.saturation})` : '';
  const captions = project?.tracks.filter(t => t.kind === 'caption' && audible(t, project.tracks)).flatMap(t => t.clips).filter(c => frame >= c.start && frame < c.start + c.duration) ?? [];
  useEffect(() => {
    let cancelled = false;
    setUrl(''); setSourceError('');
    if (asset) {
      // When the user prefers the proxy for this asset, try it first and
      // fall back to the original media on any error. Export always uses
      // the original.
      const load = async () => {
        if (isProxyPreferred(asset.id)) {
          try {
            const proxyUrl = await invoke<string>('proxy_playback_source', { id: asset.id });
            if (!cancelled) { setUrl(proxyUrl); return; }
          } catch { /* fall through to the original media */ }
        }
        void mediaUrl(asset).then(url => { if (!cancelled) setUrl(url); }).catch(error => { if (!cancelled) { setSourceError(String(error)); useProject.setState({ playing: false }); } });
      };
      void load();
    }
    return () => { cancelled = true; };
  }, [asset, active?.id, attachmentVersion, proxyVersion]);
  useEffect(() => {
    if (!project || !playing || !timelineEnd(project.tracks)) return;
    const player = new TimelinePlayer(project,
      frame => useProject.setState({ frame }),
      error => useProject.setState({ playing: false, ...(error ? { error } : {}) }));
    void player.play(useProject.getState().frame).catch(error => useProject.setState({ playing: false, error: String(error) }));
    return () => player.dispose();
  }, [project, playing]);
  useEffect(() => {
    const element = video.current;
    if (!element || !active || !project || !url) return;
    element.muted = audioCovered;
    const time = (active.sourceIn + frame - active.start) / project.settings.fps;
    const synchronize = () => {
      if (Math.abs(element.currentTime - time) > (playing ? 0.08 : 0.001)) element.currentTime = time;
      if (playing && element.paused) void element.play().catch(error => useProject.setState({ playing: false, error: `Video playback failed: ${error}` }));
      if (!playing) element.pause();
    };
    if (element.readyState) synchronize();
    else element.addEventListener('loadedmetadata', synchronize, { once: true });
    return () => element.removeEventListener('loadedmetadata', synchronize);
  }, [frame, playing, active, project, url, audioCovered]);
  return <section className="preview-player" aria-label="Program preview"><div className="preview-heading"><span>PROGRAM MONITOR</span><span>{project?.settings.width} × {project?.settings.height}</span></div><div className="preview-screen">
    {sourceError ? <p role="alert">{sourceError}</p> : asset && url ? <div className="effect-stage" style={transform}>{asset.kind === 'image' ? <img ref={image} src={url} alt={asset.name} style={grade ? { filter: grade } : undefined} /> : <video ref={video} src={url} playsInline preload="auto" style={grade ? { filter: grade } : undefined} onError={() => { setSourceError('This video codec is not supported by this WebView. Use an H.264 MP4 or WebM source.'); useProject.setState({ playing: false }); }} />}{active?.effects?.lut && <EffectPreview key={`${url}:${active.effects.lut}`} source={asset.kind === 'image' ? image : video} image={asset.kind === 'image'} preset={active.effects.lut} frame={frame} />}</div> : <div className="preview-empty"><span>{asset ? 'Loading source…' : 'StudioCut'}</span><p>{project && timelineEnd(project.tracks) ? 'No visible video at the playhead' : 'Add a clip to begin your edit'}</p></div>}
    {captions.map(c => <div key={c.id} className={`caption-overlay caption-${c.caption!.preset}`} aria-label="Captions">{c.caption!.words.filter(w => w.end > c.sourceIn && w.start < c.sourceIn + c.duration).map((word, i) => <span key={i} className={frame - c.start + c.sourceIn >= word.start && frame - c.start + c.sourceIn < word.end ? 'active-word' : ''}>{word.text}</span>)}</div>)}
  </div><div className="preview-footer"><span>{active ? asset?.name : 'Black / silence through timeline gaps'}</span>{grade && <span>Grade preview is approximate — exact grade applies on export</span>}{encoderNote && <span>{encoderNote}</span>}<span>{playing ? 'Playing · audio clock' : 'Paused'}</span></div></section>;
}
