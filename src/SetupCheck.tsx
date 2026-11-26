import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { native } from './store';
import { getWhisperBinary, getWhisperModel } from './preferences';

type Setup = { ffmpeg: boolean; ffprobe: boolean; whisperCli: boolean; whisperModel: boolean; detail: string };

/** First-run setup check (desktop only): are ffmpeg/ffprobe/whisper there? */
export function SetupCheck() {
  const [status, setStatus] = useState<Setup | null>(null);
  useEffect(() => {
    if (!native) return;
    let active = true;
    void invoke<Setup>('setup_status', { binary: getWhisperBinary(), model: getWhisperModel() })
      .then(s => { if (active) setStatus(s); })
      .catch(() => { if (active) setStatus(null); });
    return () => { active = false; };
  }, []);
  if (!native || !status) return null;
  const rows: [string, boolean, string][] = [
    ['FFmpeg', status.ffmpeg, 'Needed for import, preview audio and export.'],
    ['FFprobe', status.ffprobe, 'Needed for media metadata.'],
    ['whisper-cli', status.whisperCli, 'Needed for local transcription.'],
    ['Whisper model file', status.whisperModel, 'Set the GGML .bin path in Settings → Transcription.'],
  ];
  const allOk = rows.every(([, ok]) => ok);
  return <div className="setup-check" aria-label="First-run setup check">
    <h3>SETUP CHECK · {allOk ? 'READY' : 'ACTION NEEDED'}</h3>
    <ul>{rows.map(([label, ok, hint]) => <li key={label} className={ok ? '' : 'bad'}>{ok ? '✓' : '✗'} {label}{!ok && <small>{hint} See SETUP_BINARIES.md in the repo.</small>}</li>)}</ul>
    <p>{status.detail}</p>
  </div>;
}
