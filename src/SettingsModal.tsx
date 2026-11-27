import { useEffect, useRef, useState } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { X } from 'lucide-react';
import { DEFAULT_STYLE_DNA, SUPPORT_URL, getEncoderPreference, getStyleDna, getWhisperBinary, getWhisperModel, resetDirectorFeedback, setEncoderPreference, setStyleDna, setWhisperBinary, setWhisperModel, type ListedModel, type StyleDNA } from './preferences';

type Provider = 'gemini' | 'openai' | 'claude' | 'groq' | 'openrouter' | 'compatible' | 'ollama' | 'bedrock' | 'vertex';
type LicenseStatus = { fingerprint: string; mode: string; licensed: boolean; license: { name: string; email: string; expiresAt: number | null } | null };

const providers: { id: Provider; label: string; defaultEndpoint: string; defaultModel: string; endpointLabel?: string }[] = [
  { id: 'gemini', label: 'Gemini', defaultEndpoint: 'https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent', defaultModel: 'gemini-2.0-flash' },
  { id: 'openai', label: 'OpenAI', defaultEndpoint: 'https://api.openai.com/v1/chat/completions', defaultModel: 'gpt-4o-mini' },
  { id: 'claude', label: 'Claude', defaultEndpoint: 'https://api.anthropic.com/v1/messages', defaultModel: 'claude-sonnet-4' },
  { id: 'groq', label: 'Groq', defaultEndpoint: 'https://api.groq.com/openai/v1/chat/completions', defaultModel: 'llama-3.3-70b-versatile' },
  { id: 'openrouter', label: 'OpenRouter', defaultEndpoint: 'https://openrouter.ai/api/v1/chat/completions', defaultModel: 'openai/gpt-4o-mini' },
  { id: 'compatible', label: 'OpenAI-compatible', defaultEndpoint: '', defaultModel: 'custom-model', endpointLabel: 'https://your-host/v1/chat/completions' },
  { id: 'ollama', label: 'Local Ollama', defaultEndpoint: 'http://127.0.0.1:11434/api/chat', defaultModel: 'llama3.1', endpointLabel: 'http://127.0.0.1:11434/api/chat' },
  { id: 'bedrock', label: 'AWS Bedrock', defaultEndpoint: '', defaultModel: 'anthropic.claude-3-5-sonnet', endpointLabel: 'https://bedrock-runtime.<region>.amazonaws.com/model/<model>/converse' },
  { id: 'vertex', label: 'GCP Vertex AI', defaultEndpoint: '', defaultModel: 'gemini-2.0-flash', endpointLabel: 'https://aiplatform.googleapis.com/… endpoint per director docs' },
];

type Tab = 'keys' | 'ai' | 'transcribe' | 'style' | 'license';
const tabs: [Tab, string][] = [['keys', 'API keys'], ['ai', 'AI & encoding'], ['transcribe', 'Transcription'], ['style', 'My style'], ['license', 'License']];

export function SettingsModal() {
  const dialog = useRef<HTMLDialogElement>(null);
  const [open, setOpen] = useState(false);
  const [tab, setTab] = useState<Tab>('keys');
  const [provider, setProvider] = useState<Provider>('gemini');
  const [model, setModel] = useState(providers[0].defaultModel);
  const [endpoint, setEndpoint] = useState(providers[0].defaultEndpoint);
  const [storage, setStorage] = useState<'os' | 'session'>('os');
  const [keyBusy, setKeyBusy] = useState(false);
  const [keyMessage, setKeyMessage] = useState('');
  const [keyError, setKeyError] = useState('');
  const [encoder, setEncoder] = useState<string>(getEncoderPreference);
  const [whisperBinary, setWhisperBinaryState] = useState(getWhisperBinary);
  const [whisperModel, setWhisperModelState] = useState(getWhisperModel);
  const [whisperStatus, setWhisperStatus] = useState('');
  const [localFiles, setLocalFiles] = useState<string[]>([]);
  const [localFilesNote, setLocalFilesNote] = useState('');
  const [license, setLicense] = useState<LicenseStatus | null>(null);
  const [licenseText, setLicenseText] = useState('');
  const [licenseBusy, setLicenseBusy] = useState(false);
  const [licenseMessage, setLicenseMessage] = useState('');
  const [licenseError, setLicenseError] = useState('');
  const [styleDna, setStyleDnaState] = useState<StyleDNA>(getStyleDna);
  const [styleSaved, setStyleSaved] = useState('');
  const keyInput = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const chosen = providers.find(p => p.id === provider);
    if (chosen) { setModel(chosen.defaultModel); setEndpoint(chosen.defaultEndpoint); }
    if (keyInput.current) keyInput.current.value = '';
  }, [provider]);
  useEffect(() => {
    if (!open) return;
    setKeyMessage(''); setKeyError(''); setLicenseMessage(''); setLicenseError('');
    if (isTauri()) void invoke<LicenseStatus>('license_status').then(setLicense).catch(e => setLicenseError(String(e)));
  }, [open]);
  const scanLocalModels = async (modelPath: string) => {
    if (!isTauri()) return;
    try {
      const found = await invoke<ListedModel[]>('list_transcription_models', { request: { provider: 'local', apiKey: '', endpoint: '', modelPath } });
      setLocalFiles(found.map(f => f.path).filter((f): f is string => !!f));
      setLocalFilesNote(found.length ? `${found.length} model file${found.length === 1 ? '' : 's'} in this folder.` : 'No .bin files in this folder.');
    } catch (e) { setLocalFilesNote(String(e)); }
  };
  useEffect(() => { if (open && tab === 'transcribe') void scanLocalModels(whisperModel); }, [open, tab]);
  const close = () => { dialog.current?.close(); setOpen(false); };

  const saveKey = async () => {
    if (!isTauri()) { setKeyError('Key storage requires the desktop app.'); return; }
    const key = keyInput.current?.value ?? '';
    setKeyBusy(true); setKeyError(''); setKeyMessage('');
    try {
      await invoke('director_credential', { config: { provider, model: model || 'studiocut-scope', endpoint: endpoint || 'https://scope.invalid', maxTokens: 4096 }, key: key || null, storage });
      setKeyMessage(key ? `${provider} key saved to ${storage === 'os' ? 'OS credential storage' : 'this session only'}.` : `${provider} key removed. The ${provider} environment variable is used when present.`);
      if (keyInput.current) keyInput.current.value = '';
    } catch (e) { setKeyError(String(e)); } finally { setKeyBusy(false); }
  };

  const checkWhisper = async () => {
    setWhisperStatus('Checking…');
    try { setWhisperStatus(await invoke<string>('transcription_status', { binary: whisperBinary, model: whisperModel })); }
    catch (e) { setWhisperStatus(String(e)); }
  };

  const activate = async () => {
    if (!isTauri()) { setLicenseError('License activation requires the desktop app.'); return; }
    setLicenseBusy(true); setLicenseError(''); setLicenseMessage('');
    try {
      const info = await invoke<{ name: string; email: string }>('license_activate', { text: licenseText.trim() });
      setLicenseMessage(`License activated for ${info.name} <${info.email}>.`);
      setLicenseText('');
      setLicense(await invoke<LicenseStatus>('license_status'));
    } catch (e) { setLicenseError(String(e)); } finally { setLicenseBusy(false); }
  };
  const deactivate = async () => {
    if (!isTauri()) return;
    setLicenseBusy(true); setLicenseError('');
    try { await invoke('license_deactivate'); setLicenseMessage('Stored license removed.'); setLicense(await invoke<LicenseStatus>('license_status')); }
    catch (e) { setLicenseError(String(e)); } finally { setLicenseBusy(false); }
  };

  const chosenProvider = providers.find(p => p.id === provider)!;
  const needsEndpoint = ['compatible', 'ollama', 'bedrock', 'vertex'].includes(provider);
  return <>
    <button className="button secondary settings-button" onClick={() => { setOpen(true); dialog.current?.showModal(); }} aria-label="Open settings">Settings</button>
    <dialog ref={dialog} className="settings-dialog" onCancel={close} onClose={close} aria-labelledby="settings-title">
      <header className="settings-header"><div><span className="eyebrow">YOUR MACHINE, YOUR KEYS</span><h2 id="settings-title">Settings</h2></div><button className="icon-button" aria-label="Close settings" onClick={close}><X size={19} /></button></header>
      <div className="tabs settings-tabs" role="tablist" aria-label="Settings sections">{tabs.map(([id, label]) => <button key={id} role="tab" aria-selected={tab === id} onClick={() => setTab(id)}>{label}</button>)}</div>
      <div className="settings-body" role="tabpanel">
        {tab === 'keys' && <section aria-label="BYOK API keys">
          <p className="settings-note">Keys are stored with the operating-system credential manager, or explicitly in session-only memory. Keys are never written to project files, the timeline store or the media index.</p>
          <label>Provider<select value={provider} onChange={e => setProvider(e.target.value as Provider)}>{providers.map(p => <option key={p.id} value={p.id}>{p.label}</option>)}</select></label>
          <label>Model<input value={model} onChange={e => setModel(e.target.value)} maxLength={200} /></label>
          {needsEndpoint && <label>Endpoint<input value={endpoint} onChange={e => setEndpoint(e.target.value)} maxLength={400} placeholder={chosenProvider.endpointLabel} spellCheck={false} />{!endpoint && <small>{chosenProvider.endpointLabel}</small>}</label>}
          {!needsEndpoint && <small>Requests go to the provider's standard endpoint for this model.</small>}
          <label>{chosenProvider.label} API key<input ref={keyInput} type="password" autoComplete="off" spellCheck={false} maxLength={8192} placeholder={provider === 'ollama' ? 'Not required for local Ollama' : 'Paste key, or leave empty to remove'} /></label>
          <fieldset className="storage-choice"><legend>Storage</legend>
            <label className="inline"><input type="radio" name="key-storage" checked={storage === 'os'} onChange={() => setStorage('os')} />OS credential storage</label>
            <label className="inline"><input type="radio" name="key-storage" checked={storage === 'session'} onChange={() => setStorage('session')} />Session only (cleared on quit)</label>
          </fieldset>
          <button className="button primary" disabled={keyBusy} onClick={() => void saveKey()}>{keyBusy ? 'Working…' : 'Save key'}</button>
          <button className="button secondary" disabled={keyBusy} onClick={() => { if (keyInput.current) keyInput.current.value = ''; void saveKey(); }}>Remove saved key</button>
          <div role="status" aria-live="polite">{keyMessage}</div>{keyError && <p className="error" role="alert">{keyError}</p>}
        </section>}
        {tab === 'ai' && <section aria-label="AI and encoding preferences">
          <p className="settings-note">The preferred encoder is used as the default in the export dialog. Hardware candidates are still probe-checked and fall back to libx264 with a clear disclosure.</p>
          <label>Preferred encoder<select value={encoder} onChange={e => { setEncoder(e.target.value); setEncoderPreference(e.target.value); }}>
            <option value="auto">Auto · probe hardware, then CPU</option>
            <option value="libx264">libx264 · CPU</option>
            <option value="h264_nvenc">NVIDIA · H.264</option>
            <option value="hevc_videotoolbox">Apple VideoToolbox · HEVC</option>
            <option value="h264_amf">AMD AMF · H.264</option>
            <option value="h264_qsv">Intel Quick Sync · H.264</option>
          </select></label>
          <p className="settings-note">AI Director proposals are validated in TypeScript and again in Rust before anything touches your timeline. Every applied proposal is one undo checkpoint.</p>
        </section>}
        {tab === 'transcribe' && <section aria-label="Transcription setup">
          <p className="settings-note">Local transcription uses whisper.cpp (<code>whisper-cli</code>) with a GGML model file. See <strong>SETUP_BINARIES.md</strong> in the repository for placement per platform. Cloud transcription uses the key saved for its provider.</p>
          <label>whisper-cli path or command<input value={whisperBinary} onChange={e => { setWhisperBinaryState(e.target.value); setWhisperBinary(e.target.value); }} spellCheck={false} /></label>
          <label>GGML model path (.bin)<input list="settings-whisper-files" value={whisperModel} onChange={e => { setWhisperModelState(e.target.value); setWhisperModel(e.target.value); }} onBlur={() => void scanLocalModels(whisperModel)} placeholder="e.g. C:\models\ggml-base.en.bin or ~/models/ggml-base.en.bin" spellCheck={false} /></label>
          <datalist id="settings-whisper-files">{localFiles.map(f => <option key={f} value={f} />)}</datalist>
          {localFilesNote && <small>{localFilesNote}</small>}
          <button className="button secondary" onClick={() => void checkWhisper()}>Check availability</button>
          <div role="status" aria-live="polite">{whisperStatus && <pre className="whisper-status">{whisperStatus}</pre>}</div>
        </section>}
        {tab === 'style' && <section aria-label="My style (Style DNA)">
          <p className="settings-note">One saved look + pace for the whole studio. Export opens with this aspect and fill; captions open with this preset. Nothing here is sent to any provider.</p>
          <label>Caption preset<select value={styleDna.captionPreset} onChange={e => setStyleDnaState({ ...styleDna, captionPreset: e.target.value as StyleDNA['captionPreset'] })}><option value="hormozi">Hormozi · bold punch</option><option value="minimal">Minimal · clean type</option><option value="documentary">Documentary · lower third</option><option value="neon">Neon · electric glow</option></select></label>
          <label>Export aspect<select value={styleDna.aspect} onChange={e => setStyleDnaState({ ...styleDna, aspect: e.target.value as StyleDNA['aspect'] })}><option value="landscape">16:9 · Landscape</option><option value="vertical">9:16 · Vertical</option><option value="square">1:1 · Square</option></select></label>
          <label>Frame fill<select value={styleDna.fill} onChange={e => setStyleDnaState({ ...styleDna, fill: e.target.value as StyleDNA['fill'] })}><option value="fit">Fit · bars added</option><option value="cover">Cover · fills frame, crops edges</option></select></label>
          <label>Pace<select value={styleDna.pace} onChange={e => setStyleDnaState({ ...styleDna, pace: e.target.value as StyleDNA['pace'] })}><option value="preserve">Preserve original pacing</option><option value="tight">Tighter cuts with natural pauses</option><option value="documentary">Documentary with B-roll</option><option value="shortform">Short-form with kinetic captions</option></select></label>
          <label>Intro style<select value={styleDna.introStyle} onChange={e => setStyleDnaState({ ...styleDna, introStyle: e.target.value as StyleDNA['introStyle'] })}><option value="hook-first">Hook first · strongest line opens</option><option value="cold-open">Cold open · scene first, no title</option><option value="title-card">Title card · name up front</option><option value="none">No intro treatment</option></select></label>
          <label>Music mood<select value={styleDna.musicMood} onChange={e => setStyleDnaState({ ...styleDna, musicMood: e.target.value as StyleDNA['musicMood'] })}><option value="none">No music preference</option><option value="upbeat">Upbeat</option><option value="calm">Calm</option><option value="dramatic">Dramatic</option><option value="cinematic">Cinematic</option></select></label>
          <button className="button primary" onClick={() => { setStyleDna(styleDna); setStyleSaved('Style saved. Export and captions will open with these choices.'); }}>Save my style</button>
          <button className="button secondary" onClick={() => { resetDirectorFeedback(); setStyleDnaState({ ...DEFAULT_STYLE_DNA }); setStyleSaved('Learning cleared; style reset to defaults.'); }}>Reset learning</button>
          <div role="status" aria-live="polite">{styleSaved}</div>
        </section>}
        {tab === 'license' && <section aria-label="License">
          {!isTauri() && <p className="settings-note">License status requires the desktop app.</p>}
          {license && <dl className="project-details license-details">
            <dt>Machine fingerprint</dt><dd><code>{license.fingerprint}</code></dd>
            <dt>Mode</dt><dd>{license.mode === 'dev' ? 'Development build · license check bypassed' : license.licensed ? 'Licensed' : 'Not activated'}</dd>
            {license.license && <><dt>Licensee</dt><dd>{license.license.name} &lt;{license.license.email}&gt;</dd></>}
          </dl>}
          <p className="settings-note">Licenses are Ed25519-signed files bound to this machine's fingerprint. Paste the license file contents to activate. Verification is fully offline.</p>
          <label>License file<textarea rows={4} value={licenseText} onChange={e => setLicenseText(e.target.value)} placeholder="base64payload.signature" spellCheck={false} maxLength={8192} /></label>
          <button className="button primary" disabled={licenseBusy || !licenseText.trim()} onClick={() => void activate()}>Activate license</button>
          <button className="button secondary" disabled={licenseBusy} onClick={() => void deactivate()}>Remove stored license</button>
          <div role="status" aria-live="polite">{licenseMessage}</div>{licenseError && <p className="error" role="alert">{licenseError}</p>}
        </section>}
      </div>
      {SUPPORT_URL && <footer className="settings-footer"><a className="button secondary" href={SUPPORT_URL} target="_blank" rel="noreferrer">Support StudioCut AI on Buy Me a Coffee</a></footer>}
    </dialog>
  </>;
}
