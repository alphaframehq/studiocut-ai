import { validateProject, type Project } from './project';
import { rippleDelete } from './analysis';
import { validateFinishing } from './finishing';
import { sourceOffset, type Clip, type Track, type ZoomKey, type GainKey, type Lut } from './timeline';

export type Operation =
  | { type: 'addTrack'; trackId: string; name: string; kind: Track['kind'] }
  | { type: 'insert' | 'broll' | 'sfx' | 'captions'; trackId: string; clip: Clip }
  | { type: 'move'; clipId: string; start: number }
  | { type: 'trim'; clipId: string; start: number; sourceIn: number; duration: number }
  | { type: 'split'; clipId: string; frame: number; rightIds: string[]; rightLinkId: string | null }
  | { type: 'delete'; clipId: string }
  | { type: 'ripple'; start: number; end: number }
  | { type: 'jCut' | 'lCut'; clipId: string; frames: number }
  | { type: 'zoom'; clipId: string; keys: ZoomKey[] }
  | { type: 'lut'; clipId: string; preset: Lut }
  | { type: 'duck'; clipId: string; keys: GainKey[] }
  | { type: 'captionStyle'; clipId: string; caption: NonNullable<Clip['caption']> };
export type Edl = { version: 1; projectId: string; baseRevision: number; summary: string; operations: Operation[] };
const fields: Record<Operation['type'], string[]> = {
  addTrack: ['trackId', 'name', 'kind'], insert: ['trackId', 'clip'], broll: ['trackId', 'clip'], sfx: ['trackId', 'clip'], captions: ['trackId', 'clip'],
  move: ['clipId', 'start'], trim: ['clipId', 'start', 'sourceIn', 'duration'], split: ['clipId', 'frame', 'rightIds', 'rightLinkId'], delete: ['clipId'], ripple: ['start', 'end'],
  jCut: ['clipId', 'frames'], lCut: ['clipId', 'frames'], zoom: ['clipId', 'keys'], lut: ['clipId', 'preset'], duck: ['clipId', 'keys'], captionStyle: ['clipId', 'caption'],
};
const uuid = (s: unknown): s is string => typeof s === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(s);
const uint = (n: unknown): n is number => typeof n === 'number' && Number.isSafeInteger(n) && n >= 0 && n <= 4294967295;
function object(value: unknown, required: string[], optional: string[] = []): asserts value is Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value) || required.some(k => !(k in value)) || Object.keys(value).some(k => ![...required, ...optional].includes(k))) throw new Error('Missing or unknown EDL field.');
}
function caption(value: unknown) {
  object(value, ['preset', 'timing', 'words']);
  if (!['hormozi', 'minimal', 'documentary', 'neon'].includes(value.preset as string) || !['word', 'segment'].includes(value.timing as string) || !Array.isArray(value.words) || value.words.length > 200) throw new Error('Invalid caption.');
  for (const word of value.words) { object(word, ['text', 'start', 'end']); if (typeof word.text !== 'string' || !uint(word.start) || !uint(word.end)) throw new Error('Invalid caption word.'); }
}
export function validateEffects(value: unknown, kind: Track['kind'], max: number) {
  object(value, ['zoom', 'lut', 'duck'], ['finishing']);
  if (value.finishing !== undefined) validateFinishing(value.finishing, kind, max);
  if (!Array.isArray(value.zoom) || !Array.isArray(value.duck) || value.zoom.length > 64 || value.duck.length > 128 || (value.lut !== null && !['identity', 'warm', 'cool'].includes(value.lut as string)) || (kind !== 'video' && (value.zoom.length > 0 || value.lut !== null)) || (kind !== 'audio' && value.duck.length > 0)) throw new Error('Effects incompatible with track or too many keyframes.');
  for (const [list, keys] of [[value.zoom, ['frame', 'scale', 'x', 'y']], [value.duck, ['frame', 'gain']]] as const) {
    let previous = -1;
    for (const key of list) {
      object(key, [...keys]);
      if (!uint(key.frame) || key.frame <= previous || key.frame > max) throw new Error('Invalid effect frame.');
      for (const name of keys.filter(k => k !== 'frame')) { const n = key[name]; if (typeof n !== 'number' || !Number.isFinite(n) || n < (name === 'scale' ? 1 : 0) || n > (name === 'scale' ? 4 : 1)) throw new Error('Invalid effect value.'); }
      previous = key.frame;
    }
  }
}
export function parseEdl(value: unknown): Edl {
  if (typeof value === 'string') { if (new TextEncoder().encode(value).length > 262144) throw new Error('EDL exceeds 256 KB.'); value = JSON.parse(value); }
  object(value, ['version', 'projectId', 'baseRevision', 'summary', 'operations']);
  if (value.version !== 1 || !uuid(value.projectId) || !Number.isSafeInteger(value.baseRevision) || (value.baseRevision as number) < 0 || typeof value.summary !== 'string' || !value.summary.trim() || value.summary.length > 2000 || !Array.isArray(value.operations) || value.operations.length > 64) throw new Error('Invalid EDL header.');
  for (const op of value.operations) {
    if (!op || typeof op.type !== 'string' || !Object.hasOwn(fields, op.type)) throw new Error('Unsupported EDL operation.');
    object(op, ['type', ...fields[op.type as Operation['type']]]);
    for (const key of ['clipId', 'trackId']) if (key in op && !uuid(op[key])) throw new Error('Invalid operation ID.');
    for (const key of ['start', 'end', 'sourceIn', 'duration', 'frame', 'frames']) if (key in op && !uint(op[key])) throw new Error('Frames must be unsigned integers, never clamped.');
    if (op.type === 'addTrack' && (typeof op.name !== 'string' || !['video', 'audio', 'caption'].includes(op.kind as string))) throw new Error('Invalid track.');
    if ('clip' in op) {
      object(op.clip, ['id', 'assetId', 'linkId', 'start', 'sourceIn', 'duration'], ['caption', 'effects']);
      if (!uuid(op.clip.id) || !uuid(op.clip.assetId) || (op.clip.linkId !== null && !uuid(op.clip.linkId)) || ![op.clip.start, op.clip.sourceIn, op.clip.duration].every(uint)) throw new Error('Invalid clip.');
      if (op.clip.caption != null) caption(op.clip.caption);
    }
    if (op.type === 'split' && (!Array.isArray(op.rightIds) || !op.rightIds.every(uuid) || (op.rightLinkId !== null && !uuid(op.rightLinkId)))) throw new Error('Invalid split IDs.');
    if (op.type === 'zoom') validateEffects({ zoom: op.keys, lut: null, duck: [] }, 'video', 4294967295);
    if (op.type === 'duck') validateEffects({ zoom: [], lut: null, duck: op.keys }, 'audio', 4294967295);
    if (op.type === 'lut' && !['identity', 'warm', 'cool'].includes(op.preset as string)) throw new Error('Unsupported LUT.');
    if (op.type === 'captionStyle') caption(op.caption);
  }
  return value as unknown as Edl;
}
export function compileEdl(project: Project, value: unknown): Project {
  validateProject(project);
  const edl = parseEdl(value);
  if (edl.projectId !== project.id || edl.baseRevision !== project.revision) throw new Error('Wrong project or stale revision. Generate a new proposal.');
  let next = structuredClone(project); next.schemaVersion = project.schemaVersion >= 6 ? 6 : 5;
  const fresh = (id: string) => { if (!uuid(id) || next.assets.some(a => a.id === id) || next.tracks.some(t => t.id === id || t.clips.some(c => c.id === id || c.linkId === id))) throw new Error('New ID must be a unique UUID v4.'); };
  const targets = (id: string, linked = true) => {
    const source = next.tracks.flatMap(t => t.clips).find(c => c.id === id);
    if (!source) throw new Error('Clip ID not found.');
    const result = next.tracks.flatMap(track => track.clips.filter(c => c.id === id || (linked && source.linkId !== null && c.linkId === source.linkId)).map(clip => ({ track, clip })));
    if (result.some(v => v.track.locked)) throw new Error('Operation affects a locked track.');
    return result;
  };
  for (const op of edl.operations) {
    switch (op.type) {
      case 'addTrack': fresh(op.trackId); next.tracks.push({ id: op.trackId, name: op.name, kind: op.kind, muted: false, solo: false, locked: false, clips: [] }); break;
      case 'insert': case 'broll': case 'sfx': case 'captions': {
        fresh(op.clip.id); const t = next.tracks.find(t => t.id === op.trackId);
        if (!t || t.locked) throw new Error('Missing or locked track.');
        if ((op.type === 'broll' && t.kind !== 'video') || (op.type === 'sfx' && t.kind !== 'audio') || (op.type === 'captions' && t.kind !== 'caption')) throw new Error('Placement requires matching track kind.');
        t.clips.push(structuredClone(op.clip)); break;
      }
      case 'move': for (const { clip } of targets(op.clipId)) clip.start = op.start; break;
      case 'trim': for (const { clip } of targets(op.clipId)) Object.assign(clip, { start: op.start, sourceIn: op.sourceIn, duration: op.duration }); break;
      case 'delete': for (const { track, clip } of targets(op.clipId)) track.clips.splice(track.clips.indexOf(clip), 1); break;
      case 'split': {
        const members = targets(op.clipId);
        if (members.length !== op.rightIds.length || (members.length === 2) !== (op.rightLinkId !== null)) throw new Error('Split needs IDs for every linked member.');
        const ids = [...op.rightIds, ...(op.rightLinkId ? [op.rightLinkId] : [])]; ids.forEach(fresh);
        if (new Set(ids).size !== ids.length) throw new Error('Duplicate split ID.');
        members.forEach(({ track, clip }, i) => { const offset = op.frame - clip.start; if (offset <= 0 || offset >= clip.duration) throw new Error('Split must be inside clip.'); track.clips.push({ ...structuredClone(clip), id: op.rightIds[i], linkId: op.rightLinkId, start: op.frame, sourceIn: clip.sourceIn + sourceOffset(clip, offset), duration: clip.duration - offset }); clip.duration = offset; }); break;
      }
      case 'ripple': next = rippleDelete(next, [{ start: op.start, end: op.end }]); next.revision = project.revision; break;
      case 'jCut': case 'lCut': {
        const members = targets(op.clipId); if (members.length !== 2 || !op.frames) throw new Error('J/L cut requires a linked A/V pair and positive frames.');
        for (const { track, clip } of members) { clip.linkId = null; if (track.kind === 'audio') { if (op.type === 'jCut') { clip.start -= op.frames; clip.sourceIn -= sourceOffset(clip, op.frames); } clip.duration += op.frames; } } break;
      }
      case 'zoom': case 'lut': case 'duck': {
        const { track, clip } = targets(op.clipId, false)[0];
        if (track.kind !== (op.type === 'duck' ? 'audio' : 'video')) throw new Error('Effect requires the matching track kind.');
        clip.effects ??= { zoom: [], lut: null, duck: [] };
        if (op.type === 'zoom') clip.effects.zoom = structuredClone(op.keys);
        if (op.type === 'lut') clip.effects.lut = op.preset;
        if (op.type === 'duck') clip.effects.duck = structuredClone(op.keys); break;
      }
      case 'captionStyle': targets(op.clipId, false)[0].clip.caption = structuredClone(op.caption); break;
    }
    next.tracks.forEach(t => t.clips.sort((a, b) => a.start - b.start));
  }
  next.revision = project.revision + 1;
  return validateProject(next);
}
export function summarize(edl: Edl, project: Project): string[] {
  const label = (id: string) => { const c = project.tracks.flatMap(t => t.clips).find(c => c.id === id); return `${project.assets.find(a => a.id === c?.assetId)?.name ?? 'New clip'} (${id.slice(0, 8)})`; };
  return edl.operations.map(op => {
    switch (op.type) {
      case 'addTrack': return `Add ${op.kind} track “${op.name}”.`;
      case 'insert': case 'broll': case 'sfx': case 'captions': return `${op.type}: place ${project.assets.find(a => a.id === op.clip.assetId)?.name ?? op.clip.assetId} at frame ${op.clip.start}, for ${op.clip.duration} frames.`;
      case 'move': return `Move ${label(op.clipId)} and linked partner to frame ${op.start}.`;
      case 'trim': return `Trim ${label(op.clipId)} and linked partner: timeline ${op.start}, source ${op.sourceIn}, duration ${op.duration} frames.`;
      case 'split': return `Split ${label(op.clipId)} and linked partner at frame ${op.frame}.`;
      case 'delete': return `Delete ${label(op.clipId)} and linked partner (leave a gap).`;
      case 'ripple': return `Remove frames ${op.start}–${op.end} across ALL tracks and close the gap.`;
      case 'jCut': case 'lCut': return `${op.type === 'jCut' ? 'J' : 'L'} cut ${label(op.clipId)}: unlink A/V, extend audio ${op.type === 'jCut' ? 'earlier' : 'later'} by ${op.frames} frames.`;
      case 'zoom': return `Set ${op.keys.length} zoom keyframes on ${label(op.clipId)}.`;
      case 'lut': return `Apply ${op.preset} 3D LUT to ${label(op.clipId)}.`;
      case 'duck': return `Set ${op.keys.length} ducking gain keyframes on ${label(op.clipId)}.`;
      case 'captionStyle': return `Set ${op.caption.preset} captions (${op.caption.words.length} ${op.caption.timing} timings) on ${label(op.clipId)}.`;
    }
  });
}
