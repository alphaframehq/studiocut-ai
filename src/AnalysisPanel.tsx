import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { native, useProject } from './store';
import { getCachedTranscriptionModels, getStyleDna, getTranscriptionModel, getWhisperBinary, getWhisperModel, setCachedTranscriptionModels, setTranscriptionModel, setWhisperBinary, setWhisperModel, type ListedModel } from './preferences';
import type { CaptionPreset } from './timeline';
import type { Cut, RoughCut, Scene, SceneCut, Transcript } from './analysis';

type Preview = { thumbnail: number[] | null; peaks: number[] | null };

const defaultModelFor = (provider: string) => provider === 'groq' ? 'whisper-large-v3' : provider === 'openrouter' ? 'openai/whisper-1' : 'whisper-1';

type Result = { projectId: string; revision: number; clipId: string; cuts?: Cut[]; transcript?: Transcript };
type CutResult = { projectId: string; revision: number; cuts: Cut[] };
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
  const [preset, setPreset] = useState<CaptionPreset>(() => getStyleDna().captionPreset);
  const [status, setStatus] = useState('');
  const [failure, setFailure] = useState('');
  const [result, setResult] = useState<Result | null>(null);
  const [enabled, setEnabled] = useState<number[]>([]);
  // Phase 3: scenes (heuristic), filler cuts, per-second energy, rough cut.
  const [sceneThreshold, setSceneThreshold] = useState(0.4);
  const [scenes, setScenes] = useState<Scene[] | null>(null);
  const [fillerResult, setFillerResult] = useState<CutResult | null>(null);
  const [fillerEnabled, setFillerEnabled] = useState<number[]>([]);
  const [lowEnergy, setLowEnergy] = useState<Cut[] | null>(null);
  const [energyNote, setEnergyNote] = useState('');
  const [roughResult, setRoughResult] = useState<(RoughCut & { projectId: string; revision: number }) | null>(null);
  const [roughEnabled, setRoughEnabled] = useState<number[]>([]);
  useEffect(() => { setResult(null); setEnabled([]); setConsent(false); setScenes(null); setFillerResult(null); setFillerEnabled([]); setLowEnergy(null); setEnergyNote(''); setRoughResult(null); setRoughEnabled([]); }, [project?.id, project?.revision, selected]);
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
  const fps = project?.settings.fps ?? 30;
  // Scenes are clip-relative seconds; convert to timeline-frame cuts.
  const sceneCuts: SceneCut[] = scenes && clip ? scenes.map(s => ({ start: clip.start + Math.round(s.start * fps), end: Math.min(clip.start + clip.duration, clip.start + Math.round(s.end * fps)), score: s.score })) : [];
  const bestScene = scenes && scenes.length ? scenes.reduce((best, s) => (s.score ?? -1) > (best.score ?? -1) ? s : best) : null;
  const seekScene = (s: Scene) => { if (clip) seek(clip.start + Math.round(s.start * fps)); };
  async function run(kind: 'silence' | 'transcribe' | 'status' | 'scenes' | 'fillers' | 'energy' | 'rough') {
    if (!native || !project || busy) return;
    setFailure(''); setStatus(kind === 'silence' ? 'Analyzing 50 ms RMS windows…' : kind === 'status' ? 'Checking local installation…' : kind === 'scenes' ? 'Detecting scenes (heuristic)…' : kind === 'fillers' ? 'Finding filler words…' : kind === 'energy' ? 'Reading waveform peaks…' : kind === 'rough' ? 'Combining your analysis results…' : 'Transcribing selected audio…');
    useProject.setState({ busy: true, playing: false });
    const snapshot = { projectId: project.id, revision: project.revision, clipId: selected };
    try {
      if (kind === 'status') {
        setStatus(await invoke<string>('transcription_status', { binary, model: localModel }));
      } else if (kind === 'silence') {
        const cuts = await invoke<Cut[]>('detect_silence', { ...snapshot, settings: { thresholdDb: threshold, minSilenceMs: minimum, paddingMs: padding } });
        setResult({ ...snapshot, clipId: selected!, cuts }); setEnabled(cuts.map((_, i) => i));
        setStatus(`${cuts.length} silence ranges detected. Review before applying.`);
      } else if (kind === 'scenes') {
        const found = await invoke<Scene[]>('detect_scenes', { ...snapshot, threshold: sceneThreshold });
        setScenes(found);
        setStatus(`${found.length} scene${found.length === 1 ? '' : 's'} detected (heuristic — brightness/texture score, not ML).`);
      } else if (kind === 'fillers') {
        if (!current || !result!.transcript) throw new Error('Transcribe the clip first, then find fillers.');
        const cuts = await invoke<Cut[]>('detect_filler_cuts', { ...snapshot, words: result!.transcript.items });
        setFillerResult({ projectId: project.id, revision: project.revision, cuts }); setFillerEnabled(cuts.map((_, i) => i));
        setStatus(`${cuts.length} filler-word ranges detected. Review before applying.`);
      } else if (kind === 'energy') {
        // Frontend only: media_preview peaks are up to 2000 amplitude
        // buckets (0–1) spread evenly over the whole source file, so a
        // bucket's time is its index / 2000 × asset duration.
        if (!asset || asset.audioStream == null || asset.duration == null) throw new Error('This asset has no audio waveform to measure.');
        const preview = await invoke<Preview>('media_preview', { id: asset.id });
        if (!preview.peaks || !preview.peaks.length) throw new Error('Waveform peaks unavailable for this asset — reimport it to compute energy.');
        const clipSeconds = clip!.duration / fps;
        const sourceIn = clip!.sourceIn / fps;
        const perSecond: number[] = [];
        for (let s = 0; s < Math.floor(clipSeconds); s++) {
          const bucket = Math.min(preview.peaks.length - 1, Math.floor((sourceIn + s + 0.5) / asset.duration * preview.peaks.length));
          perSecond.push(preview.peaks[bucket]);
        }
        const max = Math.max(...perSecond, 0);
        if (max <= 0) { setLowEnergy([]); setEnergyNote('This clip is silent throughout — no energy to measure.'); }
        else {
          const stretches: Cut[] = [];
          let runStart: number | null = null;
          for (let s = 0; s <= perSecond.length; s++) {
            const low = s < perSecond.length && perSecond[s] < 0.2 * max;
            if (low && runStart === null) runStart = s;
            if (!low && runStart !== null) {
              if (s - runStart >= 1) stretches.push({ start: clip!.start + runStart * fps, end: Math.min(clip!.start + clip!.duration, clip!.start + s * fps) });
              runStart = null;
            }
          }
          setLowEnergy(stretches);
          setEnergyNote(stretches.length ? `${stretches.length} stretch${stretches.length === 1 ? '' : 'es'} under 20% energy — consider cutting.` : 'No low-energy stretches — pacing looks even across this clip.');
        }
        setStatus('Energy measured from the waveform peaks.');
      } else if (kind === 'rough') {
        if (!current || !result!.cuts) throw new Error('Detect silence first — the rough cut combines results you already ran.');
        const rough = await invoke<RoughCut>('rough_cut', { projectId: project.id, revision: project.revision, silence: result!.cuts.filter((_, i) => enabled.includes(i)), fillers: fillerResult ? fillerResult.cuts.filter((_, i) => fillerEnabled.includes(i)) : [], lowEnergy: lowEnergy ?? [], scenes: sceneCuts });
        setRoughResult({ ...rough, projectId: project.id, revision: project.revision }); setRoughEnabled(rough.cuts.map((_, i) => i));
        setStatus(`${rough.cuts.length} rough-cut ranges ready. The hook is protected and never cut.`);
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
    </div>
    <fieldset disabled={busy}><legend>04 · Scenes (heuristic)</legend>
      <p>Heuristic scene detection: FFmpeg scene-change filter plus a luma brightness/texture score. Not ML, no face detection — a high score only means the shot looks normally exposed and textured.</p>
      <div className="analysis-fields"><label>Scene threshold · 0.1–0.9<input type="number" min="0.1" max="0.9" step="0.1" value={sceneThreshold} onChange={e => setSceneThreshold(Number(e.target.value))} /></label></div>
      <button disabled={!native || !clip || asset?.videoStream == null || busy} onClick={() => void run('scenes')}>Detect scenes</button>
      {scenes && <div className="cut-review"><h3>{scenes.length} scene{scenes.length === 1 ? '' : 's'} · first 300 s of the clip</h3>{scenes.length === 0 && <p>No scene changes found at this threshold.</p>}{scenes.map((s, i) => <div className="cut-range" key={i}><span>{s.start.toFixed(2)}–{s.end.toFixed(2)}s · score {s.score === null ? '—' : `${s.score}/100`}</span><button onClick={() => seekScene(s)}>Seek</button></div>)}{bestScene && bestScene.score !== null && <button onClick={() => seekScene(bestScene)}>Jump to best scene · {bestScene.score}/100</button>}</div>}
    </fieldset>
    <fieldset disabled={busy}><legend>05 · Filler words & energy</legend>
      <p>Find fillers runs on the transcript above (um, uh, like, “you know”, plus Hindi/Hinglish matlab, aah, arre, woh). Energy is measured from the waveform peaks only — no audio is invented.</p>
      <button disabled={!current || !result?.transcript} onClick={() => void run('fillers')}>Find fillers</button>
      {fillerResult && <div className="cut-review"><h3>Review filler cuts</h3>{fillerResult.cuts.length === 0 && <p>No filler words found in this transcript.</p>}{fillerResult.cuts.map((cut, i) => <div className="cut-range" key={i}><label><input type="checkbox" checked={fillerEnabled.includes(i)} onChange={e => setFillerEnabled(e.target.checked ? [...fillerEnabled, i] : fillerEnabled.filter(n => n !== i))} />{(cut.start / fps).toFixed(2)}–{(cut.end / fps).toFixed(2)}s</label><button onClick={() => seek(Math.max(0, cut.start - fps))}>Preview cut {i + 1}</button></div>)}<button disabled={!fillerEnabled.length} onClick={() => void applyAnalysis({ type: 'cuts', cuts: fillerResult.cuts.filter((_, i) => fillerEnabled.includes(i)) }, fillerResult.projectId, fillerResult.revision)}>Apply {fillerEnabled.length} cuts · one undo</button></div>}
      <button disabled={!eligible} onClick={() => void run('energy')}>Check energy &amp; pacing</button>
      {energyNote && <p role="status">{energyNote}</p>}
      {lowEnergy && lowEnergy.length > 0 && <div className="cut-review"><h3>Low-energy stretches (under 20% of peak)</h3>{lowEnergy.map((cut, i) => <div className="cut-range" key={i}><span>{(cut.start / fps).toFixed(2)}–{(cut.end / fps).toFixed(2)}s</span><button onClick={() => seek(cut.start)}>Seek</button></div>)}</div>}
      {lowEnergy && lowEnergy.length === 0 && !energyNote.includes('silent') && <p>No low-energy stretches found.</p>}
    </fieldset>
    <fieldset disabled={busy}><legend>06 · Auto rough cut</legend>
      <p>Hook-first: combines the silence, filler and low-energy cuts you already ran — scene scores only pick the hook. It does not re-run transcription or silence detection. The opening hook (first 5 s of the best-scored scene, or the timeline start) is protected and never cut.</p>
      <button disabled={!current || !result?.cuts || !result.cuts.length} onClick={() => void run('rough')}>Auto rough cut</button>
      {roughResult && <div className="cut-review"><h3>Review rough cut</h3>{roughResult.hook && <p>Hook protected: {(roughResult.hook.start / fps).toFixed(2)}–{(roughResult.hook.end / fps).toFixed(2)}s — this range is never cut.</p>}{roughResult.cuts.length === 0 && <p>Nothing left to cut after protecting the hook.</p>}{roughResult.cuts.map((cut, i) => <div className="cut-range" key={i}><label><input type="checkbox" checked={roughEnabled.includes(i)} onChange={e => setRoughEnabled(e.target.checked ? [...roughEnabled, i] : roughEnabled.filter(n => n !== i))} />{(cut.start / fps).toFixed(2)}–{(cut.end / fps).toFixed(2)}s</label><button onClick={() => seek(Math.max(0, cut.start - fps))}>Preview cut {i + 1}</button></div>)}<button disabled={!roughEnabled.length} onClick={() => void applyAnalysis({ type: 'cuts', cuts: roughResult.cuts.filter((_, i) => roughEnabled.includes(i)) }, roughResult.projectId, roughResult.revision)}>Apply {roughEnabled.length} cuts · one undo</button></div>}
    </fieldset>
    <p role="status">{status}</p>{failure && <p role="alert" className="error">{failure}</p>}
  </section>;
}
