import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AudioLines, Film, Plus } from 'lucide-react';
import { native, useProject } from './store';
import type { Asset, Project } from './project';
import { validateProject } from './project';
import { attachMedia } from './playback';

type Preview = { thumbnail: number[] | null; peaks: number[] | null };
function MediaCard({ asset, missing, bins }: { asset: Asset; missing: boolean; bins: string[] }) {
  const { renameAsset, removeAsset, relinkAsset } = useProject();
  const [preview, setPreview] = useState<{ image?: string; peaks?: number[] }>({});
  const [error, setError] = useState('');
  const [renaming, setRenaming] = useState(false);
  const [nameDraft, setNameDraft] = useState(asset.name);
  const [relinkPath, setRelinkPath] = useState('');
  const [binDraft, setBinDraft] = useState(asset.bin ?? '');
  const [newBin, setNewBin] = useState('');
  async function applyBin() {
    const bin = newBin.trim() || binDraft;
    if (bin === (asset.bin ?? '') || bin.length > 80) return;
    const project = useProject.getState().project;
    if (!project) return;
    try {
      const next = native
        ? validateProject(await invoke<Project>('set_asset_bin', { assetId: asset.id, bin }))
        : validateProject({ ...structuredClone(project), assets: project.assets.map(a => a.id === asset.id ? { ...a, bin } : a), revision: project.revision + 1 });
      useProject.setState({ project: next, saved: false, message: bin ? `“${asset.name}” moved to bin “${bin}”.` : `“${asset.name}” moved to Unsorted.` });
      setNewBin('');
    } catch (e) { setError(String(e)); }
  }
  useEffect(() => {
    if (!native) return;
    let active = true;
    let url: string | undefined;
    void invoke<Preview>('media_preview', { id: asset.id }).then(data => {
      if (!active) return;
      if (data.thumbnail) url = URL.createObjectURL(new Blob([new Uint8Array(data.thumbnail)], { type: 'image/jpeg' }));
      setPreview({ image: url, peaks: data.peaks ?? undefined });
    }).catch(e => { if (active) setError(String(e)); });
    return () => { active = false; if (url) URL.revokeObjectURL(url); };
  }, [asset.id]);
  const peaks = preview.peaks;
  const bars = peaks ? Array.from({ length: Math.min(peaks.length, 80) }, (_, i) => {
    const count = Math.min(peaks.length, 80);
    const start = Math.floor(i * peaks.length / count);
    const end = Math.max(start + 1, Math.floor((i + 1) * peaks.length / count));
    return Math.max(...peaks.slice(start, end));
  }) : [];
  return <article className="media-card">
    <div className="media-art">{preview.image ? <img src={preview.image} alt={`Thumbnail of ${asset.name}`} /> : asset.kind === 'audio' ? <AudioLines size={28} /> : <Film size={28} />}</div>
    <h3 title={asset.sourcePath}>{asset.name}</h3>
    <p>{asset.kind} · {asset.duration === null ? 'Still image' : `${asset.duration.toFixed(2)} s`}{asset.width ? ` · ${asset.width} × ${asset.height}` : ''}</p>
    <p>{[asset.videoCodec, asset.audioCodec, asset.fps ? `${asset.fps.toFixed(2)} fps` : null, asset.sampleRate ? `${asset.sampleRate / 1000} kHz` : null].filter(Boolean).join(' · ')}</p>
    {bars.length > 0 && <svg className="waveform" viewBox={`0 0 ${bars.length * 3} 40`} role="img" aria-label={`Waveform of ${asset.name}`}>{bars.map((peak, i) => <rect key={i} x={i * 3} y={20 - Math.max(1, peak * 19)} width="2" height={Math.max(2, peak * 38)} />)}</svg>}
    <div className="media-bin"><label>Bin<select aria-label={`Bin for ${asset.name}`} value={binDraft} onChange={e => setBinDraft(e.target.value)}><option value="">Unsorted</option>{bins.map(b => <option key={b} value={b}>{b}</option>)}</select></label><label>New bin<input aria-label={`New bin for ${asset.name}`} value={newBin} onChange={e => setNewBin(e.target.value)} placeholder="New bin name…" maxLength={80} /></label><button className="button secondary" onClick={() => void applyBin()} disabled={!newBin.trim() && binDraft === (asset.bin ?? '')}>Set bin</button></div>
    {error && <p className="error">{error}</p>}
    {missing && <p className="error" role="alert">Source file missing — moved or deleted. Relink it to use this media again.</p>}
    <div className="media-card-actions"><button className="button secondary" onClick={() => { const { edit, frame } = useProject.getState(); void edit({ type: 'addClip', assetId: asset.id, start: Math.round(frame) }); }}>Add to timeline</button><button className="text-button" draggable onDragStart={e => e.dataTransfer.setData('application/studiocut-asset', asset.id)}>Drag to a track</button><button className="text-button" onClick={() => { setNameDraft(asset.name); setRenaming(!renaming); }}>Rename</button><button className="text-button" onClick={() => { if (window.confirm(`Remove "${asset.name}" from the project? The original file stays untouched. Clips using it must be removed first.`)) void removeAsset(asset.id); }}>Delete</button>{!native && <label>Reattach original for playback<input aria-label={`Reattach ${asset.name}`} type="file" onChange={e => { const file = e.target.files?.[0]; if (file) { try { attachMedia(asset, file); setError(''); useProject.setState({ message: `${asset.name} attached for this session.` }); } catch (error) { setError(String(error)); } } }} /></label>}</div>
    {renaming && <form className="media-rename" onSubmit={e => { e.preventDefault(); if (nameDraft.trim() && nameDraft.trim() !== asset.name) void renameAsset(asset.id, nameDraft.trim()); setRenaming(false); }}><label>Rename media<input aria-label={`Rename ${asset.name}`} value={nameDraft} onChange={e => setNameDraft(e.target.value)} maxLength={120} /></label><button className="button secondary" type="submit">Save name</button><button className="text-button" type="button" onClick={() => setRenaming(false)}>Cancel</button></form>}
    {missing && native && <form className="media-relink" onSubmit={e => { e.preventDefault(); if (relinkPath.trim()) void relinkAsset(asset.id, relinkPath.trim()); }}><label>New location (absolute path)<input aria-label={`Relink ${asset.name}`} value={relinkPath} onChange={e => setRelinkPath(e.target.value)} placeholder="/new/location/clip.mp4" spellCheck={false} /></label><button className="button secondary" type="submit" disabled={!relinkPath.trim()}>Relink</button></form>}
  </article>;
}

export function MediaLibrary() {
  const { project, busy, importMedia } = useProject();
  const [paths, setPaths] = useState('');
  const [query, setQuery] = useState('');
  const [kind, setKind] = useState<'all' | 'video' | 'audio' | 'image'>('all');
  const [binFilter, setBinFilter] = useState<string>('all');
  const [missing, setMissing] = useState<Set<string>>(new Set());
  useEffect(() => {
    if (!native || !project) { setMissing(new Set()); return; }
    let active = true;
    void invoke<string[]>('media_missing').then(ids => { if (active) setMissing(new Set(ids)); }).catch(() => { if (active) setMissing(new Set()); });
    return () => { active = false; };
  }, [project?.id, project?.revision]);
  if (!project) return <div className="media-empty"><div className="stack-icon"><Film size={24} /></div><h3>A home for your footage</h3><p>Create a project to start organizing your next edit.</p><span className="small-tag">NON-DESTRUCTIVE BY DESIGN</span></div>;
  const bins = [...new Set(project.assets.map(a => a.bin ?? '').filter(Boolean))].sort();
  const visible = project.assets.filter(a => (kind === 'all' || a.kind === kind) && (binFilter === 'all' || (a.bin ?? '') === binFilter) && a.name.toLowerCase().includes(query.trim().toLowerCase()));
  return <div className="media-library">
    <button className="button secondary" disabled={busy || !native} onClick={() => void importMedia()}><Plus size={15} />Import media</button>
    {native ? <details><summary>Import by local path</summary><form onSubmit={e => { e.preventDefault(); void importMedia(paths.split('\n').map(p => p.trim()).filter(Boolean)); }}><label htmlFor="media-paths">Absolute file paths, one per line</label><textarea id="media-paths" value={paths} onChange={e => setPaths(e.target.value)} rows={3} required /><button className="button secondary" disabled={busy || !paths.trim()}>Import paths</button></form></details> : <p className="import-note">Media import requires the desktop app with FFmpeg and FFprobe installed. Browser preview can play media reattached to an opened project.</p>}
    <p className="import-note">{busy ? 'Processing… Long audio can take several minutes.' : 'Video, audio and images · up to 6 hours per file. Originals stay untouched.'}</p>
    {project.assets.length > 0 && <div className="media-filter" role="search"><input type="search" value={query} onChange={e => setQuery(e.target.value)} placeholder="Search media by name…" aria-label="Search media by name" /><select value={kind} onChange={e => setKind(e.target.value as typeof kind)} aria-label="Filter media by kind"><option value="all">All kinds</option><option value="video">Video</option><option value="audio">Audio</option><option value="image">Images</option></select><span aria-live="polite">{visible.length} of {project.assets.length}</span></div>}
    {project.assets.length > 0 && <div className="media-filter" role="group" aria-label="Filter media by bin"><button className={`toggle-button${binFilter === 'all' ? ' on' : ''}`} aria-pressed={binFilter === 'all'} onClick={() => setBinFilter('all')}>All bins</button><button className={`toggle-button${binFilter === '' ? ' on' : ''}`} aria-pressed={binFilter === ''} onClick={() => setBinFilter('')}>Unsorted</button>{bins.map(b => <button key={b} className={`toggle-button${binFilter === b ? ' on' : ''}`} aria-pressed={binFilter === b} onClick={() => setBinFilter(b)}>{b}</button>)}</div>}
    {project.assets.length ? (visible.length ? <div className="media-list">{visible.map(asset => <MediaCard key={`${asset.id}:${project.revision}`} asset={asset} missing={missing.has(asset.id)} bins={bins} />)}</div> : <div className="media-empty"><Film size={24} /><h3>No matches</h3><p>Nothing in the bin matches that search or filter. Clear it to see all {project.assets.length} assets.</p></div>) : <div className="media-empty"><Film size={24} /><h3>No media yet</h3><p>Import footage to inspect metadata, thumbnails and waveforms.</p></div>}
  </div>;
}
