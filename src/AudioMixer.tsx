import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useProject } from './store';
import { audible } from './timeline';

/** Mixer gain helpers. These mirror `audio_gain` in
 * `src-tauri/src/scopes.rs`; keep the two in agreement. */
export const MIN_GAIN_DB = -60;
export const MAX_GAIN_DB = 12;
export function clampGainDb(gainDb: number): number {
  if (!Number.isFinite(gainDb)) return 0;
  return Math.min(MAX_GAIN_DB, Math.max(MIN_GAIN_DB, gainDb));
}
export function gainDbToLinear(gainDb: number): number {
  return Math.pow(10, clampGainDb(gainDb) / 20);
}
export function effectiveLoudnessLufs(measuredLufs: number, gainDb: number): number {
  return measuredLufs + clampGainDb(gainDb);
}

type LoudnessResult = { integratedLufs: number; gainDb: number; targetLufs: number };
type TrackLoudness = { clipId: string; clipName: string; result: LoudnessResult } | null;

/** Target loudness for the meter, in LUFS (matches Phase 4). */
const TARGET_LUFS = -14;
/** Meter floor, in LUFS. */
const METER_FLOOR_LUFS = -60;

function gainKey(projectId: string, trackId: string) {
  return `studiocut-mixer-gain:${projectId}:${trackId}`;
}

/** Stored mixer gain for one track, in dB. Unstored tracks read 0 dB. */
export function mixerGainDb(projectId: string, trackId: string): number {
  const stored = Number(localStorage.getItem(gainKey(projectId, trackId)));
  return Number.isFinite(stored) ? clampGainDb(stored) : 0;
}

/** Stored mixer gains for the given tracks, keyed by track id, for
 * export and preview playback. Tracks without a stored gain read 0 dB. */
export function mixerGainsForProject(projectId: string, trackIds: string[]): Record<string, number> {
  const gains: Record<string, number> = {};
  for (const trackId of trackIds) gains[trackId] = mixerGainDb(projectId, trackId);
  return gains;
}

function LoudnessMeter({ loudness, gainDb }: { loudness: TrackLoudness; gainDb: number }) {
  if (!loudness) return <p className="finish-note">Measure a clip to see its integrated loudness against the −14 LUFS target.</p>;
  const measured = loudness.result.integratedLufs;
  const effective = effectiveLoudnessLufs(measured, gainDb);
  const position = (value: number) => `${Math.min(100, Math.max(0, ((value - METER_FLOOR_LUFS) / (0 - METER_FLOOR_LUFS)) * 100))}%`;
  return <div>
    <p className="finish-note">
      {loudness.clipName}: integrated {measured.toFixed(1)} LUFS · suggested {loudness.result.gainDb >= 0 ? '+' : ''}{loudness.result.gainDb.toFixed(1)} dB to reach {loudness.result.targetLufs} LUFS
      {gainDb !== 0 ? ` · estimated ${effective.toFixed(1)} LUFS with the current gain` : ''}
    </p>
    <div role="meter" aria-valuemin={METER_FLOOR_LUFS} aria-valuemax={0} aria-valuenow={Number(effective.toFixed(1))} aria-label={`Loudness meter, ${effective.toFixed(1)} LUFS`} style={{ position: 'relative', height: 10, background: '#101814', borderRadius: 4, overflow: 'hidden' }}>
      <div style={{ position: 'absolute', left: 0, top: 0, bottom: 0, width: position(effective), background: '#b2d789' }} />
      <div style={{ position: 'absolute', left: position(TARGET_LUFS), top: 0, bottom: 0, width: 2, background: '#e8f0ea' }} title="−14 LUFS target" />
    </div>
    <p className="finish-note">Target −14 LUFS · loudness measurement only — audio is never modified.</p>
  </div>;
}

function MixerTrack({ trackId }: { trackId: string }) {
  const { project, edit } = useProject();
  const track = project?.tracks.find(t => t.id === trackId);
  const [gainDb, setGainDb] = useState(0);
  const [loudness, setLoudness] = useState<TrackLoudness>(null);
  const [measuring, setMeasuring] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    if (project) setGainDb(mixerGainDb(project.id, trackId));
    setLoudness(null);
  }, [project?.id, trackId]);
  if (!project || !track) return null;
  const clips = track.clips.map(c => ({ clip: c, asset: project.assets.find(a => a.id === c.assetId) })).filter(c => c.asset?.audioStream != null);
  const firstClip = clips[0];
  const changeGain = (value: number) => {
    const clamped = clampGainDb(value);
    setGainDb(clamped);
    localStorage.setItem(gainKey(project.id, trackId), String(clamped));
  };
  const measure = async () => {
    if (!firstClip) return;
    setMeasuring(true);
    setError('');
    try {
      const result = await invoke<LoudnessResult>('measure_loudness', { projectId: project.id, revision: project.revision, clipId: firstClip.clip.id });
      setLoudness({ clipId: firstClip.clip.id, clipName: firstClip.asset?.name ?? 'Clip', result });
    } catch (e) { setError(String(e)); } finally { setMeasuring(false); }
  };
  const toggle = (flag: 'muted' | 'solo') => {
    void edit({ type: 'toggle', trackId: track.id, flag }).catch(e => setError(String(e)));
  };
  return <article style={{ borderTop: '1px solid #344a35', padding: '12px 0' }}>
    <div style={{ display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap' }}>
      <strong style={{ fontSize: 12 }}>{track.name}</strong>
      <span className="finish-note">{audible(track, project.tracks) ? 'Audible in preview' : 'Muted in preview'}</span>
      <span style={{ marginLeft: 'auto', display: 'flex', gap: 6 }}>
        <button className="button secondary" aria-pressed={track.muted} onClick={() => toggle('muted')}>{track.muted ? 'Unmute' : 'Mute'}</button>
        <button className="button secondary" aria-pressed={track.solo} onClick={() => toggle('solo')}>{track.solo ? 'Unsolo' : 'Solo'}</button>
      </span>
    </div>
    <label style={{ display: 'flex', alignItems: 'center', gap: 10, fontSize: 11, marginTop: 10 }}>
      Gain
      <input aria-label={`Gain for ${track.name}`} type="range" min={MIN_GAIN_DB} max={MAX_GAIN_DB} step={0.5} value={gainDb} onChange={e => changeGain(Number(e.currentTarget.value))} style={{ flex: 1, accentColor: '#b2d789' }} />
      <output>{gainDb >= 0 ? '+' : ''}{gainDb.toFixed(1)} dB</output>
      <span className="finish-note">×{gainDbToLinear(gainDb).toFixed(2)}</span>
    </label>
    {error && <p className="error" role="alert">{error}</p>}
    <div style={{ marginTop: 8 }}>
      {firstClip
        ? <button className="button secondary" onClick={() => void measure()} disabled={measuring}>{measuring ? 'Measuring…' : `Measure loudness — ${firstClip.asset?.name ?? 'clip'}`}</button>
        : <p className="finish-note">No clip with audio on this track.</p>}
    </div>
    <LoudnessMeter loudness={loudness} gainDb={gainDb} />
  </article>;
}

export function AudioMixer() {
  const { project } = useProject();
  const audioTracks = project?.tracks.filter(t => t.kind === 'audio') ?? [];
  return <section className="audio-mixer" aria-label="Audio mixer">
    <header><strong>Audio Mixer</strong><span>GAIN · MUTE · SOLO · LOUDNESS</span></header>
    <p className="finish-note">
      Mute and solo apply immediately in preview. Gain is applied in preview playback and on export.
    </p>
    {audioTracks.length === 0
      ? <p className="finish-note">Add an audio track to mix it here.</p>
      : audioTracks.map(t => <MixerTrack key={t.id} trackId={t.id} />)}
  </section>;
}
