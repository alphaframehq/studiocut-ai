import { useEffect, useRef, useState } from 'react';
import { useProject } from './store';
import { audible, timelineEnd } from './timeline';
import { mediaUrl, TimelinePlayer } from './playback';
import { zoomAt } from './effects';
import { EffectPreview } from './EffectPreview';

export function Preview() {
  const { project, playing, frame } = useProject();
  const video = useRef<HTMLVideoElement>(null);
  const image = useRef<HTMLImageElement>(null);
  const [url, setUrl] = useState('');
  const [sourceError, setSourceError] = useState('');
  const [attachmentVersion, setAttachmentVersion] = useState(0);
  useEffect(() => {
    const refresh = () => setAttachmentVersion(v => v + 1);
    window.addEventListener('studiocut-media-attached', refresh);
    return () => window.removeEventListener('studiocut-media-attached', refresh);
  }, []);
  const active = project?.tracks.filter(t => t.kind === 'video' && audible(t, project.tracks)).flatMap(t => t.clips.filter(c => c.start <= frame && c.start + c.duration > frame))[0];
  const asset = project?.assets.find(a => a.id === active?.assetId);
  const zoom = zoomAt(active?.effects?.zoom ?? [], frame - (active?.start ?? 0) + (active?.sourceIn ?? 0));
  const transform = { transform: `scale(${zoom.scale})`, transformOrigin: `${zoom.x * 100}% ${zoom.y * 100}%` };
  const captions = project?.tracks.filter(t => t.kind === 'caption' && audible(t, project.tracks)).flatMap(t => t.clips).filter(c => frame >= c.start && frame < c.start + c.duration) ?? [];
  useEffect(() => {
    let cancelled = false;
    setUrl(''); setSourceError('');
    if (asset) void mediaUrl(asset).then(url => { if (!cancelled) setUrl(url); }).catch(error => { if (!cancelled) { setSourceError(String(error)); useProject.setState({ playing: false }); } });
    return () => { cancelled = true; };
  }, [asset, active?.id, attachmentVersion]);
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
    const time = (active.sourceIn + frame - active.start) / project.settings.fps;
    const synchronize = () => {
      if (Math.abs(element.currentTime - time) > (playing ? 0.08 : 0.001)) element.currentTime = time;
      if (playing && element.paused) void element.play().catch(error => useProject.setState({ playing: false, error: `Video playback failed: ${error}` }));
      if (!playing) element.pause();
    };
    if (element.readyState) synchronize();
    else element.addEventListener('loadedmetadata', synchronize, { once: true });
    return () => element.removeEventListener('loadedmetadata', synchronize);
  }, [frame, playing, active, project, url]);
  return <section className="preview-player" aria-label="Program preview"><div className="preview-heading"><span>PROGRAM MONITOR</span><span>{project?.settings.width} × {project?.settings.height}</span></div><div className="preview-screen">
    {sourceError ? <p role="alert">{sourceError}</p> : asset && url ? <div className="effect-stage" style={transform}>{asset.kind === 'image' ? <img ref={image} src={url} alt={asset.name} /> : <video ref={video} src={url} muted playsInline preload="auto" onError={() => { setSourceError('This video codec is not supported by this WebView. Use an H.264 MP4 or WebM source.'); useProject.setState({ playing: false }); }} />}{active?.effects?.lut && <EffectPreview key={`${url}:${active.effects.lut}`} source={asset.kind === 'image' ? image : video} image={asset.kind === 'image'} preset={active.effects.lut} frame={frame} />}</div> : <div className="preview-empty"><span>{asset ? 'Loading source…' : 'StudioCut'}</span><p>{project && timelineEnd(project.tracks) ? 'No visible video at the playhead' : 'Add a clip to begin your edit'}</p></div>}
    {captions.map(c => <div key={c.id} className={`caption-overlay caption-${c.caption!.preset}`} aria-label="Captions">{c.caption!.words.filter(w => w.end > c.sourceIn && w.start < c.sourceIn + c.duration).map((word, i) => <span key={i} className={frame - c.start + c.sourceIn >= word.start && frame - c.start + c.sourceIn < word.end ? 'active-word' : ''}>{word.text}</span>)}</div>)}
  </div><div className="preview-footer"><span>{active ? asset?.name : 'Black / silence through timeline gaps'}</span><span>{playing ? 'Playing · audio clock' : 'Paused'}</span></div></section>;
}
