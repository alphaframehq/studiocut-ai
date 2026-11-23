import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { native, useProject } from './store';
import { compileEdl, parseEdl, summarize, type Edl } from './edl';
import './director.css';

type Provider = 'gemini' | 'openai' | 'claude' | 'groq' | 'openrouter' | 'compatible' | 'ollama' | 'bedrock' | 'vertex';
type Config = { provider: Provider; model: string; endpoint: string; maxTokens: number };
type Trace = { requestId: string; provider: string; model: string; latencyMs: number; contextBytes: number; usage: unknown; promptVersion: number; estimatedCost: number | null };
type Proposal = { edl: Edl; trace?: Trace };
const providers: [Provider, string][] = [['gemini', 'Gemini'], ['openai', 'OpenAI'], ['claude', 'Claude'], ['groq', 'Groq'], ['openrouter', 'OpenRouter'], ['compatible', 'OpenAI-compatible'], ['ollama', 'Local Ollama'], ['bedrock', 'AWS Bedrock'], ['vertex', 'GCP Vertex AI']];

export function DirectorPanel() {
  const { project, busy, applyEdl } = useProject();
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<'wizard' | 'chat'>('wizard');
  const [step, setStep] = useState(1);
  const [config, setConfig] = useState<Config>({ provider: 'gemini', model: '', endpoint: '', maxTokens: 4096 });
  const [storage, setStorage] = useState<'os' | 'session'>('os');
  const key = useRef<HTMLInputElement>(null);
  const [instruction, setInstruction] = useState('');
  const [goal, setGoal] = useState('');
  const [style, setStyle] = useState('Preserve original pacing');
  const [consent, setConsent] = useState(false);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState('');
  const [status, setStatus] = useState('');
  const [proposal, setProposal] = useState<Proposal | null>(null);
  const [context, setContext] = useState<{ data: unknown; bytes: number; omittedAssets: number; omittedClips: number } | null>(null);
  const [raw, setRaw] = useState('');
  const [messages, setMessages] = useState<{ role: string; text: string }[]>([]);
  useEffect(() => { setConsent(false); setContext(null); setProposal(null); }, [project?.id]);
  useEffect(() => { setConsent(false); if (key.current) key.current.value = ''; }, [config.provider, config.endpoint]);
  if (!project) return null;
  const stale = proposal && (proposal.edl.projectId !== project.id || proposal.edl.baseRevision !== project.revision);
  const cloud = config.provider !== 'ollama';
  const task = async (action: () => Promise<void>) => { setWorking(true); setError(''); setStatus(''); try { await action(); } catch (e) { setError(String(e)); } finally { setWorking(false); } };
  const request = () => task(async () => {
    const text = mode === 'wizard' ? `${goal}\nStyle: ${style}.\n${instruction}` : instruction;
    setMessages(m => [...m.slice(-15), { role: 'You', text }]);
    setProposal(null);
    const result = await invoke<Proposal>('director_propose', { projectId: project.id, revision: project.revision, config, instruction: text, consent });
    const current = useProject.getState().project;
    if (!current) throw new Error('Project closed while requesting.');
    compileEdl(current, result.edl);
    setProposal(result); setMessages(m => [...m.slice(-15), { role: 'Director', text: result.edl.summary }]); setStatus('Proposal validated. Nothing has changed yet.');
  });
  return <section className="director-section" aria-label="AI Director">
    <div className="director-bar"><div><span className="eyebrow">YOUR INTENT. YOUR FINAL CUT.</span><h2>AI Director <span>BYOK</span></h2><p>Describe the edit. Review every change. Approve once.</p></div><button className="button secondary" aria-expanded={open} onClick={() => setOpen(v => !v)}>{open ? 'Close Director' : 'Open AI Director'}</button></div>
    {open && <div className="director-layout"><div className="director-setup">
      <div className="director-tabs" role="tablist" aria-label="Director mode"><button role="tab" aria-selected={mode === 'wizard'} onClick={() => setMode('wizard')}>Director Wizard</button><button role="tab" aria-selected={mode === 'chat'} onClick={() => setMode('chat')}>Chat-to-Edit</button></div>
      {!native && <p className="director-notice">Desktop required for credentials and model requests. This browser can validate and review an EDL locally; no API keys are accepted here.</p>}
      <details open={step === 1}><summary>1. Choose your model & credential storage</summary><fieldset disabled={working}>
        <label>Provider<select value={config.provider} onChange={e => setConfig({ ...config, provider: e.target.value as Provider, model: '', endpoint: e.target.value === 'ollama' ? 'http://127.0.0.1:11434/api/chat' : '' })}>{providers.map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>
        <label>Model ID<input value={config.model} onChange={e => setConfig({ ...config, model: e.target.value })} maxLength={200} placeholder="Exact model ID from your provider" /></label>
        {['compatible', 'ollama', 'bedrock', 'vertex'].includes(config.provider) && <label>Full API endpoint<input value={config.endpoint} onChange={e => setConfig({ ...config, endpoint: e.target.value })} placeholder={config.provider === 'compatible' ? 'https://your-host/v1/chat/completions' : 'Full model request URL'} /></label>}
        {config.provider === 'bedrock' && <p>Uses a Bedrock API key (bearer token), not an AWS access-key/secret pair.</p>}
        {config.provider === 'vertex' && <p>Uses a Google OAuth access token. Tokens expire; replace yours when needed. Use the Google publisher generateContent endpoint.</p>}
        {cloud && <><label>Credential storage<select value={storage} disabled={!native} onChange={e => setStorage(e.target.value as 'os' | 'session')}><option value="os">OS credential store (recommended)</option><option value="session">Session only — cleared on app exit</option></select></label><label>API key / access token<input ref={key} type="password" autoComplete="off" spellCheck={false} disabled={!native} maxLength={8192} placeholder={native ? 'Stored only by Rust, never in project data' : 'Available in the desktop app'} /></label><div className="director-actions"><button className="button secondary" disabled={!native || !config.model} onClick={() => void task(async () => { const secret = key.current?.value; if (key.current) key.current.value = ''; if (!secret) throw new Error('Enter a credential.'); await invoke('director_credential', { config, key: secret, storage }); setStatus(storage === 'os' ? 'Saved in the OS credential store.' : 'Stored in Rust memory for this session only. Existing OS credentials are unchanged.'); })}>Save credential</button><button className="button secondary" disabled={!native || !config.model} onClick={() => void task(async () => { await invoke('director_credential', { config, key: null, storage: null }); setStatus('Saved OS/session credential removed. Environment credentials, if configured, are unchanged.'); })}>Forget credential</button></div><p>No silent fallback: if the OS store is unavailable, explicitly choose Session only and enter the key again.</p></>}
        <label>Maximum output tokens<input type="number" min={256} max={8192} value={config.maxTokens} onChange={e => setConfig({ ...config, maxTokens: Number(e.target.value) })} /></label><p>No automatic provider fallback. One request at a time; one rate-limit retry. Pricing varies by model; the token cap is not a dollar-budget guarantee.</p>
      </fieldset><button className="button secondary" onClick={() => setStep(2)}>Continue to edit brief</button></details>
      <div className="director-brief"><h3>2. {mode === 'wizard' ? 'Shape your edit' : 'Tell the Director what to change'}</h3>{mode === 'wizard' && <><label>Editing goal<input value={goal} onChange={e => setGoal(e.target.value)} maxLength={1000} placeholder="Turn this interview into a concise opening" /></label><label>Editorial direction<select value={style} onChange={e => setStyle(e.target.value)}><option>Preserve original pacing</option><option>Tighter cuts with natural pauses</option><option>Documentary with B-roll</option><option>Short-form with kinetic captions</option></select></label></>}
        <label>{mode === 'wizard' ? 'Constraints and details' : 'Edit request'}<textarea value={instruction} onChange={e => setInstruction(e.target.value)} maxLength={3000} rows={4} placeholder="Keep the opening sentence. Duck music under dialogue. Do not change locked tracks." /></label>
        <p>Up to 100 assets, 200 clips and 8,000 transcript characters. Paths and media bytes are excluded. Existing caption timings are the transcript; this does not transcribe media. Each request uses current project context, not earlier chat messages.</p>
        <button className="button secondary" disabled={!native || working} onClick={() => void task(async () => { setContext(await invoke('director_context', { projectId: project.id, revision: project.revision })); })}>Inspect outbound context</button>
        {context && <details><summary>{context.bytes.toLocaleString()} bytes · {context.omittedAssets} assets / {context.omittedClips} clips omitted</summary><pre>{JSON.stringify(context.data, null, 2)}</pre></details>}
        {cloud && <label className="consent"><input type="checkbox" checked={consent} disabled={!native} onChange={e => setConsent(e.target.checked)} />I approve sending this brief and bounded catalog/transcript context to {providers.find(p => p[0] === config.provider)?.[1]}.</label>}
        <button className="button primary" disabled={!native || working || busy || !config.model || (cloud && !consent) || !(instruction.trim() || goal.trim())} onClick={() => void request()}>{working ? 'Waiting for a validated proposal…' : 'Generate edit proposal'}</button>
      </div>
      <details className="edl-import"><summary>Review a typed EDL JSON proposal</summary><label>EDL JSON<textarea rows={7} value={raw} onChange={e => setRaw(e.target.value)} maxLength={262144} spellCheck={false} /></label><button className="button secondary" disabled={working || busy} onClick={() => { setError(''); try { const edl = parseEdl(raw); compileEdl(project, edl); setProposal({ edl }); setStatus('Local EDL validated. Nothing has changed yet.'); } catch (e) { setError(String(e)); } }}>Validate proposal</button></details>
    </div><aside className="director-review" aria-label="Chat and edit approval"><h3>3. Review & approve</h3><p className="director-badge">Propose → validate → approve → one undo</p>
      <div className="director-messages" aria-live="polite">{messages.map((m, i) => <div key={i}><strong>{m.role}</strong><p>{m.text}</p></div>)}</div>
      {proposal ? <><h4>{proposal.edl.summary}</h4><p>{proposal.edl.operations.length} operations · based on revision {proposal.edl.baseRevision}</p><ol>{summarize(proposal.edl, project).map((text, i) => <li key={i}>{text}</li>)}</ol>{stale && <p role="alert" className="error">Stale proposal: the project changed. Generate a new proposal.</p>}<div className="director-actions"><button className="button primary" disabled={!!stale || busy || working || !proposal.edl.operations.length} onClick={() => void task(async () => { if (await applyEdl(proposal.edl)) { setProposal(null); setStatus('Approved edits applied as one undo checkpoint.'); } else throw new Error(useProject.getState().error); })}>Approve & apply edits</button><button className="button secondary" disabled={working} onClick={() => { setProposal(null); setStatus('Proposal discarded. Timeline unchanged.'); }}>Discard proposal</button></div><details><summary>Validated EDL and request trace</summary><pre>{JSON.stringify(proposal, null, 2)}</pre><p>Trace is session-only. Provider usage is shown when reported. Cost is not estimated. No shell or external tools were invoked.</p></details></> : <div className="director-empty"><strong>You are the final decision.</strong><p>Generated edits appear here before anything touches your timeline. Invalid output is rejected, never silently repaired.</p></div>}
      <div role="status" aria-live="polite">{working ? 'Working… No edits are applied while a request is running.' : status}</div>{error && <p className="error" role="alert">{error}</p>}
    </aside></div>}
  </section>;
}
