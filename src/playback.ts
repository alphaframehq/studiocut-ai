import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import type { Asset, Project } from './project';
import { audible, timelineEnd } from './timeline';
import { native } from './store';
import { gainAt } from './effects';

const attachments = new Map<string, { url: string; file: File }>();
export function attachMedia(asset: Asset, file: File) {
  if (file.size !== asset.sizeBytes || file.name !== asset.name) throw new Error('Choose the original file with the same name and size.');
  const previous = attachments.get(asset.id);
  if (previous) URL.revokeObjectURL(previous.url);
  attachments.set(asset.id, { file, url: URL.createObjectURL(file) });
  window.dispatchEvent(new Event('studiocut-media-attached'));
}
export async function mediaUrl(asset: Asset): Promise<string> {
  if (native) return convertFileSrc(await invoke<string>('playback_source', { id: asset.id }));
  const attachment = attachments.get(asset.id);
  if (!attachment) throw new Error(`Reattach ${asset.name} for browser preview, or open this project in the desktop app.`);
  return attachment.url;
}
export type AudioReader = (asset: Asset, start: number, duration: number, context: AudioContext) => Promise<AudioBuffer>;
const readAudio: AudioReader = async (asset, start, duration, context) => {
  if (native) {
    const bytes = await invoke<number[]>('audio_chunk', { id: asset.id, start, duration });
    const raw = new DataView(Uint8Array.from(bytes).buffer);
    const length = Math.floor(raw.byteLength / 8);
    if (!length) throw new Error('No decoded audio samples.');
    const buffer = context.createBuffer(2, length, 48000);
    for (let channel = 0; channel < 2; channel++) {
      const samples = buffer.getChannelData(channel);
      for (let i = 0; i < length; i++) samples[i] = raw.getFloat32((i * 2 + channel) * 4, true);
    }
    return buffer;
  }
  const attachment = attachments.get(asset.id);
  if (!attachment) throw new Error(`Reattach ${asset.name} for playback.`);
  if (attachment.file.size > 64 * 1024 * 1024) throw new Error('Browser audio preview is limited to 64 MB sources; use the desktop app for larger files.');
  return context.decodeAudioData(await attachment.file.arrayBuffer());
};

export class TimelinePlayer {
  private context: AudioContext;
  private generation = 0;
  private origin = 0;
  private firstFrame = 0;
  private nextTime = 0;
  private nodes = new Set<AudioBufferSourceNode>();
  private timer = 0;
  private raf = 0;
  private loading = false;
  private running = false;
  private browserBuffers = new Map<string, AudioBuffer>();
  constructor(private project: Project, private onFrame: (frame: number) => void, private onStop: (error?: string) => void, private reader: AudioReader = readAudio) {
    this.context = new AudioContext({ sampleRate: 48000 });
  }
  get currentFrame() {
    return this.running ? Math.min(timelineEnd(this.project.tracks), this.firstFrame + Math.max(0, this.context.currentTime - this.origin) * this.project.settings.fps) : this.firstFrame;
  }
  async play(frame: number) {
    this.pause();
    const generation = this.generation;
    this.firstFrame = frame;
    this.nextTime = frame / this.project.settings.fps;
    await this.context.resume();
    const initial = await this.prepare(this.nextTime, Math.min(this.nextTime + 2, timelineEnd(this.project.tracks) / this.project.settings.fps));
    if (generation !== this.generation) return;
    this.origin = this.context.currentTime + 0.1;
    this.running = true;
    this.schedule(initial);
    this.nextTime += 2;
    const tick = () => {
      if (!this.running) return;
      const current = this.currentFrame;
      this.onFrame(current);
      if (current >= timelineEnd(this.project.tracks)) { this.pause(); this.onStop(); return; }
      this.raf = requestAnimationFrame(tick);
    };
    tick();
    this.timer = window.setInterval(() => { void this.fill(generation); }, 100);
  }
  private async prepare(start: number, end: number) {
    const pieces: { buffer: AudioBuffer; clip: import('./timeline').Clip; at: number; offset: number; duration: number }[] = [];
    if (end <= start) return pieces;
    const fps = this.project.settings.fps;
    for (const track of this.project.tracks.filter(t => t.kind === 'audio' && audible(t, this.project.tracks))) {
      for (const clip of track.clips) {
        const at = Math.max(start, clip.start / fps);
        const until = Math.min(end, (clip.start + clip.duration) / fps);
        if (until <= at) continue;
        const asset = this.project.assets.find(a => a.id === clip.assetId)!;
        const source = clip.sourceIn / fps + at - clip.start / fps;
        let buffer = this.browserBuffers.get(asset.id);
        if (!buffer) {
          buffer = await this.reader(asset, source, until - at, this.context);
          if (!native && this.reader === readAudio) {
            this.browserBuffers.set(asset.id, buffer);
            if (this.browserBuffers.size > 2) this.browserBuffers.delete(this.browserBuffers.keys().next().value!);
          }
        }
        pieces.push({ buffer, clip, at, offset: !native && this.reader === readAudio ? source : 0, duration: until - at });
      }
    }
    return pieces;
  }
  private schedule(pieces: Awaited<ReturnType<TimelinePlayer['prepare']>>) {
    for (const piece of pieces) {
      const node = this.context.createBufferSource();
      node.buffer = piece.buffer;
      const gain = this.context.createGain();
      node.connect(gain); gain.connect(this.context.destination);
      const start = this.origin + piece.at - this.firstFrame / this.project.settings.fps;
      const duration = Math.min(piece.duration, piece.buffer.duration - piece.offset);
      const sourceFrame = piece.clip.sourceIn + piece.at * this.project.settings.fps - piece.clip.start;
      const keys = piece.clip.effects?.duck ?? [];
      gain.gain.setValueAtTime(gainAt(keys, sourceFrame), start);
      for (const key of keys) if (key.frame > sourceFrame && key.frame < sourceFrame + duration * this.project.settings.fps) gain.gain.linearRampToValueAtTime(key.gain, start + (key.frame - sourceFrame) / this.project.settings.fps);
      gain.gain.linearRampToValueAtTime(gainAt(keys, sourceFrame + duration * this.project.settings.fps), start + duration);
      node.onended = () => { node.disconnect(); gain.disconnect(); this.nodes.delete(node); };
      this.nodes.add(node);
      node.start(start, piece.offset, duration);
    }
  }
  private async fill(generation: number) {
    if (this.loading || !this.running) return;
    const position = this.currentFrame / this.project.settings.fps;
    if (this.nextTime - position > 1.4 || this.nextTime >= timelineEnd(this.project.tracks) / this.project.settings.fps) return;
    this.loading = true;
    try {
      const at = this.nextTime;
      const pieces = await this.prepare(at, Math.min(at + 2, timelineEnd(this.project.tracks) / this.project.settings.fps));
      if (generation !== this.generation) return;
      if (this.currentFrame / this.project.settings.fps >= at) throw new Error('Audio decoder fell behind. Playback paused to preserve synchronization.');
      this.schedule(pieces);
      this.nextTime += 2;
    } catch (error) {
      if (generation === this.generation) { this.pause(); this.onStop(String(error)); }
    } finally { this.loading = false; }
  }
  pause() {
    this.firstFrame = this.currentFrame;
    this.running = false;
    this.generation++;
    clearInterval(this.timer);
    cancelAnimationFrame(this.raf);
    for (const node of this.nodes) { node.stop(); node.disconnect(); }
    this.nodes.clear();
  }
  dispose() { this.pause(); this.browserBuffers.clear(); void this.context.close(); }
}
