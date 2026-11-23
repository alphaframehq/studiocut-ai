import { useState } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { defaultFinishing, validateCube, type Finishing } from './finishing';
import { validateProject } from './project';
import { useProject } from './store';
import type { Lut } from './timeline';
import './finishing.css';

export function FinishingPanel() {
  const { project, selected, frame, busy, replaceTracks } = useProject();
  const [error, setError] = useState('');
  const track = project?.tracks.find(t => t.clips.some(c => c.id === selected));
  const clip = track?.clips.find(c => c.id === selected);
  const f = clip?.effects?.finishing ?? defaultFinishing();
  async function apply(patch: Partial<Finishing>, lut?: Lut | null) {
    if (!project || !clip || !track) return;
    setError('');
    try {
      const next = structuredClone(project); next.schemaVersion = 6;
      for (const t of next.tracks) for (const c of t.clips) {
        const linkedSpeed = patch.speed !== undefined && clip.linkId !== null && clip.linkId === c.linkId;
        if (c.id !== clip.id && !linkedSpeed) continue;
        if (t.locked) throw new Error('Unlock every affected track first.');
        c.effects ??= { zoom: [], lut: null, duck: [] };
        c.effects.finishing = { ...(c.effects.finishing ?? defaultFinishing()), ...(c.id === clip.id ? patch : { speed: patch.speed! }) };
        if (c.id === clip.id && lut !== undefined) c.effects.lut = lut;
      }
      validateProject(next);
      await replaceTracks(next.tracks, project.id, project.revision);
    } catch (e) { setError(String(e)); }
  }
  const number = (label: string, key: 'speed' | 'brightness' | 'contrast' | 'saturation' | 'gain', min: number, max: number, step = .05) => <label>{label}<input aria-label={label} type="number" min={min} max={max} step={step} key={`${clip?.id}-${f[key]}`} defaultValue={f[key]} onBlur={e => { const value = Number(e.currentTarget.value); if (value !== f[key]) void apply({ [key]: value }); }} onKeyDown={e => { if (e.key === 'Enter') e.currentTarget.blur(); }} /></label>;
  return <section className="finishing-panel" aria-label="Color and audio inspector"><header><strong>Color / Audio</strong><span>EXPORT FINISHING</span></header>
    <p className="finish-note">These settings are applied on export, not in the preview player.</p>
    {!clip || !track || track.kind === 'caption' ? <p>Select a video or audio clip in the timeline to finish it.</p> : <fieldset disabled={busy || track.locked}><legend>{track.name} · selected clip</legend>
      {number('Speed (linked A/V)', 'speed', .25, 4, .25)}
      <small>Duration stays fixed; speed changes source consumption. Trim first to leave enough source media.</small>
      {track.kind === 'video' ? <>
        <div className="finish-grid">{number('Brightness', 'brightness', -1, 1)}{number('Contrast', 'contrast', 0, 2)}{number('Saturation', 'saturation', 0, 3)}</div>
        <div className="finish-grid">{['Red balance', 'Green balance', 'Blue balance'].map((label, i) => <label key={label}>{label}<input type="number" min={-1} max={1} step={.05} key={`${clip.id}-${f.balance[i]}`} defaultValue={f.balance[i]} onBlur={e => { const balance = [...f.balance] as Finishing['balance']; balance[i] = Number(e.target.value); if (balance[i] !== f.balance[i]) void apply({ balance }); }} /></label>)}</div>
        <label>LUT preset<select value={f.cube ? 'custom' : clip.effects?.lut ?? 'none'} onChange={e => void apply({ cube: null }, e.target.value === 'none' ? null : e.target.value as Lut)}><option value="none">No LUT</option><option value="identity">StudioCut Identity · CC0</option><option value="warm">StudioCut Warm · CC0</option><option value="cool">StudioCut Cool · CC0</option>{f.cube && <option value="custom">Imported .cube</option>}</select></label>
        <label>Import .cube LUT<input type="file" accept=".cube" onChange={async e => {
          const file = e.target.files?.[0]; if (!file) return;
          try { if (!file.name.toLowerCase().endsWith('.cube') || file.size > 262144) throw new Error('Choose a .cube file of at most 256 KB.'); const cube = await file.text(); validateCube(cube); if (isTauri()) await invoke('lut_validate', { text: cube }); await apply({ cube }); } catch (e) { setError(String(e)); }
          e.target.value = '';
        }} /></label>
        <details><summary>Transform / opacity keyframes ({f.transforms.length})</summary><p className="finish-note">Source-frame keys interpolate linearly. X/Y are canvas fractions; 0 is centered.</p>
          {f.transforms.map((key, i) => <div className="keyframe-row" key={`${clip.id}-${key.frame}`}><strong>Frame {key.frame}</strong>{(['scale', 'x', 'y', 'opacity'] as const).map(name => <label key={`${name}-${key[name]}`}>{name}<input aria-label={`${name} at frame ${key.frame}`} type="number" step={.05} defaultValue={key[name]} onBlur={e => { const transforms = structuredClone(f.transforms); transforms[i][name] = Number(e.target.value); if (transforms[i][name] !== key[name]) void apply({ transforms }); }} /></label>)}<button onClick={() => void apply({ transforms: f.transforms.filter((_, index) => index !== i) })}>Remove key {key.frame}</button></div>)}
          <button onClick={() => { const sourceFrame = clip.sourceIn + (frame - clip.start) * f.speed; if (frame < clip.start || frame >= clip.start + clip.duration || !Number.isInteger(sourceFrame)) { setError('Place the playhead inside the clip at a whole source frame.'); return; } if (f.transforms.some(k => k.frame === sourceFrame)) return; void apply({ transforms: [...f.transforms, { frame: sourceFrame, scale: 1, x: 0, y: 0, opacity: 1 }].sort((a, b) => a.frame - b.frame) }); }}>Add key at playhead</button>
        </details>
      </> : <>
        {number('Output gain', 'gain', 0, 2)}
        <label>Audio role<select value={f.role} onChange={e => void apply({ role: e.target.value as Finishing['role'], autoDuck: e.target.value === 'music' && f.autoDuck })}><option value="normal">Normal</option><option value="voice">Voice / dialogue</option><option value="music">Background music</option></select></label>
        <label className="finish-check"><input type="checkbox" checked={f.denoise} onChange={e => void apply({ denoise: e.target.checked })} /> FFT noise reduction (afftdn)</label>
        <label className="finish-check"><input type="checkbox" checked={f.level} onChange={e => void apply({ level: e.target.checked })} /> Vocal leveling (compressor)</label>
        <label className="finish-check"><input type="checkbox" checked={f.autoDuck} disabled={f.role !== 'music'} onChange={e => void apply({ autoDuck: e.target.checked })} /> Duck background music under voice</label>
        <small>Auto-ducking listens to unmuted Voice tracks. No Voice track means no ducking.</small>
      </>}
    </fieldset>}
    {error && <p role="alert" className="error">{error}</p>}
  </section>;
}
