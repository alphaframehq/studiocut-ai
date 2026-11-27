import { useEffect, useRef, useState } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { useProject } from './store';
import { getEncoderPreference, getExportUseProxy, getStyleDna, setExportUseProxy } from './preferences';
import { mixerGainsForProject } from './AudioMixer';
import './finishing.css';
type Job = { version: 1; id: string; sequence: number; timestamp: number; state: string; progress: number; encoder: string | null; disclosure: string; output: string; error: string | null };
const active = (job: Job | null) => !!job && ['probing', 'rendering', 'validating'].includes(job.state);
// Mirror of the Rust width() rule in filtergraph.rs (even-rounded 9:16).
const exportWidth = (aspect: string, height: number) => aspect === 'vertical' ? (Math.round(height * 9 / 16) % 2 === 1 ? Math.round(height * 9 / 16) + 1 : Math.round(height * 9 / 16)) : aspect === 'square' ? height : height * 16 / 9;
const presets = [
  { id: 'youtube', label: 'YouTube · 16:9', aspect: 'landscape', height: 1080, fill: 'fit' },
  { id: 'shorts', label: 'Shorts · 9:16', aspect: 'vertical', height: 1080, fill: 'cover' },
  { id: 'reels', label: 'Reels · 9:16', aspect: 'vertical', height: 1080, fill: 'cover' },
] as const;
export function ExportModal({ hidden = false }: { hidden?: boolean }) {
  const { project, busy, faceTracks } = useProject();
  const dialog = useRef<HTMLDialogElement>(null);
  const progressCursor = useRef<Job | null>(null);
  const [open, setOpen] = useState(false), [pending, setPending] = useState(false), [job, setJob] = useState<Job | null>(null), [error, setError] = useState('');
  const [container, setContainer] = useState('mp4'), [height, setHeight] = useState(1080), [fps, setFps] = useState(30), [encoder, setEncoder] = useState<string>(getEncoderPreference), [output, setOutput] = useState('');
  const [aspect, setAspect] = useState('landscape'), [fill, setFill] = useState('fit'), [preset, setPreset] = useState<string>('youtube');
  const [useProxy, setUseProxy] = useState(getExportUseProxy);
  // Face tracks for cover export: clips in this project with a saved
  // face-tracked plan (Analysis §07) whose target matches the export
  // aspect. Only built for cover exports; fit exports and clips
  // without a plan stay centred.
  const exportFaceTracks: Record<string, { tSeconds: number; focusX: number; focusY: number }[]> = (() => {
    if (fill !== 'cover' || !project) return {};
    const wanted = aspect === 'vertical' ? '9:16' : aspect === 'square' ? '1:1' : null;
    if (!wanted) return {};
    const tracks: Record<string, { tSeconds: number; focusX: number; focusY: number }[]> = {};
    for (const track of project.tracks) for (const clip of track.clips) {
      const plan = faceTracks[clip.id];
      if (plan && plan.method === 'face-tracked' && plan.target === wanted && plan.keyframes.length) {
        tracks[clip.id] = plan.keyframes.map(k => ({ tSeconds: k.tSeconds, focusX: k.focusX, focusY: k.focusY }));
      }
    }
    return tracks;
  })();
  const faceTrackCount = Object.keys(exportFaceTracks).length;
  // Re-read the Settings preferences every time the dialog opens, so a
  // change made in Settings while this dialog was closed still applies —
  // including a saved Style DNA (aspect/fill).
  useEffect(() => { if (open) { setEncoder(getEncoderPreference()); setUseProxy(getExportUseProxy()); const dna = getStyleDna(); setAspect(dna.aspect); setFill(dna.fill); setPreset(dna.aspect === 'vertical' ? 'shorts' : dna.aspect === 'square' ? 'custom' : 'youtube'); } }, [open]);
  useEffect(() => {
    if (!open || !isTauri()) return;
    let stopped = false, timer: ReturnType<typeof setTimeout>, failures = 0;
    let last = progressCursor.current;
    const poll = async () => {
      try {
        const batch = await invoke<{ version: number; messageType: string; reset: boolean; events: Job[] } | null>('export_progress', { id: last?.id ?? null, after: last?.sequence ?? null });
        if (batch && (batch.version !== 1 || batch.messageType !== 'export.progress')) throw new Error('Unsupported export progress protocol.');
        if (!stopped && batch) {
          for (const next of batch.events) {
            if (next.version !== 1) throw new Error('Unsupported export progress protocol.');
            if (!last || batch.reset || (next.id === last.id && next.sequence > last.sequence)) { setJob(next); last = next; progressCursor.current = next; }
          }
          setError('');
        }
        failures = 0;
      } catch (e) { if (!stopped) setError(`Progress disconnected; retrying: ${String(e)}`); failures++; }
      if (!stopped) timer = setTimeout(poll, Math.min(10000, 250 * 2 ** Math.min(failures, 6)));
    };
    void poll(); return () => { stopped = true; clearTimeout(timer); };
  }, [open]);
  const close = () => { dialog.current?.close(); setOpen(false); };
  return <>{!hidden && <button className="primary" disabled={!project || busy} onClick={() => { setOpen(true); dialog.current?.showModal(); }}>Export</button>}
    <dialog ref={dialog} className="export-dialog" onCancel={() => setOpen(false)} onClose={() => setOpen(false)} aria-labelledby="export-title">
      <header><div><span className="finish-kicker">LOCAL RENDER ENGINE</span><h2 id="export-title">Export your cut</h2></div><button aria-label="Close export" onClick={close}>Close</button></header>
      <p>Originals stay untouched. The finished file appears only after FFprobe validation. Existing files are never overwritten.</p>
      {!isTauri() && <p className="finish-warning" role="status">Export requires the desktop app and installed FFmpeg / FFprobe. Browser preview cannot render local media.</p>}
      <form onSubmit={async e => {
        e.preventDefault(); if (!project || !isTauri()) return; setPending(true); setError('');
        try { setJob(await invoke<Job>('export_start', { projectId: project.id, revision: project.revision, output, settings: { container, height, fps, encoder, aspect, fill, useProxy, trackGains: mixerGainsForProject(project.id, project.tracks.filter(t => t.kind === 'audio').map(t => t.id)), faceTracks: exportFaceTracks } })); }
        catch (e) { setError(String(e)); } finally { setPending(false); }
      }}>
        <fieldset disabled={active(job) || pending}><legend>Output settings</legend>
          <div className="finish-grid" role="group" aria-label="Platform preset">
            {presets.map(p => <button key={p.id} type="button" className={`button secondary${preset === p.id ? ' preset-on' : ''}`} aria-pressed={preset === p.id} onClick={() => { setPreset(p.id); setAspect(p.aspect); setHeight(p.height); setFill(p.fill); }}>{p.label}</button>)}
          </div><div className="finish-grid">
          <label>Aspect<select value={aspect} onChange={e => { setAspect(e.target.value); setPreset('custom'); }}><option value="landscape">16:9 · Landscape</option><option value="vertical">9:16 · Vertical</option><option value="square">1:1 · Square</option></select></label>
          <label>Frame fill<select value={fill} onChange={e => { setFill(e.target.value); setPreset('custom'); }}><option value="fit">Fit · bars added</option><option value="cover">Cover · fills frame, crops edges</option></select></label>
          <label>Container<select value={container} onChange={e => setContainer(e.target.value)}><option value="mp4">MP4</option><option value="mov">MOV</option></select></label>
          <label>Resolution<select value={height} onChange={e => setHeight(Number(e.target.value))}><option value={1080}>{exportWidth(aspect, 1080)} × 1080</option><option value={2160}>{exportWidth(aspect, 2160)} × 2160 · 4K</option></select></label>
          <label>Frame rate<select value={fps} onChange={e => setFps(Number(e.target.value))}>{[24, 30, 60].map(n => <option key={n} value={n}>{n} fps</option>)}</select></label>
        </div><label>Encoder<select value={encoder} onChange={e => setEncoder(e.target.value)}><option value="auto">Auto · probe hardware, then CPU</option><option value="libx264">libx264 · CPU</option><option value="h264_nvenc">NVIDIA · H.264</option><option value="hevc_videotoolbox">Apple VideoToolbox · HEVC</option><option value="h264_amf">AMD AMF · H.264</option><option value="h264_qsv">Intel Quick Sync · H.264</option></select></label>
          <small>Each hardware candidate runs a real encode probe. If unavailable, libx264 CPU fallback is disclosed below. {fill === 'cover' ? `Cover fills the frame edge to edge; the sides or top and bottom are cropped. ${faceTrackCount > 0 ? `The crop follows the tracked face in ${faceTrackCount} clip${faceTrackCount === 1 ? '' : 's'} with a saved face-tracked plan; other clips stay centred.` : 'Plan a face-tracked reframe in Analysis §07 to make the crop follow a face; otherwise it stays centred.'}` : 'Fit keeps the whole frame; bars fill the unused space.'}</small>
          <label style={{ display: 'flex', alignItems: 'flex-start', gap: 8, fontSize: 12 }}>
            <input type="checkbox" checked={useProxy} onChange={e => { const next = e.target.checked; setUseProxy(next); setExportUseProxy(next); }} style={{ marginTop: 2 }} />
            <span>Render from 540p proxy files when available</span>
          </label>
          {useProxy && <p className="finish-warning" role="status">Proxy export is lower resolution (540p). Clips without a ready proxy use the original media. The export summary states how many clips used proxies. Original files are never modified.</p>}
          <label>Absolute output path<input required value={output} onChange={e => setOutput(e.target.value)} placeholder={`Full path to a new .${container} file`} /></label>
          <button className="primary" type="submit" disabled={!isTauri() || !project?.tracks.some(t => t.clips.length)}>Start export</button>
        </fieldset>
      </form>
      {job && <section className="export-progress" aria-live="polite"><strong>{job.state} · {Math.round(job.progress * 100)}%</strong><progress aria-label="Export progress" max={1} value={job.progress} /><p>{job.disclosure}</p><small>{job.output}</small>{job.error && <p role="alert">{job.error}</p>}{active(job) && <button onClick={async () => { try { await invoke('export_cancel', { id: job.id }); } catch (e) { setError(String(e)); } }}>Cancel export</button>}</section>}
      {error && <p role="alert" className="error">{error}</p>}
      <footer>Closing this window leaves the desktop export running. Reopen it to resume progress.</footer>
    </dialog>
  </>;
}
