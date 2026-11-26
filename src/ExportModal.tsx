import { useEffect, useRef, useState } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { useProject } from './store';
import { getEncoderPreference } from './preferences';
import './finishing.css';
type Job = { version: 1; id: string; sequence: number; timestamp: number; state: string; progress: number; encoder: string | null; disclosure: string; output: string; error: string | null };
const active = (job: Job | null) => !!job && ['probing', 'rendering', 'validating'].includes(job.state);
export function ExportModal({ hidden = false }: { hidden?: boolean }) {
  const { project, busy } = useProject();
  const dialog = useRef<HTMLDialogElement>(null);
  const progressCursor = useRef<Job | null>(null);
  const [open, setOpen] = useState(false), [pending, setPending] = useState(false), [job, setJob] = useState<Job | null>(null), [error, setError] = useState('');
  const [container, setContainer] = useState('mp4'), [height, setHeight] = useState(1080), [fps, setFps] = useState(30), [encoder, setEncoder] = useState<string>(getEncoderPreference), [output, setOutput] = useState('');
  // Re-read the Settings preference every time the dialog opens, so a
  // change made in Settings while this dialog was closed still applies.
  useEffect(() => { if (open) setEncoder(getEncoderPreference()); }, [open]);
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
        try { setJob(await invoke<Job>('export_start', { projectId: project.id, revision: project.revision, output, settings: { container, height, fps, encoder } })); }
        catch (e) { setError(String(e)); } finally { setPending(false); }
      }}>
        <fieldset disabled={active(job) || pending}><legend>Output settings</legend><div className="finish-grid">
          <label>Container<select value={container} onChange={e => setContainer(e.target.value)}><option value="mp4">MP4</option><option value="mov">MOV</option></select></label>
          <label>Resolution<select value={height} onChange={e => setHeight(Number(e.target.value))}><option value={1080}>1920 × 1080</option><option value={2160}>3840 × 2160 · 4K</option></select></label>
          <label>Frame rate<select value={fps} onChange={e => setFps(Number(e.target.value))}>{[24, 30, 60].map(n => <option key={n} value={n}>{n} fps</option>)}</select></label>
        </div><label>Encoder<select value={encoder} onChange={e => setEncoder(e.target.value)}><option value="auto">Auto · probe hardware, then CPU</option><option value="libx264">libx264 · CPU</option><option value="h264_nvenc">NVIDIA · H.264</option><option value="hevc_videotoolbox">Apple VideoToolbox · HEVC</option><option value="h264_amf">AMD AMF · H.264</option><option value="h264_qsv">Intel Quick Sync · H.264</option></select></label>
          <small>Each hardware candidate runs a real encode probe. If unavailable, libx264 CPU fallback is disclosed below. Portrait/square canvases are letterboxed into 16:9.</small>
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
