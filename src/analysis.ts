import { validateProject, type Project } from './project';
import { newId, sourceOffset, timelineEnd, type CaptionPreset, type CaptionWord, type Track } from './timeline';

export type Cut = { start: number; end: number };
export type Transcript = { timing: 'word' | 'segment'; items: { text: string; start: number; end: number }[] };

export function rippleDelete(project: Project, cuts: Cut[]): Project {
  const end = timelineEnd(project.tracks);
  if (!cuts.length || cuts.length > 2000 || cuts.some((c, i) => !Number.isSafeInteger(c.start) || !Number.isSafeInteger(c.end) || c.start < 0 || c.start >= c.end || c.end > end || (i > 0 && c.start < cuts[i - 1].end))) throw new Error('Cuts must be sorted, non-overlapping, and within the timeline.');
  if (project.tracks.some(t => t.locked && t.clips.some(c => c.start + c.duration > cuts[0].start))) throw new Error('Unlock every affected track before ripple deleting.');
  const shift = (frame: number) => cuts.reduce((n, c) => n + Math.max(0, Math.min(frame - c.start, c.end - c.start)), 0);
  const tracks = structuredClone(project.tracks);
  const links = new Map<string, string>();
  for (const track of tracks) {
    track.clips = track.clips.flatMap(clip => {
      let cursor = clip.start;
      const end = clip.start + clip.duration;
      const pieces: [number, number][] = [];
      for (const cut of cuts) {
        if (cut.end <= cursor || cut.start >= end) continue;
        if (cut.start > cursor) pieces.push([cursor, cut.start]);
        cursor = Math.min(cut.end, end);
      }
      if (cursor < end) pieces.push([cursor, end]);
      return pieces.map(([a, b], i) => {
        let linkId: string | null = null;
        if (clip.linkId) {
          const key = `${clip.linkId}:${a}:${b}`;
          if (!links.has(key)) links.set(key, newId());
          linkId = links.get(key)!;
        }
        return { ...clip, id: i === 0 ? clip.id : newId(), linkId, start: a - shift(a), sourceIn: clip.sourceIn + sourceOffset(clip, a - clip.start), duration: b - a };
      });
    });
  }
  return validateProject({ ...project, revision: project.revision + 1, tracks });
}

export function addCaptions(project: Project, clipId: string, transcript: Transcript, preset: CaptionPreset): Project {
  const source = project.tracks.filter(t => t.kind !== 'caption').flatMap(t => t.clips).find(c => c.id === clipId);
  if (!source) throw new Error('Select a media clip first.');
  if ((source.effects?.finishing?.speed ?? 1) !== 1) throw new Error('Transcribe original-speed media before retiming.');
  const fps = project.settings.fps;
  if (!['word', 'segment'].includes(transcript.timing) || !transcript.items.length || transcript.items.length > 12000) throw new Error('No usable timestamped speech returned.');
  let previous = 0;
  const words: CaptionWord[] = [];
  for (const item of transcript.items) {
    if (!item.text.trim() || item.text.length > 500 || !Number.isFinite(item.start) || !Number.isFinite(item.end) || item.start < previous || item.end <= item.start || item.end > source.duration / fps + 0.05) throw new Error('Invalid transcript timestamps.');
    previous = item.end;
    const start = Math.round(item.start * fps);
    const end = Math.min(source.duration, Math.round(item.end * fps));
    if (end > start) words.push({ text: item.text, start: source.sourceIn + start, end: source.sourceIn + end });
  }
  const track: Track = { id: newId(), name: 'Captions', kind: 'caption', muted: false, solo: false, locked: false, clips: [] };
  const size = transcript.timing === 'segment' ? 1 : preset === 'documentary' ? 10 : 4;
  for (let i = 0; i < words.length; i += size) {
    const group = words.slice(i, i + size);
    const start = group[0].start;
    const end = group[group.length - 1].end;
    track.clips.push({ id: newId(), assetId: source.assetId, linkId: null, start: source.start + start - source.sourceIn, sourceIn: start, duration: end - start, caption: { preset, timing: transcript.timing, words: group } });
  }
  if (!track.clips.length) throw new Error('No speech spans at least one project frame.');
  return validateProject({ ...project, schemaVersion: project.schemaVersion >= 6 ? 6 : project.schemaVersion >= 5 ? 5 : 4, revision: project.revision + 1, tracks: [...project.tracks, track] });
}
