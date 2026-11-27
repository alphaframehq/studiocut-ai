import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { native, useProject } from './store';
import { compileEdl, parseEdl, summarize, type Edl, type Operation } from './edl';
import { addDirectorHistory, buildLearnedPreferences, directorOpLabel, getCachedDirectorModels, getDirectorFeedback, getDirectorHistory, getDirectorModel, maybeUpdateStyleDnaFromFeedback, recordDirectorFeedback, recordPaceFeedback, recordPresetFeedback, resetDirectorFeedback, setCachedDirectorModels, setDirectorModel, type DirectorFeedback, type DirectorHistoryEntry, type DirectorListedModel } from './preferences';
import './director.css';

type Provider = 'gemini' | 'openai' | 'claude' | 'groq' | 'openrouter' | 'compatible' | 'ollama' | 'bedrock' | 'vertex';
type Config = { provider: Provider; model: string; endpoint: string; maxTokens: number };
type Trace = { requestId: string; provider: string; model: string; latencyMs: number; contextBytes: number; usage: unknown; promptVersion: number; estimatedCost: number | null };
type ProposalPlan = { pace: string; cuts: string; captions: string; grade: string; music: string };
type Proposal = {
  id?: string;
  edl: Edl;
  trace?: Trace;
  text?: string;
  plan?: ProposalPlan;
  qaDropped?: { index: number; reason: string }[];
  diff?: { added: number; removed: number; changed: number };
  gradeSuggestion?: { brightness: number; contrast: number; saturation: number; cube: string | null };
  captionPreset?: string;
};
type Continuing = { id: string; edl: Edl; summary: string };
type LoudnessResult = { integratedLufs: number; gainDb: number; targetLufs: number };
const providers: [Provider, string][] = [['gemini', 'Gemini'], ['openai', 'OpenAI'], ['claude', 'Claude'], ['groq', 'Groq'], ['openrouter', 'OpenRouter'], ['compatible', 'OpenAI-compatible'], ['ollama', 'Local Ollama'], ['bedrock', 'AWS Bedrock'], ['vertex', 'GCP Vertex AI']];
const ROUGH_OP_TYPES = new Set<Operation['type']>(['trim', 'split', 'delete', 'ripple', 'move', 'jCut', 'lCut']);

type TitleSection = { title: string; body: string };
function parseTitleSections(text: string): TitleSection[] {
  const sections: TitleSection[] = [];
  let current: TitleSection | null = null;
  for (const line of text.split('\n')) {
    const header = line.match(/^##\s+(.+?)\s*$/);
    if (header) {
      current = { title: header[1], body: '' };
      sections.push(current);
      continue;
    }
    if (current) current.body += (current.body ? '\n' : '') + line;
  }
  return sections
    .map(s => ({ title: s.title, body: s.body.trim() }))
    .filter(s => s.body.length > 0);
}

/** Map a plan's free-text pace to a Style DNA pace value. */
function mapPlanPace(paceText: string): 'preserve' | 'tight' | 'documentary' | 'shortform' {
  const lower = paceText.toLowerCase();
  if (lower.includes('tight')) return 'tight';
  if (lower.includes('short') || lower.includes('shortform')) return 'shortform';
  if (lower.includes('documentary')) return 'documentary';
  return 'preserve';
}

export function DirectorPanel() {
  const { project, busy, applyEdl, selected } = useProject();
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<'wizard' | 'chat'>('wizard');
  const [step, setStep] = useState(1);
  const [config, setConfig] = useState<Config>({ provider: 'gemini', model: getDirectorModel('gemini') ?? '', endpoint: '', maxTokens: 4096 });
  const [storage, setStorage] = useState<'os' | 'session'>('os');
  const key = useRef<HTMLInputElement>(null);
  const instructionRef = useRef<HTMLTextAreaElement>(null);
  const [instruction, setInstruction] = useState('');
  const [goal, setGoal] = useState('');
  const [style, setStyle] = useState('Preserve original pacing');
  const [consent, setConsent] = useState(false);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState('');
  const [status, setStatus] = useState('');
  const [proposal, setProposal] = useState<Proposal | null>(null);
  const [proposalId, setProposalId] = useState<string | null>(null);
  const [continuing, setContinuing] = useState<Continuing | null>(null);
  const [checked, setChecked] = useState<boolean[]>([]);
  const [pendingModifiedOp, setPendingModifiedOp] = useState<string | null>(null);
  const [roughCutApplied, setRoughCutApplied] = useState(false);
  const [polishUnlocked, setPolishUnlocked] = useState(false);
  const [learnedRecorded, setLearnedRecorded] = useState({ preset: false, pace: false });
  const [feedback, setFeedback] = useState<DirectorFeedback>(() => getDirectorFeedback());
  const [learningNote, setLearningNote] = useState('');
  const [loudness, setLoudness] = useState('');
  const [loudnessError, setLoudnessError] = useState('');
  const [context, setContext] = useState<{ data: unknown; bytes: number; omittedAssets: number; omittedClips: number } | null>(null);
  const [raw, setRaw] = useState('');
  const [messages, setMessages] = useState<{ role: string; text: string }[]>([]);
  const [models, setModels] = useState<DirectorListedModel[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [modelsError, setModelsError] = useState('');
  const [modelsNote, setModelsNote] = useState('');
  const [modelMode, setModelMode] = useState<'list' | 'custom'>('list');
  const [customModel, setCustomModel] = useState('');
  const [titles, setTitles] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [copiedSection, setCopiedSection] = useState<string | null>(null);
  const [history, setHistory] = useState<DirectorHistoryEntry[]>([]);
  const [hasKey, setHasKey] = useState(false);
  useEffect(() => { setConsent(false); setContext(null); setProposal(null); setProposalId(null); setContinuing(null); setChecked([]); setPendingModifiedOp(null); setRoughCutApplied(false); setPolishUnlocked(false); setLearnedRecorded({ preset: false, pace: false }); setTitles(null); setLoudness(''); setLoudnessError(''); }, [project?.id]);
  useEffect(() => { if (project) setHistory(getDirectorHistory(project.id)); }, [project?.id]);
  useEffect(() => { setConsent(false); setHasKey(false); if (key.current) key.current.value = ''; }, [config.provider, config.endpoint]);
  useEffect(() => { void refreshModels(config.provider, '', config.endpoint, false, config.model); }, []);
  // Fetch (or load from the 1h cache) the Director-model list for a
  // provider. Static lists (Claude/Bedrock) and the empty Vertex list come
  // back with a note; Ollama needs no key.
  async function refreshModels(p: Provider, keyValue: string, endpointValue: string, bypassCache: boolean, currentModel: string) {
    if (!native) return;
    setModelsError(''); setModelsNote('');
    const needsKey = !['ollama', 'claude', 'bedrock', 'vertex'].includes(p);
    if (needsKey && !keyValue) { setModels([]); setModelsNote('Enter an API key, then refresh the model list.'); return; }
    if (!bypassCache) {
      const cached = getCachedDirectorModels(p);
      if (cached) { applyModels(cached.models, currentModel, null); return; }
    }
    setModelsLoading(true);
    try {
      const found = await invoke<{ models: DirectorListedModel[]; note: string | null }>('list_director_models', { request: { provider: p, apiKey: keyValue, endpoint: endpointValue } });
      setCachedDirectorModels(p, found.models);
      if (found.note) setModelsNote(found.note);
      applyModels(found.models, currentModel, found.note);
    } catch (error) { setModelsError(String(error)); }
    finally { setModelsLoading(false); }
  }
  // Keep the currently selected model visible even if the provider no
  // longer lists it (e.g. a custom or retired id).
  function applyModels(found: DirectorListedModel[], current: string, note: string | null) {
    const list = current && !found.some(m => m.id === current) ? [{ id: current, recommended: false, note: null }, ...found] : found;
    setModels(list);
    if (!list.length && !note) setModelsNote('No chat models returned for this provider.');
  }
  function changeProvider(p: Provider) {
    const persisted = getDirectorModel(p) ?? '';
    setConfig({ provider: p, model: persisted, endpoint: p === 'ollama' ? 'http://127.0.0.1:11434/api/chat' : '', maxTokens: 4096 });
    setModels([]); setModelsError(''); setModelsNote(''); setModelMode('list'); setCustomModel('');
    void refreshModels(p, '', p === 'ollama' ? 'http://127.0.0.1:11434/api/chat' : '', false, persisted);
  }
  function pickModel(id: string) {
    if (id === '__custom') { setModelMode('custom'); return; }
    setModelMode('list');
    setConfig(c => ({ ...c, model: id }));
    setDirectorModel(config.provider, id);
  }
  function refreshFeedback() {
    setFeedback(getDirectorFeedback());
  }
  function recordHistory(kind: 'edit' | 'titles', instructionText: string, summary: string, edl?: Edl) {
    if (!project) return;
    setHistory(addDirectorHistory(project.id, { kind, instruction: instructionText, summary, at: Date.now(), ...(edl ? { edl } : {}) }));
  }
  if (!project) return null;
  const stale = proposal && (proposal.edl.projectId !== project.id || proposal.edl.baseRevision !== project.revision);
  const cloud = config.provider !== 'ollama';
  const summaries = proposal ? summarize(proposal.edl, project) : [];
  const operations = proposal?.edl.operations ?? [];
  const isStaged = operations.length > 6;
  const stage1Indices = operations.map((_op, i) => i).filter(i => ROUGH_OP_TYPES.has(operations[i].type));
  const stage2Indices = operations.map((_op, i) => i).filter(i => !ROUGH_OP_TYPES.has(operations[i].type));
  const allIndices = operations.map((_op, i) => i);
  const titleSections = titles ? parseTitleSections(titles) : [];
  const learnedSummary = buildLearnedPreferences();
  const topLearned = learnedSummary ? learnedSummary.slice(0, learnedSummary.indexOf('.') + 1 || learnedSummary.length) : '';
  const feedbackEntries = Object.entries(feedback.ops).filter(([, counts]) => counts.accepted + counts.rejected + counts.modified >= 1);
  const feedbackNonEmpty =
    feedbackEntries.length > 0 ||
    Object.values(feedback.presets).some(c => c.accepted + c.rejected >= 1) ||
    Object.values(feedback.pace).some(c => c.accepted + c.rejected >= 1);
  const task = async (action: () => Promise<void>) => { setWorking(true); setError(''); setStatus(''); try { await action(); } catch (e) { setError(String(e)); } finally { setWorking(false); } };

  /** Record accept/reject feedback for one apply, plus preset/pace learning (once per proposal). */
  function recordApplyFeedback(current: Proposal, appliedOps: Operation[], rejectedOps: Operation[]) {
    recordDirectorFeedback([
      ...appliedOps.map(op => ({ opType: op.type, outcome: 'accepted' as const })),
      ...rejectedOps.map(op => ({ opType: op.type, outcome: 'rejected' as const })),
    ]);
    const captionApplied = appliedOps.some(op => op.type === 'captions' || op.type === 'captionStyle');
    const captionInStage = appliedOps.length + rejectedOps.length > 0 &&
      [...appliedOps, ...rejectedOps].some(op => op.type === 'captions' || op.type === 'captionStyle');
    if (current.captionPreset && !learnedRecorded.preset && (captionApplied || captionInStage)) {
      // Rejected only when every caption op in this stage was unchecked.
      recordPresetFeedback(current.captionPreset, captionApplied ? 'accepted' : 'rejected');
      setLearnedRecorded(r => ({ ...r, preset: true }));
    }
    if (current.plan?.pace && !learnedRecorded.pace) {
      recordPaceFeedback(mapPlanPace(current.plan.pace), appliedOps.length >= 1 ? 'accepted' : 'rejected');
      setLearnedRecorded(r => ({ ...r, pace: true }));
    }
    const update = maybeUpdateStyleDnaFromFeedback();
    if (update.changed) setLearningNote(update.note);
    refreshFeedback();
  }

  /** Apply one subset of the current proposal's ops as a sub-EDL (one undo checkpoint). */
  const applyOps = (indices: number[], stage: 'all' | 'rough' | 'polish') => task(async () => {
    if (!proposal) return;
    const current = proposal;
    const checkedIndices = indices.filter(i => checked[i] !== false);
    if (!checkedIndices.length) return;
    const subEdl: Edl = { ...current.edl, operations: checkedIndices.map(i => current.edl.operations[i]) };
    if (!(await applyEdl(subEdl))) throw new Error(useProject.getState().error);
    const appliedOps = checkedIndices.map(i => current.edl.operations[i]);
    const rejectedOps = indices.filter(i => checked[i] === false).map(i => current.edl.operations[i]);
    recordApplyFeedback(current, appliedOps, rejectedOps);
    if (stage === 'rough') {
      // Keep the polish ops for the next step, rebased onto the new
      // revision so the follow-up apply still validates.
      const newRevision = useProject.getState().project?.revision ?? current.edl.baseRevision + 1;
      const remainingIndices = current.edl.operations.map((_op, i) => i).filter(i => !ROUGH_OP_TYPES.has(current.edl.operations[i].type));
      const remainingOps = remainingIndices.map(i => current.edl.operations[i]);
      const remainingChecked = remainingIndices.map(i => checked[i] !== false);
      const rebased: Proposal = { ...current, edl: { ...current.edl, baseRevision: newRevision, operations: remainingOps } };
      setProposal(rebased);
      setChecked(remainingChecked);
      setContinuing(c => (c ? { ...c, edl: rebased.edl } : c));
      setRoughCutApplied(true);
      setPolishUnlocked(true);
      setStatus('Rough cut applied as one undo checkpoint. Polish ops remain to review.');
      return;
    }
    setProposal(null);
    setProposalId(null);
    setContinuing(null);
    setChecked([]);
    setRoughCutApplied(false);
    setPolishUnlocked(false);
    setLearnedRecorded({ preset: false, pace: false });
    setStatus(stage === 'polish' ? 'Polish applied as one undo checkpoint.' : 'Approved edits applied as one undo checkpoint.');
  });

  const request = () => task(async () => {
    const text = mode === 'wizard' ? `${goal}\nStyle: ${style}.\n${instruction}` : instruction;
    // The conversation context: an explicit Continue, or — for a follow-up
    // with a non-empty instruction — the latest proposal.
    const prior: Continuing | null = continuing ?? (proposal && proposalId ? { id: proposalId, edl: proposal.edl, summary: proposal.edl.summary } : null);
    const effectiveContinuing = instruction.trim() && prior ? prior : null;
    if (pendingModifiedOp) {
      recordDirectorFeedback([{ opType: pendingModifiedOp, outcome: 'modified' }]);
      setPendingModifiedOp(null);
      refreshFeedback();
    }
    const learned = buildLearnedPreferences();
    setMessages(m => [...m.slice(-15), { role: 'You', text }]);
    setProposal(null); setTitles(null);
    const result = await invoke<Proposal>('director_propose', { projectId: project.id, revision: project.revision, config, instruction: text, consent, mode: 'edit', previousProposalId: effectiveContinuing?.id ?? null, previousEdl: effectiveContinuing?.edl ?? null, learnedPreferences: learned || null });
    const current = useProject.getState().project;
    if (!current) throw new Error('Project closed while requesting.');
    compileEdl(current, result.edl);
    const id = result.id ?? `proposal-${Date.now()}`;
    setProposal(result); setProposalId(id); setChecked(result.edl.operations.map(() => true));
    setRoughCutApplied(false); setPolishUnlocked(false); setLearnedRecorded({ preset: false, pace: false });
    setMessages(m => [...m.slice(-15), { role: 'Director', text: result.edl.summary }]); setStatus('Proposal validated. Nothing has changed yet.');
    recordHistory('edit', text, result.edl.summary, result.edl);
  });
  const requestTitles = () => task(async () => {
    const text = `Suggest 5 video titles and 3 opening hooks for this project. Base them only on the asset names and transcript in the context.${goal ? ` Topic: ${goal}.` : ''}${instruction ? ` Note: ${instruction}` : ''}`;
    setTitles(null);
    const learned = buildLearnedPreferences();
    const result = await invoke<Proposal>('director_propose', { projectId: project.id, revision: project.revision, config, instruction: text, consent, mode: 'titles', previousProposalId: null, previousEdl: null, learnedPreferences: learned || null });
    if (!result.text) throw new Error('Provider returned no titles.');
    setTitles(result.text);
    setMessages(m => [...m.slice(-15), { role: 'Director', text: 'Titles & hooks ready — copy what you like, nothing was applied.' }]);
    setStatus('Titles & hooks ready. Nothing was applied to the timeline.');
    recordHistory('titles', text, 'Titles & hooks');
  });
  const measureLoudness = async () => {
    if (!project || !selected) return;
    setLoudness(''); setLoudnessError('');
    try {
      const result = await invoke<LoudnessResult>('measure_loudness', { projectId: project.id, revision: project.revision, clipId: selected });
      setLoudness(`Integrated ${result.integratedLufs.toFixed(1)} LUFS · suggest ${result.gainDb >= 0 ? '+' : ''}${result.gainDb.toFixed(1)} dB to reach ${result.targetLufs} LUFS (suggestion only — audio file unchanged)`);
    } catch (e) {
      setLoudnessError(String(e));
    }
  };
  function continueFromHistory(entry: DirectorHistoryEntry) {
    if (!entry.edl) return;
    setContinuing({ id: `history-${entry.at}`, edl: entry.edl, summary: entry.summary });
    setMode('chat');
    setStep(2);
  }
  function continueCurrentProposal() {
    if (!proposal || !proposalId) return;
    setContinuing({ id: proposalId, edl: proposal.edl, summary: proposal.edl.summary });
    setMode('chat');
  }
  function redoOp(index: number) {
    if (!proposal || !proposalId) return;
    setInstruction(`Redo operation ${index + 1} (${summaries[index] ?? `operation ${index + 1}`}): `);
    setContinuing({ id: proposalId, edl: proposal.edl, summary: proposal.edl.summary });
    setPendingModifiedOp(proposal.edl.operations[index].type);
    setMode('chat');
    setTimeout(() => instructionRef.current?.focus(), 0);
  }
  function discardProposal() {
    setProposal(null);
    setProposalId(null);
    setContinuing(null);
    setChecked([]);
    setPendingModifiedOp(null);
    setRoughCutApplied(false);
    setPolishUnlocked(false);
    setLearnedRecorded({ preset: false, pace: false });
    setStatus('Proposal discarded. Timeline unchanged.');
  }
  const renderOpRow = (index: number) => <li key={index}>
    <label>
      <input type="checkbox" checked={checked[index] !== false} onChange={e => setChecked(prev => { const next = [...prev]; while (next.length <= index) next.push(true); next[index] = e.target.checked; return next; })} />
      {' '}{summaries[index]}
    </label>
    {' '}<button className="button secondary" disabled={working} onClick={() => redoOp(index)}>Redo</button>
  </li>;
  const formatSigned = (value: number) => `${value >= 0 ? '+' : ''}${value.toFixed(2)}`;
  return <section className="director-section" aria-label="AI Director">
    <div className="director-bar"><div><span className="eyebrow">YOUR INTENT. YOUR FINAL CUT.</span><h2>AI Director <span>BYOK</span></h2><p>Describe the edit. Review every change. Approve once.</p></div><button className="button secondary" aria-expanded={open} onClick={() => setOpen(v => !v)}>{open ? 'Close Director' : 'Open AI Director'}</button></div>
    {open && <div className="director-layout"><div className="director-setup">
      <div className="director-tabs" role="tablist" aria-label="Director mode"><button role="tab" aria-selected={mode === 'wizard'} onClick={() => setMode('wizard')}>Director Wizard</button><button role="tab" aria-selected={mode === 'chat'} onClick={() => setMode('chat')}>Chat-to-Edit</button></div>
      {!native && <p className="director-notice">Desktop required for credentials and model requests. This browser can validate and review an EDL locally; no API keys are accepted here.</p>}
      <details open={step === 1}><summary>1. Choose your model & credential storage</summary><fieldset disabled={working}>
        <label>Provider<select value={config.provider} onChange={e => changeProvider(e.target.value as Provider)}>{providers.map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>
        <label>Model<select value={modelMode === 'custom' ? '__custom' : config.model} onChange={e => pickModel(e.target.value)}>{models.map(m => <option key={m.id} value={m.id}>{m.recommended ? `★ ${m.id}` : m.id}</option>)}<option value="__custom">Custom model…</option></select></label>
        {modelMode === 'custom' && <label>Custom model id<input value={customModel} onChange={e => { setCustomModel(e.target.value); setConfig(c => ({ ...c, model: e.target.value })); setDirectorModel(config.provider, e.target.value); }} maxLength={200} placeholder="Exact model ID from your provider" spellCheck={false} /></label>}
        <button className="button secondary" disabled={!native || modelsLoading || (cloud && !hasKey && !['claude', 'bedrock', 'vertex'].includes(config.provider))} onClick={() => void refreshModels(config.provider, key.current?.value ?? '', config.endpoint, true, config.model)}>{modelsLoading ? 'Loading models…' : 'Refresh models'}</button>
        {modelsNote && <p>{modelsNote}</p>}
        {modelsError && <p role="alert" className="error">{modelsError}</p>}
        {['compatible', 'ollama', 'bedrock', 'vertex'].includes(config.provider) && <label>Full API endpoint<input value={config.endpoint} onChange={e => setConfig({ ...config, endpoint: e.target.value })} placeholder={config.provider === 'compatible' ? 'https://your-host/v1/chat/completions' : 'Full model request URL'} /></label>}
        {config.provider === 'bedrock' && <p>Uses a Bedrock API key (bearer token), not an AWS access-key/secret pair.</p>}
        {config.provider === 'vertex' && <p>Uses a Google OAuth access token. Tokens expire; replace yours when needed. Use the Google publisher generateContent endpoint.</p>}
        {cloud && <><label>Credential storage<select value={storage} disabled={!native} onChange={e => setStorage(e.target.value as 'os' | 'session')}><option value="os">OS credential store (recommended)</option><option value="session">Session only — cleared on app exit</option></select></label><label>API key / access token<input ref={key} type="password" autoComplete="off" spellCheck={false} disabled={!native} maxLength={8192} onChange={e => setHasKey(!!e.target.value)} placeholder={native ? 'Stored only by Rust, never in project data' : 'Available in the desktop app'} /></label><div className="director-actions"><button className="button secondary" disabled={!native || !config.model} onClick={() => void task(async () => { const secret = key.current?.value; if (key.current) key.current.value = ''; if (!secret) throw new Error('Enter a credential.'); await invoke('director_credential', { config, key: secret, storage }); setStatus(storage === 'os' ? 'Saved in the OS credential store.' : 'Stored in Rust memory for this session only. Existing OS credentials are unchanged.'); })}>Save credential</button><button className="button secondary" disabled={!native || !config.model} onClick={() => void task(async () => { await invoke('director_credential', { config, key: null, storage: null }); setStatus('Saved OS/session credential removed. Environment credentials, if configured, are unchanged.'); })}>Forget credential</button></div><p>No silent fallback: if the OS store is unavailable, explicitly choose Session only and enter the key again.</p></>}
        <label>Maximum output tokens<input type="number" min={256} max={8192} value={config.maxTokens} onChange={e => setConfig({ ...config, maxTokens: Number(e.target.value) })} /></label><p>No automatic provider fallback. One request at a time; one rate-limit retry. Pricing varies by model; the token cap is not a dollar-budget guarantee.</p>
      </fieldset><button className="button secondary" onClick={() => setStep(2)}>Continue to edit brief</button></details>
      <div className="director-brief"><h3>2. {mode === 'wizard' ? 'Shape your edit' : 'Tell the Director what to change'}</h3>
        {continuing && <p className="director-notice">Continuing: {continuing.summary} <button className="button secondary" onClick={() => setContinuing(null)}>Clear</button></p>}
        {mode === 'wizard' && <><label>Editing goal<input value={goal} onChange={e => setGoal(e.target.value)} maxLength={1000} placeholder="Turn this interview into a concise opening" /></label><label>Editorial direction<select value={style} onChange={e => setStyle(e.target.value)}><option>Preserve original pacing</option><option>Tighter cuts with natural pauses</option><option>Documentary with B-roll</option><option>Short-form with kinetic captions</option></select></label></>}
        <label>{mode === 'wizard' ? 'Constraints and details' : 'Edit request'}<textarea ref={instructionRef} value={instruction} onChange={e => setInstruction(e.target.value)} maxLength={3000} rows={4} placeholder={continuing ? 'Revise: e.g. make it faster, use fewer captions…' : 'Keep the opening sentence. Duck music under dialogue. Do not change locked tracks.'} /></label>
        <p>Up to 100 assets, 200 clips and 8,000 transcript characters. Paths and media bytes are excluded. Existing caption timings are the transcript; this does not transcribe media. Each request uses current project context, not earlier chat messages.</p>
        <button className="button secondary" disabled={!native || working} onClick={() => void task(async () => { setContext(await invoke('director_context', { projectId: project.id, revision: project.revision })); })}>Inspect outbound context</button>
        {context && <details><summary>{context.bytes.toLocaleString()} bytes · {context.omittedAssets} assets / {context.omittedClips} clips omitted</summary><pre>{JSON.stringify(context.data, null, 2)}</pre></details>}
        {cloud && <label className="consent"><input type="checkbox" checked={consent} disabled={!native} onChange={e => setConsent(e.target.checked)} />I approve sending this brief and bounded catalog/transcript context to {providers.find(p => p[0] === config.provider)?.[1]}.</label>}
        <button className="button primary" disabled={!native || working || busy || !config.model || (cloud && !consent) || !(instruction.trim() || goal.trim())} onClick={() => void request()}>{working ? 'Waiting for a validated proposal…' : 'Generate edit proposal'}</button>
        <button className="button secondary" disabled={!native || working || busy || !config.model || (cloud && !consent)} onClick={() => void requestTitles()}>{working ? 'Working…' : 'Suggest titles & hooks'}</button>
        <button className="button secondary" disabled={!native || working || !selected} onClick={() => void measureLoudness()}>Measure loudness</button>
        {!selected && <p>Select a clip on the timeline to measure its loudness.</p>}
        {loudness && <p role="status">{loudness}</p>}
        {loudnessError && <p role="alert" className="error">{loudnessError}</p>}
        {titles && <div className="titles-result"><h4>Titles & hooks</h4>
          {titleSections.length > 0 ? <>
            {titleSections.map(s => <div key={s.title}><h5>{s.title}</h5><pre>{s.body}</pre><button className="button secondary" onClick={() => { void navigator.clipboard.writeText(s.body); setCopiedSection(s.title); setTimeout(() => setCopiedSection(null), 2000); }}>{copiedSection === s.title ? 'Copied ✓' : `Copy ${s.title}`}</button></div>)}
            <button className="button secondary" onClick={() => { void navigator.clipboard.writeText(titles); setCopied(true); setTimeout(() => setCopied(false), 2000); }}>{copied ? 'Copied ✓' : 'Copy all'}</button>
          </> : <>
            <pre>{titles}</pre><button className="button secondary" onClick={() => { void navigator.clipboard.writeText(titles); setCopied(true); setTimeout(() => setCopied(false), 2000); }}>{copied ? 'Copied ✓' : 'Copy all'}</button>
          </>}
        </div>}
      </div>
      {history.length > 0 && <div className="director-history"><h4>Recent Director requests</h4><ol>{history.slice().reverse().map((h, i) => <li key={`${h.at}-${i}`}><span>{h.kind === 'titles' ? 'Titles' : 'Edit'} · {new Date(h.at).toLocaleString()}</span><p>{h.summary}</p><button className="button secondary" onClick={() => { setInstruction(h.instruction); setMode('chat'); setStep(2); }}>Reuse</button>{h.edl && <button className="button secondary" onClick={() => continueFromHistory(h)}>Continue this proposal</button>}</li>)}</ol></div>}
      {feedbackNonEmpty && <div className="director-learned"><h4>What the Director learned</h4>
        <ul>{feedbackEntries.map(([opType, counts]) => { const total = counts.accepted + counts.rejected + counts.modified; return <li key={opType}>{directorOpLabel(opType)}: {counts.accepted}/{total} accepted</li>; })}</ul>
        {topLearned && <p>{topLearned}</p>}
        {learningNote && <p role="status">{learningNote}</p>}
        <button className="button secondary" onClick={() => { resetDirectorFeedback(); setFeedback(getDirectorFeedback()); setLearningNote(''); }}>Reset learning</button>
      </div>}
      <details className="edl-import"><summary>Review a typed EDL JSON proposal</summary><label>EDL JSON<textarea rows={7} value={raw} onChange={e => setRaw(e.target.value)} maxLength={262144} spellCheck={false} /></label><button className="button secondary" disabled={working || busy} onClick={() => { setError(''); try { const edl = parseEdl(raw); compileEdl(project, edl); const id = `imported-${edl.baseRevision}-${Date.now()}`; setProposal({ edl }); setProposalId(id); setChecked(edl.operations.map(() => true)); setRoughCutApplied(false); setPolishUnlocked(false); setLearnedRecorded({ preset: false, pace: false }); setStatus('Local EDL validated. Nothing has changed yet.'); } catch (e) { setError(String(e)); } }}>Validate proposal</button></details>
    </div><aside className="director-review" aria-label="Chat and edit approval"><h3>3. Review & approve</h3><p className="director-badge">Propose → validate → approve → one undo</p>
      <div className="director-messages" aria-live="polite">{messages.map((m, i) => <div key={i}><strong>{m.role}</strong><p>{m.text}</p></div>)}</div>
      {proposal ? <><h4>{proposal.edl.summary}</h4><p>{proposal.edl.operations.length} operations · based on revision {proposal.edl.baseRevision}</p>
        {proposal.plan ? <p>{(['pace', 'cuts', 'captions', 'grade', 'music'] as const).map(field => <span key={field} style={{ display: 'inline-block', border: '1px solid var(--color-border-strong)', borderRadius: 4, padding: '2px 6px', marginRight: 6, marginBottom: 4 }}>{field.charAt(0).toUpperCase() + field.slice(1)}: {proposal.plan![field]}</span>)}</p> : <p>Plan not returned by this model.</p>}
        {proposal.qaDropped && proposal.qaDropped.length > 0 && <p>QA dropped {proposal.qaDropped.length} op(s): {proposal.qaDropped.map(q => `#${q.index + 1}: ${q.reason}`).join('; ')} (deterministic Rust validation — these operations were removed before review.)</p>}
        {proposal.diff && <p>vs previous: +{proposal.diff.added} added, −{proposal.diff.removed} removed, ~{proposal.diff.changed} changed</p>}
        {proposal.gradeSuggestion && <p><span style={{ display: 'inline-block', border: '1px solid var(--color-border-strong)', borderRadius: 4, padding: '2px 6px', marginRight: 6 }}>Grade suggestion: brightness {formatSigned(proposal.gradeSuggestion.brightness)} · contrast {formatSigned(proposal.gradeSuggestion.contrast)} · saturation {formatSigned(proposal.gradeSuggestion.saturation)}{proposal.gradeSuggestion.cube ? ` · cube: ${proposal.gradeSuggestion.cube}` : ''}</span></p>}
        {proposal.captionPreset && <p><span style={{ display: 'inline-block', border: '1px solid var(--color-border-strong)', borderRadius: 4, padding: '2px 6px', marginRight: 6 }}>Caption preset: {proposal.captionPreset}</span></p>}
        {isStaged ? <>
          <h5>Stage 1 — Rough cut</h5>
          <ol>{stage1Indices.map(renderOpRow)}</ol>
          <div className="director-actions"><button className="button primary" disabled={!!stale || busy || working || !stage1Indices.length || !stage1Indices.some(i => checked[i] !== false)} onClick={() => void applyOps(stage1Indices, 'rough')}>Apply rough cut</button></div>
          <h5>Stage 2 — Polish</h5>
          <ol>{stage2Indices.map(renderOpRow)}</ol>
          <div className="director-actions">{roughCutApplied || polishUnlocked
            ? <button className="button primary" disabled={!!stale || busy || working || !stage2Indices.length || !stage2Indices.some(i => checked[i] !== false)} onClick={() => void applyOps(stage2Indices, 'polish')}>Apply polish</button>
            : <button className="button secondary" disabled={working} onClick={() => setPolishUnlocked(true)}>Skip to polish</button>}
          </div>
        </> : <>
          <ol>{allIndices.map(renderOpRow)}</ol>
          <div className="director-actions"><button className="button primary" disabled={!!stale || busy || working || !proposal.edl.operations.length || !checked.some(c => c)} onClick={() => void applyOps(allIndices, 'all')}>Approve & apply edits</button></div>
        </>}
        {stale && <p role="alert" className="error">Stale proposal: the project changed. Generate a new proposal.</p>}
        <div className="director-actions"><button className="button secondary" disabled={working} onClick={() => continueCurrentProposal()}>Continue</button><button className="button secondary" disabled={working} onClick={() => discardProposal()}>Discard proposal</button></div>
        <details><summary>Validated EDL and request trace</summary><pre>{JSON.stringify(proposal, null, 2)}</pre><p>Trace is session-only. Provider usage is shown when reported. Cost is not estimated. No shell or external tools were invoked.</p></details></> : <div className="director-empty"><strong>You are the final decision.</strong><p>Generated edits appear here before anything touches your timeline. Invalid output is rejected, never silently repaired.</p></div>}
      <div role="status" aria-live="polite">{working ? 'Working… No edits are applied while a request is running.' : status}</div>{error && <p className="error" role="alert">{error}</p>}
    </aside></div>}
  </section>;
}
