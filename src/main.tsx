import { Component, StrictMode, useEffect, useRef, useState, type ReactNode } from 'react';
import { createRoot } from 'react-dom/client';
import { ArrowRight, Clapperboard, FolderOpen, HardDrive, LockKeyhole, Plus, Save, Scissors, ShieldCheck, Sparkles, X } from 'lucide-react';
import { native, useProject } from './store';
import { getRecentProjects, type RecentProject } from './preferences';
import './design-tokens.css';
import './styles.css';
import './analysis.css';
import { MediaLibrary } from './MediaLibrary';
import { Timeline } from './Timeline';
import { Preview } from './Preview';
import { AnalysisPanel } from './AnalysisPanel';
import { DirectorPanel } from './DirectorPanel';
import { FinishingPanel } from './FinishingPanel';
import { ExportModal } from './ExportModal';
import { SettingsModal } from './SettingsModal';
import { SetupCheck } from './SetupCheck';

/** Last-resort guard: a render crash must never take the whole app (and
 * the project on disk) down without telling the user what happened. */
class ErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };
  static getDerivedStateFromError(error: Error) { return { error }; }
  componentDidCatch(error: Error) { console.error(error); }
  render() {
    if (this.state.error) {
      return <div className="error-boundary" role="alert">
        <h2>Something went wrong in the editor.</h2>
        <p>Your project file on disk is untouched. Reload to try again — autosave may have a newer backup.</p>
        <pre>{this.state.error.message}</pre>
        <button className="button primary" onClick={() => window.location.reload()}>Reload StudioCut</button>
      </div>;
    }
    return this.props.children;
  }
}

function App() {
  const { project, saved, busy, autosaving, recovery, message, error, createProject, openProject, openProjectByPath, saveProject, autosave, recoverProject, discardRecovery } = useProject();
  const [tab, setTab] = useState('Media');
  const [name, setName] = useState('Untitled project');
  const [format, setFormat] = useState('1920x1080');
  const [fps, setFps] = useState(30);
  const [recent, setRecent] = useState<RecentProject[]>(getRecentProjects);
  const [panels, setPanels] = useState({ media: true, inspector: true, analysis: true, director: true });
  const togglePanel = (key: 'media' | 'inspector' | 'analysis' | 'director') => setPanels(p => ({ ...p, [key]: !p[key] }));
  const dialog = useRef<HTMLDialogElement>(null);
  const file = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const warn = (e: BeforeUnloadEvent) => { if (project && !saved) e.preventDefault(); };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [project, saved]);
  // Autosave: 2s after the last edit, desktop only, only while dirty.
  const revision = project?.revision;
  useEffect(() => {
    if (!native || !project || saved || busy) return;
    const timer = setTimeout(() => { void autosave(); }, 2000);
    return () => clearTimeout(timer);
  }, [revision, project?.id, saved, busy]);
  useEffect(() => { setRecent(getRecentProjects()); }, [project?.id]);
  const mayReplace = () => !project || saved || window.confirm('This project has not been saved. Continue without saving?');
  const open = () => { if (mayReplace()) { if (native) void openProject(); else file.current?.click(); } };
  const newProject = () => { if (mayReplace()) dialog.current?.showModal(); };
  return <>
    <header className="app-header"><a className="brand" href="#" aria-label="StudioCut home"><span className="brand-icon"><Scissors size={21} /></span>StudioCut<span className="ai-badge">AI</span></a><div className="header-center"><span className="status-dot" />Local-first workspace</div><span className="header-end"><span className="version">TIMELINE EDITOR</span><SettingsModal /></span></header>
    <div className="app-body"><aside className="rail" aria-label="Workspace tools"><div className="rail-top"><span className="rail-active" aria-label="Editor"><Clapperboard size={21} /></span><span title="AI tools arrive in a later milestone" className="rail-muted"><Sparkles size={21} /></span></div><span className="rail-bottom" title="Local files stay on your machine"><ShieldCheck size={21} /></span></aside>
      <main><div className="workspace-heading"><div><div className="eyebrow">YOUR CREATIVE SPACE</div><h1>{project?.name ?? 'Make room for your next story.'}</h1><p>{project ? `${project.settings.width} × ${project.settings.height} · ${project.settings.fps} fps · Project format v${project.schemaVersion}` : 'A fresh timeline. A little possibility. Everything stays yours.'}</p></div><div className="heading-actions">{project && <button className="button secondary" onClick={newProject} disabled={busy}><Plus size={16} />New</button>}<button className="button secondary open-button" onClick={open} disabled={busy}><FolderOpen size={16} />Open project</button></div></div>
        <section className="workspace" aria-label="Editing workspace"><div className="workspace-toolbar"><div className="project-label"><span className="status-dot" />{project ? 'Project workspace' : 'Getting started'}<span className="divider" /><span className="subtle">{project ? (autosaving ? 'Saving…' : saved ? 'Saved' : 'Unsaved changes') : 'No project open'}</span></div><div className="toolbar-actions">{project && <><button className={`toggle-button${panels.media ? ' on' : ''}`} onClick={() => togglePanel('media')} aria-pressed={panels.media} aria-label="Toggle Media Bin panel">Media Bin</button><button className={`toggle-button${panels.inspector ? ' on' : ''}`} onClick={() => togglePanel('inspector')} aria-pressed={panels.inspector} aria-label="Toggle Inspector panel">Inspector</button><button className={`toggle-button${panels.analysis ? ' on' : ''}`} onClick={() => togglePanel('analysis')} aria-pressed={panels.analysis} aria-label="Toggle Analysis and Captions panel">Analysis</button><button className={`toggle-button${panels.director ? ' on' : ''}`} onClick={() => togglePanel('director')} aria-pressed={panels.director} aria-label="Toggle AI Director panel">AI Director</button></>}<ExportModal /><span className="local-pill"><HardDrive size={12} /> ON DEVICE</span>{project && <button className="icon-button" onClick={() => void saveProject()} disabled={busy} aria-label={native ? 'Save project' : 'Download project'}><Save size={17} /></button>}</div></div>
          {recovery && project && <div className="recovery-banner" role="alert"><div><strong>Autosave backup found</strong><p>A backup from {new Date(recovery.backupModifiedMs).toLocaleString()} is newer than the saved file. Recover it, or discard the backup.</p></div><div className="recovery-actions"><button className="button primary" onClick={() => void recoverProject()} disabled={busy}>Recover backup</button><button className="button secondary" onClick={() => void discardRecovery()} disabled={busy}>Discard backup</button></div></div>}
          <div className="editor-grid">{panels.media ? <section className="media-panel"><div className="tabs" role="tablist" aria-label="Project panel">{['Media', 'Project'].map(t => <button key={t} role="tab" aria-selected={tab === t} onClick={() => setTab(t)}>{t}</button>)}</div><div className="panel-content" role="tabpanel"><div className="panel-section-label">{tab === 'Media' ? 'PROJECT LIBRARY' : 'PROJECT DETAILS'}<span>{tab === 'Media' ? `${project?.assets.length ?? 0} items` : `v${project?.schemaVersion ?? 1}`}</span></div>{tab === 'Media' ? <MediaLibrary /> : <dl className="project-details"><dt>Project</dt><dd>{project?.name ?? 'Not created'}</dd><dt>Resolution</dt><dd>{project ? `${project.settings.width} × ${project.settings.height}` : '—'}</dd><dt>Frame rate</dt><dd>{project ? `${project.settings.fps} fps` : '—'}</dd><dt>Audio sample rate</dt><dd>48 kHz</dd><dt>Timeline</dt><dd>{project?.tracks.length ?? 0} tracks · {project?.tracks.reduce((n, t) => n + t.clips.length, 0) ?? 0} clips</dd><dt>Undo history</dt><dd>Last 50 edits · session only</dd></dl>}</div><div className="panel-footer"><LockKeyhole size={13} />Local by default. Cloud only with consent.</div></section> : <section className="media-panel media-panel-hidden" aria-label="Media panel hidden"><button className="button secondary" onClick={() => togglePanel('media')}>Show Media Bin</button></section>}
            {project ? <Preview /> : <section className="viewer-panel"><div className="viewer"><div className="welcome"><div className="welcome-symbol"><Clapperboard size={31} strokeWidth={1.4} /></div><span className="eyebrow">FROM FIRST IDEA TO FINAL CUT</span><h2>Your story starts here.</h2><p>Create a local project.<br />Keep your originals. Own every edit.</p><button className="button primary" onClick={newProject} disabled={busy}><Plus size={17} />Create a project<ArrowRight size={15} /></button><span className="viewer-footnote">Local-first. Yours by default.</span>{native && recent.length > 0 && <div className="recent-projects"><h3>Recent projects</h3>{recent.map(r => <button key={r.path} className="recent-item" disabled={busy} onClick={() => void openProjectByPath(r.path)}><strong>{r.name}</strong><small title={r.path}>{r.path}</small></button>)}</div>}<SetupCheck /></div></div></section>}
          </div>{project ? <><Timeline />{panels.inspector && <FinishingPanel />}{panels.analysis && <AnalysisPanel />}{panels.director && <DirectorPanel />}</> : <div className="timeline-empty"><p>Every great edit starts with a project</p><span>Create or open a project to get started.</span></div>}
        </section>
        <section className="principles" aria-label="Workspace principles"><div><ShieldCheck /><span><strong>Your files stay yours</strong><p>Original media is never overwritten.</p></span></div><div><HardDrive /><span><strong>Built for your machine</strong><p>Local project files. No mandatory cloud.</p></span></div><div><Scissors /><span><strong>Every edit, reversible</strong><p>Linked clips. Frame-accurate trims. Undo.</p></span></div></section>
        <div className="feedback" role="status" aria-live="polite">{busy ? 'Working…' : message}</div>{error && <div className="error" role="alert">{error}</div>}
      </main></div>
    <footer className="statusbar"><span><span className="status-dot" />{native ? 'Desktop · local workspace' : 'Browser preview · reattach media for playback'}</span><span>STEP 03 <span className="footer-divider">/</span> TIMELINE & PREVIEW</span></footer>
    <input ref={file} aria-label="Open project file" type="file" accept=".studiocut" hidden onChange={e => { const selected = e.target.files?.[0]; if (selected) void openProject(selected); e.target.value = ''; }} />
    <dialog ref={dialog} className="project-dialog"><form onSubmit={async e => { e.preventDefault(); const [w, h] = format.split('x').map(Number); if (await createProject(name, w, h, fps)) dialog.current?.close(); }}><div className="dialog-heading"><span className="eyebrow">START SOMETHING NEW</span><button type="button" className="icon-button" aria-label="Close dialog" onClick={() => dialog.current?.close()}><X size={19} /></button></div><h2>Create a project</h2><p>Your original files always stay untouched.</p><label htmlFor="project-name">Project name</label><input id="project-name" autoFocus value={name} onChange={e => setName(e.target.value)} maxLength={120} required /><div className="form-row"><div><label htmlFor="resolution">Canvas size</label><select id="resolution" value={format} onChange={e => setFormat(e.target.value)}><option value="1920x1080">1920 × 1080 · Landscape</option><option value="1080x1920">1080 × 1920 · Portrait</option><option value="3840x2160">3840 × 2160 · 4K</option><option value="1080x1080">1080 × 1080 · Square</option></select></div><div><label htmlFor="frame-rate">Frame rate</label><select id="frame-rate" value={fps} onChange={e => setFps(Number(e.target.value))}>{[24, 25, 30, 50, 60].map(f => <option key={f} value={f}>{f} fps</option>)}</select></div></div>{error && <p className="error" role="alert">{error}</p>}<button className="button primary create-submit" disabled={busy || !name.trim()} type="submit">Create project<ArrowRight size={16} /></button><span className="dialog-note"><LockKeyhole size={12} />No sign-up. No network request.</span></form></dialog>
  </>;
}
createRoot(document.getElementById('root')!).render(<StrictMode><ErrorBoundary><App /></ErrorBoundary></StrictMode>);
