import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useProject } from './store';
import { audible } from './timeline';

type ScopeData = {
  sampleWidth: number; sampleHeight: number; approximate: boolean; fullResolution: boolean;
  waveform: [number, number][]; histogramLuma: number[];
  histogramRed: number[]; histogramGreen: number[]; histogramBlue: number[];
  vectorscope: number[]; vectorscopePeak: number;
};

const APPROXIMATION_NOTE = 'Approximate — computed from a sample frame no wider than 320 px. Use as a grading aid, not a broadcast measurement.';
const FULL_RESOLUTION_NOTE = 'Full-resolution sampling active — the native frame is analysed, with no downscaling. Slower on large sources. Still a grading aid, not a broadcast measurement.';

function drawWaveform(canvas: HTMLCanvasElement, scopes: ScopeData) {
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  const { width, height } = canvas;
  ctx.clearRect(0, 0, width, height);
  ctx.fillStyle = '#101814';
  ctx.fillRect(0, 0, width, height);
  const columns = scopes.waveform.length;
  const barWidth = width / columns;
  ctx.fillStyle = '#b2d789';
  for (let i = 0; i < columns; i++) {
    const [min, max] = scopes.waveform[i];
    const top = height - (max / 255) * height;
    const bottom = height - (min / 255) * height;
    ctx.fillRect(i * barWidth, top, Math.max(1, barWidth - 0.5), Math.max(1, bottom - top));
  }
}

function drawHistogram(canvas: HTMLCanvasElement, scopes: ScopeData) {
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  const { width, height } = canvas;
  ctx.clearRect(0, 0, width, height);
  ctx.fillStyle = '#101814';
  ctx.fillRect(0, 0, width, height);
  const lumaPeak = Math.max(1, ...scopes.histogramLuma);
  ctx.fillStyle = '#e8f0ea';
  const lumaWidth = width / scopes.histogramLuma.length;
  for (let i = 0; i < scopes.histogramLuma.length; i++) {
    const barHeight = (scopes.histogramLuma[i] / lumaPeak) * height;
    ctx.fillRect(i * lumaWidth, height - barHeight, Math.max(1, lumaWidth - 0.5), barHeight);
  }
  const channels: [number[], string][] = [
    [scopes.histogramRed, '#e57373'],
    [scopes.histogramGreen, '#81c784'],
    [scopes.histogramBlue, '#64b5f6'],
  ];
  for (const [bins, colour] of channels) {
    const peak = Math.max(1, ...bins);
    ctx.strokeStyle = colour;
    ctx.globalAlpha = 0.9;
    ctx.beginPath();
    for (let i = 0; i < bins.length; i++) {
      const x = (i / bins.length) * width;
      const y = height - (bins[i] / peak) * height;
      if (i === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    }
    ctx.stroke();
  }
  ctx.globalAlpha = 1;
}

function drawVectorscope(canvas: HTMLCanvasElement, scopes: ScopeData) {
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  const { width, height } = canvas;
  const size = Math.min(width, height);
  const originX = width / 2;
  const originY = height / 2;
  const radius = size / 2 - 4;
  ctx.clearRect(0, 0, width, height);
  ctx.fillStyle = '#101814';
  ctx.fillRect(0, 0, width, height);
  ctx.strokeStyle = '#3a4a3f';
  ctx.beginPath();
  ctx.arc(originX, originY, radius, 0, Math.PI * 2);
  ctx.stroke();
  ctx.beginPath();
  ctx.moveTo(originX - radius, originY);
  ctx.lineTo(originX + radius, originY);
  ctx.moveTo(originX, originY - radius);
  ctx.lineTo(originX, originY + radius);
  ctx.stroke();
  const cells = 64;
  const peak = Math.max(1, scopes.vectorscopePeak);
  for (let cellY = 0; cellY < cells; cellY++) {
    for (let cellX = 0; cellX < cells; cellX++) {
      const count = scopes.vectorscope[cellY * cells + cellX];
      if (!count) continue;
      const x = originX + ((cellX + 0.5) / cells - 0.5) * 2 * radius;
      const y = originY + ((cellY + 0.5) / cells - 0.5) * 2 * radius;
      const intensity = Math.min(1, count / peak);
      ctx.fillStyle = `rgba(178, 215, 137, ${0.25 + 0.75 * intensity})`;
      const dot = 1 + 3 * intensity;
      ctx.fillRect(x - dot / 2, y - dot / 2, dot, dot);
    }
  }
}

export function ScopesPanel() {
  const { project, frame } = useProject();
  const [scopes, setScopes] = useState<ScopeData | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [fullResolution, setFullResolution] = useState(false);
  const waveformRef = useRef<HTMLCanvasElement>(null);
  const histogramRef = useRef<HTMLCanvasElement>(null);
  const vectorscopeRef = useRef<HTMLCanvasElement>(null);
  const active = project?.tracks.filter(t => t.kind === 'video' && audible(t, project.tracks)).flatMap(t => t.clips.filter(c => c.start <= frame && c.start + c.duration > frame))[0];
  const asset = project?.assets.find(a => a.id === active?.assetId);
  const speed = active?.effects?.finishing?.speed ?? 1;
  const atSeconds = active && project ? (active.sourceIn + (frame - active.start) * speed) / project.settings.fps : 0;

  useEffect(() => {
    if (!project || !active || !asset) { setScopes(null); return; }
    setBusy(true);
    const timer = setTimeout(() => {
      void invoke<ScopeData>('analyze_scopes', { projectId: project.id, revision: project.revision, clipId: active.id, atSeconds, fullResolution })
        .then(result => { setScopes(result); setError(''); })
        .catch(e => setError(String(e)))
        .finally(() => setBusy(false));
    }, 400);
    return () => clearTimeout(timer);
  }, [project?.id, project?.revision, active?.id, frame, fullResolution]);

  useEffect(() => {
    if (!scopes) return;
    if (waveformRef.current) drawWaveform(waveformRef.current, scopes);
    if (histogramRef.current) drawHistogram(histogramRef.current, scopes);
    if (vectorscopeRef.current) drawVectorscope(vectorscopeRef.current, scopes);
  }, [scopes]);

  const refresh = async () => {
    if (!project || !active) return;
    setBusy(true);
    setError('');
    try {
      setScopes(await invoke<ScopeData>('analyze_scopes', { projectId: project.id, revision: project.revision, clipId: active.id, atSeconds, fullResolution }));
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };

  return <section className="scopes-panel" aria-label="Colour scopes">
    <header><strong>Scopes</strong><span>WAVEFORM · HISTOGRAM · VECTORSCOPE</span></header>
    <p className="finish-note">{fullResolution ? FULL_RESOLUTION_NOTE : APPROXIMATION_NOTE}</p>
    <label className="finish-note" style={{ display: 'flex', gap: 8, alignItems: 'center' }}><input type="checkbox" checked={fullResolution} onChange={e => setFullResolution(e.target.checked)} />Full-resolution sampling (slower — analyses the native frame instead of a 320 px sample)</label>
    {asset ? <p className="finish-note">Frame at the playhead — {asset.name} · {scopes ? `${scopes.sampleWidth} × ${scopes.sampleHeight} sample frame` : 'sampling…'}</p> : <p className="finish-note">Place the playhead over a video clip to inspect its frame.</p>}
    {error && <p className="error" role="alert">{error}</p>}
    <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(220px, 1fr))', gap: 12 }}>
      <figure style={{ margin: 0 }}><figcaption>Waveform (luma)</figcaption><canvas ref={waveformRef} width={320} height={120} style={{ width: '100%', display: 'block' }} /></figure>
      <figure style={{ margin: 0 }}><figcaption>Histogram (luma + RGB)</figcaption><canvas ref={histogramRef} width={320} height={120} style={{ width: '100%', display: 'block' }} /></figure>
      <figure style={{ margin: 0 }}><figcaption>Vectorscope (chroma)</figcaption><canvas ref={vectorscopeRef} width={200} height={200} style={{ width: '100%', maxWidth: 200, display: 'block' }} /></figure>
    </div>
    <div style={{ display: 'flex', gap: 8, marginTop: 12 }}>
      <button className="button secondary" onClick={() => void refresh()} disabled={busy || !active}>{busy ? 'Sampling…' : 'Refresh scopes'}</button>
      {scopes?.approximate && <span className="finish-note" style={{ alignSelf: 'center' }}>Approximate — downscaled sample frame</span>}
      {scopes?.fullResolution && <span className="finish-note" style={{ alignSelf: 'center' }}>Full resolution — native frame, no downscaling</span>}
    </div>
  </section>;
}
