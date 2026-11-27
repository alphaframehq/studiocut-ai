import { useEffect, useRef, useState } from 'react';
import { Pause, Play, Scissors, Undo2, Redo2, Plus, Trash2 } from 'lucide-react';
import { useProject } from './store';
import { newId, timelineEnd, type Clip, type Track } from './timeline';

export function Timeline() {
  const { project, frame, selected, playing, busy, history, edit, undo, seek, addMarker, removeMarker, applyAnalysis, replaceTracks } = useProject();
  const [razor, setRazor] = useState(false);
  const [zoom, setZoom] = useState(36);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<{ id: string; x: number; original: number; current: number; edge?: 'in' | 'out' } | null>(null);
  const dragRef = useRef(drag);
  dragRef.current = drag;
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if ((event.target as HTMLElement).closest('input,select,textarea,dialog')) return;
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'z') { event.preventDefault(); void undo(event.shiftKey); }
      if (event.code === 'Space' && project && timelineEnd(project.tracks)) { event.preventDefault(); useProject.setState({ playing: !useProject.getState().playing }); }
      if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') { event.preventDefault(); seek(frame + (event.key === 'ArrowLeft' ? -1 : 1)); }
      if (selected && event.key.toLowerCase() === 's') void edit({ type: 'split', clipId: selected, frame: Math.round(frame) });
      if (event.key.toLowerCase() === 'm' && project) { event.preventDefault(); void addMarker(frame); }
      if (selected && event.key === 'Delete') void edit({ type: 'remove', clipId: selected });
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  }, [project, frame, selected, undo, edit, seek]);
  if (!project) return null;
  const fps = project.settings.fps;
  const end = timelineEnd(project.tracks);
  const width = Math.max(700, (end / fps + 5) * zoom);
  const clip = project.tracks.flatMap(t => t.clips).find(c => c.id === selected);
  const isLocked = (c: Clip) => project.tracks.some(t => t.locked && t.clips.some(other => other.id === c.id || (c.linkId && other.linkId === c.linkId)));
  function begin(event: React.PointerEvent, c: Clip, edge?: 'in' | 'out') {
    if (busy || isLocked(c)) return;
    event.stopPropagation();
    useProject.setState({ selected: c.id, playing: false });
    if (razor && !edge) {
      const rect = event.currentTarget.getBoundingClientRect();
      void edit({ type: 'split', clipId: c.id, frame: c.start + Math.round((event.clientX - rect.left) / zoom * fps) });
      return;
    }
    event.currentTarget.setPointerCapture(event.pointerId);
    const original = edge === 'out' ? c.start + c.duration : c.start;
    setDrag({ id: c.id, x: event.clientX, original, current: original, edge });
  }
  function move(event: React.PointerEvent) {
    const d = dragRef.current;
    if (!d || !project) return;
    let current = d.original + Math.round((event.clientX - d.x) / zoom * fps);
    // Snap-to-edge: clip edges magnetise to other clips, the playhead and 0.
    const clip = project.tracks.flatMap(t => t.clips).find(c => c.id === d.id);
    const points = new Set<number>([0, frame]);
    for (const t of project.tracks) for (const c of t.clips) {
      if (clip && (c.id === clip.id || (clip.linkId !== null && c.linkId === clip.linkId))) continue;
      points.add(c.start);
      points.add(c.start + c.duration);
    }
    const threshold = Math.max(2, Math.round(8 / zoom * fps));
    const snap = (value: number) => {
      for (const p of points) if (Math.abs(value - p) <= threshold) return p;
      return value;
    };
    if (d.edge) current = snap(current);
    else if (clip) {
      const snappedStart = snap(current);
      current = snappedStart !== current ? snappedStart : snap(current + clip.duration) - clip.duration;
    }
    setDrag({ ...d, current });
  }
  function zoomToFit() {
    const el = scrollRef.current;
    if (!el || !project) return;
    const seconds = end / fps + 5;
    const available = Math.max(200, el.clientWidth - 150);
    setZoom(Math.min(120, Math.max(4, Math.floor(available / seconds))));
  }
  function finish() {
    const d = dragRef.current;
    if (!d) return;
    setDrag(null);
    if (d.current === d.original) return;
    void edit(d.edge ? { type: 'trim', clipId: d.id, edge: d.edge, frame: d.current } : { type: 'move', clipId: d.id, start: d.current });
  }
  function row(track: Track) {
    if (!project) return null;
    return <div className={`track-row ${track.locked ? 'track-locked' : ''}`} key={track.id}>
      <div className="track-head"><strong>{track.name}</strong><span>{track.kind}</span><div>
        {(['muted', 'solo', 'locked'] as const).map(flag => <button key={flag} disabled={busy || (track.locked && flag !== 'locked')} aria-label={`${flag === 'muted' ? 'Mute' : flag === 'solo' ? 'Solo' : 'Lock'} ${track.name}`} aria-pressed={track[flag]} onClick={() => void edit({ type: 'toggle', trackId: track.id, flag })}>{flag === 'muted' ? 'M' : flag === 'solo' ? 'S' : 'L'}</button>)}
      </div></div>
      <div className="track-lane" style={{ width }} onClick={event => { if (event.target === event.currentTarget) seek((event.clientX - event.currentTarget.getBoundingClientRect().left) / zoom * fps); }} onDragOver={event => event.preventDefault()} onDrop={event => { event.preventDefault(); const assetId = event.dataTransfer.getData('application/studiocut-asset'); if (assetId) void edit({ type: 'addClip', assetId, trackId: track.id, start: Math.max(0, Math.round((event.clientX - event.currentTarget.getBoundingClientRect().left) / zoom * fps)) }); }}>
        {track.clips.map(c => {
          const asset = project.assets.find(a => a.id === c.assetId)!;
          const d = drag?.id === c.id ? drag : null;
          const start = d && d.edge !== 'out' ? d.current : c.start;
          const duration = d?.edge === 'in' ? c.duration + c.start - d.current : d?.edge === 'out' ? d.current - c.start : c.duration;
          return <div key={c.id} className={`timeline-clip ${track.kind} ${selected === c.id ? 'selected' : ''}`} style={{ left: start / fps * zoom, width: Math.max(4, duration / fps * zoom) }} onPointerDown={event => begin(event, c)} onPointerMove={move} onPointerUp={finish} onPointerCancel={() => setDrag(null)}>
            <button className="trim-handle in" aria-label={`Trim start ${asset.name} ${track.name}`} disabled={isLocked(c)} onPointerDown={event => begin(event, c, 'in')} onKeyDown={event => { if (event.key.startsWith('Arrow')) { event.preventDefault(); event.stopPropagation(); void edit({ type: 'trim', clipId: c.id, edge: 'in', frame: c.start + (event.key === 'ArrowLeft' ? -1 : 1) }); } }} />
            <button className="clip-label" aria-label={`Select ${asset.name} on ${track.name} at frame ${c.start}`} onClick={() => useProject.setState({ selected: c.id })}>{c.linkId ? '↔ ' : ''}{c.caption ? c.caption.words.filter(w => w.end > c.sourceIn && w.start < c.sourceIn + c.duration).map(w => w.text).join(' ') : asset.name}<small>{c.duration} frames</small></button>
            <button className="trim-handle out" aria-label={`Trim end ${asset.name} ${track.name}`} disabled={isLocked(c)} onPointerDown={event => begin(event, c, 'out')} onKeyDown={event => { if (event.key.startsWith('Arrow')) { event.preventDefault(); event.stopPropagation(); void edit({ type: 'trim', clipId: c.id, edge: 'out', frame: c.start + c.duration + (event.key === 'ArrowLeft' ? -1 : 1) }); } }} />
          </div>;
        })}
        <div className="playhead-line" style={{ left: frame / fps * zoom }} />
      </div>
    </div>;
  }
  return <section className="editing-timeline" aria-label="Timeline editor">
    <div className="timeline-toolbar"><strong>Timeline <span>· {project.tracks.length} tracks</span></strong>
      <button aria-label="Undo" disabled={busy || !history.past.length} onClick={() => void undo()}><Undo2 size={15} /></button>
      <button aria-label="Redo" disabled={busy || !history.future.length} onClick={() => void undo(true)}><Redo2 size={15} /></button>
      <button aria-label="Razor tool" aria-pressed={razor} onClick={() => setRazor(!razor)}><Scissors size={15} /> Razor</button>
      <button disabled={!clip || busy || (clip && isLocked(clip))} onClick={() => selected && void edit({ type: 'split', clipId: selected, frame: Math.round(frame) })}>Split at playhead</button>
      <button aria-label="Delete selected clip" disabled={!clip || busy || (clip && isLocked(clip))} onClick={() => selected && void edit({ type: 'remove', clipId: selected })}><Trash2 size={15} /></button>
      <button disabled={!clip || busy || (clip && isLocked(clip))} title="Delete the selected clip and ripple-close the gap across all tracks (one undo)" onClick={() => clip && void applyAnalysis({ type: 'cuts', cuts: [{ start: clip.start, end: clip.start + clip.duration }] }, project.id, project.revision)}>Ripple-cut selection</button>
      <button disabled={busy} title="Append an empty caption track (project v6)" onClick={() => void replaceTracks([...project.tracks, { id: newId(), name: 'Captions', kind: 'caption', muted: false, solo: false, locked: false, clips: [] }], project.id, project.revision)}><Plus size={14} /> Caption track</button>
      <button disabled={busy || !end} onClick={zoomToFit}>Zoom to fit</button>
      <button disabled={busy} onClick={() => void addMarker(frame)}>+ Marker</button>
      <button disabled={busy} onClick={() => void edit({ type: 'addTrack', kind: 'video' })}><Plus size={14} /> Video track</button>
      <button disabled={busy} onClick={() => void edit({ type: 'addTrack', kind: 'audio' })}><Plus size={14} /> Audio track</button>
      <label>Zoom<input aria-label="Timeline zoom" type="range" min="4" max="120" value={zoom} onChange={e => setZoom(Number(e.target.value))} /></label>
    </div>
    <div className="transport"><button aria-label={playing ? 'Pause preview' : 'Play preview'} disabled={!end || busy} onClick={() => { if (frame >= end) seek(0); useProject.setState({ playing: !playing }); }}>{playing ? <Pause size={16} /> : <Play size={16} />}</button><output aria-label="Playhead time">{(frame / fps).toFixed(2)}s / {(end / fps).toFixed(2)}s</output><label>Playhead frame<input aria-label="Playhead frame" type="number" min="0" max={end} value={Math.round(frame)} onChange={e => seek(Math.min(end, Number(e.target.value)))} /></label><span>Audio-master clock · {fps} fps</span></div>
    <div className="marker-row" aria-label="Timeline markers"><span>MARKERS</span>{project.markers.map(m => <span key={m.id} className="marker-chip"><button onClick={() => seek(m.frame)} title={`Jump to ${m.label}`}>{m.label} · {(m.frame / fps).toFixed(2)}s</button><button aria-label={`Delete ${m.label}`} onClick={() => void removeMarker(m.id)}>×</button></span>)}{!project.markers.length && <span>No markers yet — press M at the playhead, or use + Marker.</span>}</div>
    <div className="timeline-scroll" ref={scrollRef}><div className="timeline-canvas" style={{ width: width + 150 }}>
      <div className="timeline-ruler"><div className="ruler-label">TRACKS</div><div className="ruler" style={{ width }} onClick={event => seek(Math.min(end, (event.clientX - event.currentTarget.getBoundingClientRect().left) / zoom * fps))}>{Array.from({ length: Math.ceil(width / (zoom * 5)) }, (_, i) => <span key={i} style={{ left: i * zoom * 5 }}>{i * 5}s</span>)}{project.markers.map(m => <span key={m.id} className="marker-tick" style={{ left: m.frame / fps * zoom }} title={m.label} />)}</div></div>
      {project.tracks.map(row)}
      {!project.tracks.length && <p className="timeline-empty">Add media from the library to create linked video and audio tracks.</p>}
    </div></div>
    {clip && <div className="clip-inspector" key={`${clip.id}-${project.revision}`}><strong>Selected clip {clip.linkId ? '· linked A/V' : ''}</strong><label>Start frame<input aria-label="Clip start frame" type="number" defaultValue={clip.start} disabled={busy || isLocked(clip)} onBlur={e => { const start = Number(e.target.value); if (start !== clip.start) void edit({ type: 'move', clipId: clip.id, start }); }} /></label><label>Trim in frame<input aria-label="Clip trim in frame" type="number" defaultValue={clip.start} disabled={busy || isLocked(clip)} onBlur={e => { const frame = Number(e.target.value); if (frame !== clip.start) void edit({ type: 'trim', clipId: clip.id, edge: 'in', frame }); }} /></label><label>Trim out frame<input aria-label="Clip trim out frame" type="number" defaultValue={clip.start + clip.duration} disabled={busy || isLocked(clip)} onBlur={e => { const frame = Number(e.target.value); if (frame !== clip.start + clip.duration) void edit({ type: 'trim', clipId: clip.id, edge: 'out', frame }); }} /></label><span>Source in: {clip.sourceIn} · Duration: {clip.duration} frames</span></div>}
    <p className="timeline-help">Drag clips to move · Drag edges to trim · S to split · M to add a marker · Space to play · Ctrl/⌘ Z to undo · 50 edits per session</p>
  </section>;
}
