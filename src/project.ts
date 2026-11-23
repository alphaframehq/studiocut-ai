import type { Track, Clip } from './timeline';
import { validateEffects } from './edl';

export type Asset = {
  id: string; sourcePath: string; name: string; sizeBytes: number; modifiedNs: string;
  kind: 'video' | 'audio' | 'image'; duration: number | null; width: number | null; height: number | null;
  fps: number | null; videoStream: number | null; audioStream: number | null;
  videoCodec: string | null; audioCodec: string | null; sampleRate: number | null; channels: number | null; createdAt: number;
};
export type Project = {
  schemaVersion: 1 | 2 | 3 | 4 | 5 | 6;
  id: string;
  name: string;
  revision: number;
  createdAt: number;
  settings: { width: number; height: number; fps: number; audioSampleRate: 48000 };
  assets: Asset[];
  tracks: Track[];
};

const uuid = (value: unknown) => typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value);
const keys = (obj: object, allowed: string[]) => Object.keys(obj).every(key => allowed.includes(key));
const uint = (n: number) => Number.isSafeInteger(n) && n >= 0;

export function validateProject(value: unknown): Project {
  if (!value || typeof value !== 'object') throw new Error('This file is not a StudioCut project.');
  const p = value as Project;
  const s = p.settings;
  if (![1, 2, 3, 4, 5, 6].includes(p.schemaVersion)) throw new Error('Unsupported project version.');
  if (!uuid(p.id)) throw new Error('Invalid project identity.');
  if (typeof p.name !== 'string' || !p.name.trim() || p.name.length > 120) throw new Error('Project name must be 1–120 characters.');
  if (!uint(p.revision) || !uint(p.createdAt) || (p.schemaVersion === 1 && p.revision !== 0)) throw new Error('Invalid project revision or creation date.');
  if (!s || !Number.isInteger(s.width) || !Number.isInteger(s.height) || s.width < 16 || s.width > 8192 || s.height < 16 || s.height > 8192 || ![24, 25, 30, 50, 60].includes(s.fps) || s.audioSampleRate !== 48000) throw new Error('Unsupported project settings.');
  if (!Array.isArray(p.assets) || !Array.isArray(p.tracks) || (p.schemaVersion < 3 && p.tracks.length) || (p.schemaVersion === 1 && p.assets.length)) throw new Error('Unsupported project graph. Your file has not been changed.');
  const ids = new Set<string>();
  for (const a of p.assets) {
    if (!a || typeof a !== 'object' || !uuid(a.id) || ids.has(a.id) || typeof a.sourcePath !== 'string' || !a.sourcePath || a.sourcePath.includes('\0') || typeof a.name !== 'string' || !a.name || !uint(a.sizeBytes) || a.sizeBytes === 0 || !uint(a.createdAt) || typeof a.modifiedNs !== 'string' || !/^\d+$/.test(a.modifiedNs) || BigInt(a.modifiedNs) > (1n << 128n) - 1n || !['video', 'audio', 'image'].includes(a.kind)
      || (a.duration !== null && (!Number.isFinite(a.duration) || a.duration <= 0 || a.duration > 21600))
      || (a.fps !== null && (!Number.isFinite(a.fps) || a.fps < 0))
      || [a.width, a.height, a.sampleRate, a.channels].some(n => n !== null && (!uint(n) || n === 0 || n > 4294967295))
      || [a.videoStream, a.audioStream].some(n => n !== null && (!uint(n) || n > 4294967295))
      || [a.videoCodec, a.audioCodec].some(n => n !== null && typeof n !== 'string')
      || (a.videoStream === null && a.audioStream === null) || (a.kind === 'audio' && a.audioStream === null)
      || (a.kind !== 'audio' && (a.videoStream === null || a.width === null || a.height === null)) || (a.kind !== 'image' && a.duration === null)
      || !keys(a, ['id', 'sourcePath', 'name', 'sizeBytes', 'modifiedNs', 'kind', 'duration', 'width', 'height', 'fps', 'videoStream', 'audioStream', 'videoCodec', 'audioCodec', 'sampleRate', 'channels', 'createdAt'])) throw new Error('Invalid media metadata.');
    ids.add(a.id);
  }
  const trackIds = new Set<string>();
  const clipIds = new Set<string>();
  const links = new Map<string, { kind: string; clip: Clip }[]>();
  let count = 0;
  if (p.tracks.length > 32) throw new Error('Maximum 32 tracks.');
  for (const t of p.tracks) {
    if (!t || !uuid(t.id) || trackIds.has(t.id) || typeof t.name !== 'string' || !t.name.trim() || t.name.length > 80 || !['video', 'audio', 'caption'].includes(t.kind) || (t.kind === 'caption' && p.schemaVersion < 4) || [t.muted, t.solo, t.locked].some(v => typeof v !== 'boolean') || !Array.isArray(t.clips) || !keys(t, ['id', 'name', 'kind', 'muted', 'solo', 'locked', 'clips'])) throw new Error('Invalid timeline track.');
    trackIds.add(t.id);
    let end = 0;
    for (const c of t.clips) {
      const asset = p.assets.find(a => a.id === c?.assetId);
      const sourceDuration = Math.ceil(c?.duration * (c?.effects?.finishing?.speed ?? 1));
      if (c?.effects?.finishing && p.schemaVersion < 6) throw new Error('Finishing requires project v6.');
      if (!c || !uuid(c.id) || clipIds.has(c.id) || !asset || ![c.start, c.sourceIn, c.duration].every(uint) || c.duration === 0 || c.start < end || c.start + c.duration > 21600 * s.fps || c.sourceIn + sourceDuration > 21600 * s.fps || (asset.kind !== 'image' && c.sourceIn + sourceDuration > Math.floor(asset.duration! * s.fps)) || (t.kind === 'audio' ? asset.audioStream === null : t.kind === 'video' && (asset.videoStream === null || asset.kind === 'audio')) || (c.linkId !== null && !uuid(c.linkId)) || !keys(c, ['id', 'assetId', 'linkId', 'start', 'sourceIn', 'duration', 'caption', 'effects'])) throw new Error('Invalid clip: check source bounds, overlaps, and track type.');
      if (c.effects != null) {
        if (p.schemaVersion < 5) throw new Error('Effects require project v5.');
        validateEffects(c.effects, t.kind, asset.kind === 'image' ? 21600 * s.fps : Math.floor(asset.duration! * s.fps));
      }
      if (t.kind === 'caption') {
        const caption = c.caption;
        if (!caption || c.linkId !== null || !['hormozi', 'minimal', 'documentary', 'neon'].includes(caption.preset) || !['word', 'segment'].includes(caption.timing) || !Array.isArray(caption.words) || !caption.words.length || caption.words.length > 200 || !keys(caption, ['preset', 'timing', 'words'])) throw new Error('Invalid caption clip.');
        let previous = 0;
        for (const word of caption.words) {
          if (!word || typeof word.text !== 'string' || !word.text.trim() || word.text.length > 500 || !uint(word.start) || !uint(word.end) || word.start < previous || word.end <= word.start || word.end > Math.floor((asset.duration ?? 0) * s.fps) || !keys(word, ['text', 'start', 'end'])) throw new Error('Invalid caption timestamps.');
          previous = word.end;
        }
      } else if (c.caption != null) throw new Error('Caption data must belong to a caption track.');
      end = c.start + c.duration;
      clipIds.add(c.id);
      count++;
      if (c.linkId) links.set(c.linkId, [...(links.get(c.linkId) ?? []), { kind: t.kind, clip: c }]);
    }
  }
  if (count > 2000) throw new Error('Maximum 2,000 clips.');
  for (const pair of links.values()) {
    const [a, b] = pair;
    if (pair.length !== 2 || a.kind === b.kind || (a.clip.effects?.finishing?.speed ?? 1) !== (b.clip.effects?.finishing?.speed ?? 1) || ['assetId', 'start', 'sourceIn', 'duration'].some(k => a.clip[k as keyof Clip] !== b.clip[k as keyof Clip])) throw new Error('Linked clips must be a synchronized video/audio pair.');
  }
  if (!keys(p, ['schemaVersion', 'id', 'name', 'revision', 'createdAt', 'settings', 'assets', 'tracks']) || !keys(s, ['width', 'height', 'fps', 'audioSampleRate'])) throw new Error('This project contains unsupported fields.');
  if (new TextEncoder().encode(JSON.stringify(p, null, 2)).length > 1048576) throw new Error('Project file exceeds 1 MB.');
  return p;
}
