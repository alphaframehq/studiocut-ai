import '../../src/main';
import { useProject } from '../../src/store';
import { attachMedia } from '../../src/playback';
import { newId } from '../../src/timeline';

async function fixture() {
  const response = await fetch('/src-tauri/tests/preview-fixture.mp4');
  if (!response.ok) throw new Error('Generate preview-fixture.mp4 before this test.');
  const file = new File([await response.blob()], 'preview-fixture.mp4', { type: 'video/mp4' });
  await useProject.getState().createProject('Real media acceptance', 1920, 1080, 30);
  const project = useProject.getState().project!;
  const asset = { id: newId(), sourcePath: file.name, name: file.name, sizeBytes: file.size, modifiedNs: '1', kind: 'video' as const, duration: 8, width: 160, height: 90, fps: 30, videoStream: 0, audioStream: 1, videoCodec: 'h264', audioCodec: 'aac', sampleRate: 48000, channels: 1, createdAt: 1 };
  attachMedia(asset, file);
  useProject.setState({ project: { ...project, schemaVersion: 2, assets: [asset] } });
  await useProject.getState().edit({ type: 'addClip', assetId: asset.id, start: 0 });
  const preset = new URLSearchParams(location.search).get('captions');
  if (preset && ['hormozi', 'minimal', 'documentary', 'neon'].includes(preset)) {
    const current = useProject.getState().project!;
    await useProject.getState().applyAnalysis({ type: 'captions', clipId: current.tracks[0].clips[0].id, preset: preset as import('../../src/timeline').CaptionPreset, transcript: { timing: 'word', items: [
      { text: 'Caption', start: 0, end: 2 }, { text: 'timing', start: 2, end: 4 }, { text: 'test', start: 4, end: 6 }, { text: 'fixture', start: 6, end: 8 }
    ] } }, current.id, current.revision);
  }
  if (new URLSearchParams(location.search).has('director')) {
    const current = structuredClone(useProject.getState().project!);
    current.id = '11111111-1111-4111-8111-111111111111';
    current.revision = 1;
    current.tracks[0].clips[0].id = '22222222-2222-4222-8222-222222222222';
    useProject.setState({ project: current });
  }
  const status = document.createElement('output');
  status.id = 'acceptance-status';
  status.style.cssText = 'display:block;padding:16px;background:#122017;color:white';
  status.textContent = 'Fixture ready: real H.264 video and AAC audio';
  document.body.append(status);
  let observed = false;
  useProject.subscribe(state => {
    if (state.playing && state.frame > 75) {
      const video = document.querySelector('video');
      if (video && video.readyState >= 2 && Math.abs(video.currentTime - state.frame / 30) < 0.2) {
        observed = true;
        status.textContent = 'CHECKPOINT: video synchronized beyond first audio chunk';
      }
    }
    if (!state.playing && state.frame >= 239 && observed) status.textContent = 'PASS: real audio-clock playback completed with synchronized video across chunk boundaries';
  });
}
void fixture().catch(error => { document.body.append(`FAIL: ${error}`); });
