import { validateProject, type Project } from './project';

export type CaptionPreset = 'hormozi' | 'minimal' | 'documentary' | 'neon';
export type CaptionWord = { text: string; start: number; end: number };
export type ZoomKey = { frame: number; scale: number; x: number; y: number };
export type GainKey = { frame: number; gain: number };
export type Lut = 'identity' | 'warm' | 'cool';
export type Effects = { zoom: ZoomKey[]; lut: Lut | null; duck: GainKey[]; finishing?: import('./finishing').Finishing };
export type Clip = { id: string; assetId: string; linkId: string | null; start: number; sourceIn: number; duration: number; effects?: Effects; caption?: { preset: CaptionPreset; timing: 'word' | 'segment'; words: CaptionWord[] } };
export type Track = { id: string; name: string; kind: 'video' | 'audio' | 'caption'; muted: boolean; solo: boolean; locked: boolean; clips: Clip[] };
export type Edit =
  | { type: 'addTrack'; kind: Track['kind'] }
  | { type: 'addClip'; assetId: string; start: number; trackId?: string }
  | { type: 'move'; clipId: string; start: number }
  | { type: 'trim'; clipId: string; edge: 'in' | 'out'; frame: number }
  | { type: 'split'; clipId: string; frame: number }
  | { type: 'remove'; clipId: string }
  | { type: 'toggle'; trackId: string; flag: 'muted' | 'solo' | 'locked' };
export const HISTORY_LIMIT = 50;
export function newId() {
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 15) | 64;
  bytes[8] = (bytes[8] & 63) | 128;
  const hex = Array.from(bytes, b => b.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
export function sourceOffset(clip: Clip, frames: number): number {
  const offset = frames * (clip.effects?.finishing?.speed ?? 1);
  if (!Number.isSafeInteger(offset)) throw new Error('Edit must land on a whole source frame at this speed.');
  return offset;
}
export const timelineEnd = (tracks: Track[]) => Math.max(0, ...tracks.flatMap(t => t.clips.map(c => c.start + c.duration)));
export const audible = (track: Track, tracks: Track[]) => !track.muted && (!tracks.some(t => t.kind === track.kind && t.solo) || track.solo);

export function editTimeline(project: Project, edit: Edit): Project {
  const next = structuredClone(project);
  next.schemaVersion = project.schemaVersion >= 6 ? 6 : project.schemaVersion >= 5 ? 5 : project.schemaVersion >= 4 ? 4 : 3;
  next.revision++;
  const tracks = next.tracks;
  const addTrack = (kind: Track['kind']) => {
    const track: Track = { id: newId(), name: `${kind === 'video' ? 'Video' : 'Audio'} ${tracks.filter(t => t.kind === kind).length + 1}`, kind, muted: false, solo: false, locked: false, clips: [] };
    tracks.push(track);
    return track;
  };
  if (edit.type === 'addTrack') addTrack(edit.kind);
  else if (edit.type === 'toggle') {
    const track = tracks.find(t => t.id === edit.trackId);
    if (!track) throw new Error('Track not found.');
    if (track.locked && edit.flag !== 'locked') throw new Error('Unlock the track before editing.');
    track[edit.flag] = !track[edit.flag];
  } else if (edit.type === 'addClip') {
    const asset = next.assets.find(a => a.id === edit.assetId);
    if (!asset) throw new Error('Media not found.');
    const duration = asset.kind === 'image' ? project.settings.fps * 5 : Math.floor(asset.duration! * project.settings.fps);
    if (!duration) throw new Error('Media is shorter than one project frame.');
    const kinds: Track['kind'][] = asset.kind === 'audio' ? ['audio'] : asset.audioStream === null ? ['video'] : ['video', 'audio'];
    const linkId = kinds.length === 2 ? newId() : null;
    for (const kind of kinds) {
      let track = edit.trackId && kind === kinds[0] ? tracks.find(t => t.id === edit.trackId) : undefined;
      if (edit.trackId && kind === kinds[0] && (!track || track.kind !== kind || track.locked)) throw new Error('Choose an unlocked matching track.');
      track ??= tracks.find(t => t.kind === kind && !t.locked && t.clips.every(c => edit.start + duration <= c.start || edit.start >= c.start + c.duration));
      track ??= addTrack(kind);
      track.clips.push({ id: newId(), assetId: asset.id, linkId, start: edit.start, sourceIn: 0, duration });
    }
  } else {
    const selected = tracks.flatMap(t => t.clips).find(c => c.id === edit.clipId);
    if (!selected) throw new Error('Select a clip first.');
    const group = tracks.flatMap(track => track.clips.filter(c => c.id === selected.id || (selected.linkId !== null && c.linkId === selected.linkId)).map(clip => ({ track, clip })));
    if (group.some(({ track }) => track.locked)) throw new Error('Unlock all linked tracks before editing.');
    const rightLink = selected.linkId ? newId() : null;
    for (const { track, clip } of group) {
      if (edit.type === 'move') clip.start = edit.start;
      if (edit.type === 'trim') {
        if (edit.edge === 'in') {
          const delta = edit.frame - clip.start;
          clip.start += delta; clip.sourceIn += sourceOffset(clip, delta); clip.duration -= delta;
        } else clip.duration = edit.frame - clip.start;
      }
      if (edit.type === 'split') {
        const left = edit.frame - clip.start;
        if (left <= 0 || left >= clip.duration) throw new Error('Place the playhead inside the clip to split.');
        track.clips.push({ ...clip, id: newId(), linkId: rightLink, start: edit.frame, sourceIn: clip.sourceIn + sourceOffset(clip, left), duration: clip.duration - left });
        clip.duration = left;
      }
      if (edit.type === 'remove') track.clips = track.clips.filter(c => c.id !== clip.id);
    }
  }
  for (const t of tracks) t.clips.sort((a, b) => a.start - b.start);
  return validateProject(next);
}

export type History = { past: Track[][]; future: Track[][] };
export const emptyHistory = (): History => ({ past: [], future: [] });
export function remember(history: History, tracks: Track[]): History {
  return { past: [...history.past, structuredClone(tracks)].slice(-HISTORY_LIMIT), future: [] };
}
export function travel(project: Project, history: History, direction: 'undo' | 'redo'): { project: Project; history: History } {
  const from = direction === 'undo' ? history.past : history.future;
  if (!from.length) throw new Error(`Nothing to ${direction}.`);
  const tracks = structuredClone(from[from.length - 1]);
  const current = structuredClone(project.tracks);
  return {
    project: validateProject({ ...project, schemaVersion: project.schemaVersion >= 6 ? 6 : project.schemaVersion >= 5 ? 5 : project.schemaVersion >= 4 || tracks.some(t => t.kind === 'caption') ? 4 : 3, revision: project.revision + 1, tracks }),
    history: direction === 'undo'
      ? { past: from.slice(0, -1), future: [...history.future, current].slice(-HISTORY_LIMIT) }
      : { past: [...history.past, current].slice(-HISTORY_LIMIT), future: from.slice(0, -1) },
  };
}
