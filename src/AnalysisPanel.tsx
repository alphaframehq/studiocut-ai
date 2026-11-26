import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { native, useProject } from './store';
import { getCachedTranscriptionModels, getTranscriptionModel, getWhisperBinary, getWhisperModel, setCachedTranscriptionModels, setTranscriptionModel, setWhisperBinary, setWhisperModel, type ListedModel } from './preferences';
import type { CaptionPreset } from './timeline';
import type { Cut, Transcript } from './analysis';

const defaultModelFor = (provider: string) => provider === 'groq' ? 'whisper-large-v3' : provider === 'openrouter' ? 'openai/whisper-1' : 'whisper-1';

type Result = { projectId: string; revision: number; clipId: string; cuts?: Cut[]; transcript?: Transcript };
export function AnalysisPanel() {
  const { project, selected, busy, applyAnalysis, seek } = useProject();
  const [threshold, setThreshold] = useState(-38);
  const [minimum, setMinimum] = useState(400);
  const [padding, setPadding] = useState(80);
  const [provider, setProvider] = useState('local');
  const [binary, setBinaryState] = useState(getWhisperBinary);
  const [localModel, setLocalModelState] = useState(getWhisperModel);
  const [model, setModel] = useState('whisper-1');
  const [models, setModels] = useState<ListedModel[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [modelsError, setModelsError] = useState('');
  const [modelsHint, setModelsHint] = useState('');
  const [modelMode, setModelMode] = useState<'list' | 'custom'>('list');
  const [customModel, setCustomModel] = useState('');
  const [localFiles, setLocalFiles] = useState<string[]>([]);
  const [endpoint, setEndpoint] = useState('');
  const [key, setKey] = useState('');
  const [language, setLanguage] = useState('');
  const [consent, setConsent] = useState(false);
  const [preset, setPreset] = useState<CaptionPreset>('hormozi');
  const [status, setStatus] = useState('');
  const [failure, setFailure] = useState('');
  const [result, setResult] = useState<Result | null>(null);
  const [enabled, setEnabled] = useState<number[]>([]);
  useEffect(() => { setResult(null); setEnabled([]); setConsent(false); }, [project?.id, project?.revision, selected]);
  // Fetch (or load from the 1h cache) the transcription-model list for a
  // provider. Local scans the folder next to the configured .bin file.
  async function refreshModels(p: string, keyValue: string, endpointValue: string, localModelValue: string, bypassCache: boolean) {
    if (!native) return;
    setModelsError(''); setModelsHint('');
    if (p === 'local') {
      try {
        const found = await invoke<ListedModel[]>('list_transcription_models', { request: { provider: 'local', apiKey: '', endpoint: '', modelPath: localModelValue } });
        setLocalFiles(found.map(f => f.path).filter((f): f is string => !!f));
        setModelsHint(found.length ? `${found.length} model file${found.length === 1 ? '' : 's'} found in this folder.` : 'No .bin files in this folder.');
      } catch (error) { setModelsError(String(error)); }
      return;
    }
    if (!keyValue) { setModels([]); setModelsHint('Enter an API key, then refresh the model list.'); return; }
    if (!bypassCache) {
      const cached = getCachedTranscriptionModels(p);
      if (cached) { applyModels(cached.models, model); return; }
    }
    setModelsLoading(true);
    try {
      const found = await invoke<ListedModel[]>('list_transcription_models', { request: { provider: p, apiKey: keyValue, endpoint: endpointValue, modelPath: '' } });
      setCachedTranscriptionModels(p, found);
      applyModels(found, model);
    } catch (error) { setModelsError(String(error)); }
    finally { setModelsLoading(false); }
  }
  // Keep the currently selected model visible even if the provider no
  // longer lists it (e.g. a custom or retired id).
  function applyModels(found: ListedModel[], current: string) {
    const list = current && !found.some(m => m.id === current) ? [{ id: current, recommended: false, path: null }, ...found] : found;
    setModels(list);
    setModelsHint(list.length ? `${list.length} transcription model${list.length === 1 ? '' : 's'} · ★ timestamps verified` : 'No transcription models returned for this provider.');
  }
  function changeProvider(p: string) {
    setProvider(p); setKey(''); setConsent(false); setStatus(''); setModels([]); setModelsError(''); setModelsHint(''); setModelMode('list'); setCustomModel('');
    const next = getTranscriptionModel(p) ?? defaultModelFor(p);
    setModel(next);
    void refreshModels(p, '', endpoint, localModel, false);
  }
  // Scan the local model folder once on mount (desktop only).
  useEffect(() => { if (native) void refreshModels('local', '', '', getWhisperModel(), false); }, []);
  const effectiveModel = provider === 'local' ? localModel : modelMode === 'custom' ? customModel : model;
  const clip = project?.tracks.filter(t => t.kind !== 'caption').flatMap(t => t.clips).find(c => c.id === selected);
  const asset = project?.assets.find(a => a.id === clip?.assetId);
  const eligible = native && clip && asset?.audioStream != null && !busy;
  const current = result && result.projectId === project?.id && result.revision === project.revision && result.clipId === selected;
  async function run(kind: 'silence' | 'transcribe' | 'status') {
    if (!native || !project || busy) return;
    setFailure(''); setStatus(kind === 'silence' ? 'Analyzing 50 ms RMS windows…' : kind === 'status' ? 'Checking local installation…' : 'Transcribing selected audio…');
    useProject.setState({ busy: true, playing: false });
    const snapshot = { projectId: project.id, revision: project.revision, clipId: selected };
    try {
      if (kind === 'status') {
        setStatus(await invoke<string>('transcription_status', { binary, model: localModel }));
      } else if (kind === 'silence') {
        const cuts = await invoke<Cut[]>('detect_silence', { ...snapshot, settings: { thresholdDb: threshold, minSilenceMs: minimum, paddingMs: padding } });
        setResult({ ...snapshot, clipId: selected!, cuts }); setEnabled(cuts.map((_, i) => i));
        setStatus(`${cuts.length} silence ranges detected. Review before applying.`);
      } else {
        const transcript = await invoke<Transcript>('transcribe_clip', { ...snapshot, options: { provider, binary, model: effectiveModel, apiKey: key, endpoint, language, allowUpload: consent } });
        setResult({ ...snapshot, clipId: selected!, transcript });
        setStatus(transcript.timing === 'word' ? 'Word timestamps ready for active-word captions.' : 'Segment timestamps only: whole phrases highlight; word timings were not invented.');
      }
    } catch (error) { setFailure(String(error)); setStatus(''); }
    finally { useProject.setState({ busy: false }); }
  }
  return <section className="analysis-panel" aria-label="Silence and captions">
    <div className="analysis-title"><h2>Silence & captions</h2><span>{asset ? `Selected: ${asset.name}` : 'Select an audio or video clip on the timeline'}</span></div>
    {!native && <p className="analysis-notice">Analysis unavailable in browser preview. Use the desktop app for Rust DSP, whisper-cli, and secure BYOK transcription. Saved caption clips still play here.</p>}
    <div className="analysis-columns">
      <fieldset disabled={busy}><legend>01 · Remove dead air</legend>
        <p>50 ms stereo RMS windows. Quiet audio is not always silence—review each range. Cuts ripple all tracks, not just the selected clip.</p>
        <div className="analysis-fields"><label>Threshold · dBFS<input type="number" min="-96" max="0" value={threshold} onChange={e => setThreshold(Number(e.target.value))} /></label><label>Min silence · ms<input type="number" min="50" max="10000" value={minimum} onChange={e => setMinimum(Number(e.target.value))} /></label><label>Speech padding · ms<input type="number" min="0" max="2000" value={padding} onChange={e => setPadding(Number(e.target.value))} /></label></div>
        <button disabled={!eligible} onClick={() => void run('silence')}>Detect silence</button>
        {current && result.cuts && <div className="cut-review"><h3>Review detected cuts</h3>{result.cuts.length === 0 && <p>No silence meets these settings.</p>}{result.cuts.map((cut, i) => <div className="cut-range" key={i}><label><input type="checkbox" checked={enabled.includes(i)} onChange={e => setEnabled(e.target.checked ? [...enabled, i] : enabled.filter(n => n !== i))} />{(cut.start / project!.settings.fps).toFixed(2)}–{(cut.end / project!.settings.fps).toFixed(2)}s</label><button onClick={() => seek(Math.max(0, cut.start - project!.settings.fps))}>Preview cut {i + 1}</button></div>)}<button disabled={!enabled.length} onClick={() => void applyAnalysis({ type: 'cuts', cuts: result.cuts!.filter((_, i) => enabled.includes(i)) }, result.projectId, result.revision)}>Apply {enabled.length} cuts · one undo</button></div>}
      </fieldset>
      <fieldset disabled={busy}><legend>02 · Transcribe locally or BYOK</legend>
        <label>Transcription provider<select value={provider} onChange={e => changeProvider(e.target.value)}><option value="local">Local · whisper.cpp</option><option value="groq">Groq</option><option value="openai">OpenAI</option><option value="openrouter">OpenRouter</option><option value="openaicompatible">OpenAI-compatible</option></select></label>
        {provider === 'local' ? <><label>whisper-cli executable<input value={binary} onChange={e => { setBinaryState(e.target.value); setWhisperBinary(e.target.value); }} spellCheck={false} /></label><label>Local model path<input list="whisper-local-files" value={localModel} onChange={e => { setLocalModelState(e.target.value); setWhisperModel(e.target.value); }} placeholder="/path/to/ggml-base.en.bin" spellCheck={false} /></label><datalist id="whisper-local-files">{localFiles.map(f => <option key={f} value={f} />)}</datalist><button disabled={!native} onClick={() => void refreshModels('local', '', '', localModel, true)}>Scan folder again</button>{modelsHint && <p>{modelsHint}</p>}{modelsError && <p role="alert" className="error">{modelsError}</p>}<button disabled={!native} onClick={() => void run('status')}>Check local availability</button>{!localModel && <p>Local transcription unavailable until you select an installed model. No automatic downloads.</p>}</> : <><p>The selected audio is sent as a 16 kHz mono WAV. Keys are session-only and never saved in project files.</p><label>API key<input type="password" autoComplete="off" value={key} onChange={e => setKey(e.target.value)} /></label><label>Transcription model<select value={modelMode === 'custom' ? '__custom' : model} onChange={e => { if (e.target.value === '__custom') { setModelMode('custom'); } else { setModelMode('list'); setModel(e.target.value); setTranscriptionModel(provider, e.target.value); } }}>{models.map(m => <option key={m.id} value={m.id}>{m.recommended ? `★ ${m.id}` : `${m.id} · timestamps not verified`}</option>)}<option value="__custom">Custom model…</option></select></label>{modelMode === 'custom' && <label>Custom model id<input value={customModel} onChange={e => { setCustomModel(e.target.value); setTranscriptionModel(provider, e.target.value); }} placeholder="e.g. whisper-large-v3" spellCheck={false} /></label>}<button disabled={!key || modelsLoading} onClick={() => void refreshModels(provider, key, endpoint, localModel, true)}>{modelsLoading ? 'Loading models…' : 'Refresh models'}</button>{modelsHint && <p>{modelsHint}</p>}{modelsError && <p role="alert" className="error">{modelsError}</p>}{provider === 'openaicompatible' && <label>Full transcription endpoint<input type="url" value={endpoint} onChange={e => setEndpoint(e.target.value)} placeholder="https://host/v1/audio/transcriptions" /></label>}<label className="upload-consent"><input type="checkbox" checked={consent} onChange={e => setConsent(e.target.checked)} />I allow uploading this selected audio to {provider}.</label><p>Provider must return word or segment timestamps with verbose_json. Unsupported models produce an error, not estimated timings.</p></>}
        <label>Language · optional<input value={language} onChange={e => setLanguage(e.target.value)} placeholder="Auto-detect (or en)" maxLength={8} /></label>
        <button disabled={!eligible || (provider === 'local' ? !localModel : !consent || !key || !effectiveModel)} onClick={() => void run('transcribe')}>Transcribe selected clip</button><p>Maximum 10 minutes per selection. Split longer clips first.</p>
      </fieldset>
      <fieldset disabled={busy}><legend>03 · Kinetic captions</legend>
        <label>Caption preset<select value={preset} onChange={e => setPreset(e.target.value as CaptionPreset)}><option value="hormozi">Hormozi · bold punch</option><option value="minimal">Minimal · clean type</option><option value="documentary">Documentary · lower third</option><option value="neon">Neon · electric glow</option></select></label>
        <p>Captions follow the preview’s audio-master clock. Trim, move, split, mute, and undo them like other clips.</p>
        {current && result.transcript && <><div className="transcript-review">{result.transcript.items.map((item, i) => <button key={i} onClick={() => seek(clip!.start + Math.round(item.start * project!.settings.fps))} title={`${item.start.toFixed(2)}–${item.end.toFixed(2)}s`}>{item.text}</button>)}</div><button onClick={() => void applyAnalysis({ type: 'captions', clipId: result.clipId, transcript: result.transcript!, preset }, result.projectId, result.revision)}>Add {preset} caption clips</button></>}
      </fieldset>
    </div><p role="status">{status}</p>{failure && <p role="alert" className="error">{failure}</p>}
  </section>;
}
