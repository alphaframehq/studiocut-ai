import { create } from 'zustand';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { validateProject, type Project, type Marker } from './project';
import { editTimeline, emptyHistory, remember, travel, newId, type Edit, type History } from './timeline';
import { addRecentProject } from './preferences';

export const native = isTauri();
export type RecoveryInfo = { backupPath: string; backupModifiedMs: number; projectModifiedMs: number };
type State = {
  project: Project | null; saved: boolean; busy: boolean; message: string; error: string;
  projectPath: string | null; autosaving: boolean; recovery: RecoveryInfo | null;
  history: History; frame: number; playing: boolean; selected: string | null;
  seek: (frame: number) => void;
  replaceTracks: (tracks: import('./timeline').Track[], projectId: string, revision: number) => Promise<void>;
  edit: (edit: Edit) => Promise<void>;
  undo: (redo?: boolean) => Promise<void>;
  applyEdl: (edl: import('./edl').Edl) => Promise<boolean>;
  applyAnalysis: (action: { type: 'cuts'; cuts: import('./analysis').Cut[] } | { type: 'captions'; clipId: string; transcript: import('./analysis').Transcript; preset: import('./timeline').CaptionPreset }, projectId: string, revision: number) => Promise<void>;
  createProject: (name: string, width: number, height: number, fps: number) => Promise<boolean>;
  openProject: (file?: File) => Promise<void>;
  openProjectByPath: (path: string) => Promise<void>;
  saveProject: () => Promise<void>;
  autosave: () => Promise<void>;
  recoverProject: () => Promise<void>;
  discardRecovery: () => Promise<void>;
  setMarkers: (markers: Marker[]) => Promise<void>;
  addMarker: (frame: number) => Promise<void>;
  removeMarker: (id: string) => Promise<void>;
  renameAsset: (assetId: string, name: string) => Promise<void>;
  removeAsset: (assetId: string) => Promise<void>;
  relinkAsset: (assetId: string, path: string) => Promise<void>;
  importMedia: (paths?: string[]) => Promise<void>;
};
export const useProject = create<State>((set, get) => ({
  project: null, saved: false, busy: false, message: '', error: '', projectPath: null, autosaving: false, recovery: null, history: emptyHistory(), frame: 0, playing: false, selected: null,
  seek: frame => set({ frame: Math.max(0, Math.round(frame)), playing: false }),
  replaceTracks: async (tracks, projectId, revision) => {
    const { project, history, busy } = get();
    if (!project || busy) throw new Error('Project is unavailable or busy.');
    if (project.id !== projectId || project.revision !== revision) throw new Error('Project changed; retry the finishing edit.');
    set({ busy: true, playing: false, error: '' });
    try {
      const candidate = validateProject({ ...project, schemaVersion: 6, tracks, revision: revision + 1 });
      const next = native ? validateProject(await invoke('edit_timeline', { projectId, revision, tracks, undo: null })) : candidate;
      set({ project: next, saved: false, history: remember(history, project.tracks), message: 'Finishing updated (export only).' });
    } finally { set({ busy: false }); }
  },
  applyEdl: async edl => {
    const { project, history, busy } = get();
    if (!project || busy) return false;
    set({ busy: true, playing: false, error: '' });
    try {
      const { compileEdl } = await import('./edl');
      if (!edl.operations.length) throw new Error('Proposal has no edits to apply.');
      const candidate = compileEdl(project, edl);
      const next = native ? validateProject(await invoke('director_apply', { edl })) : candidate;
      set({ project: next, history: remember(history, project.tracks), saved: false, message: 'Approved proposal applied. Undo restores all edits together.', selected: null });
      return true;
    } catch (error) { set({ error: String(error) }); return false; }
    finally { set({ busy: false }); }
  },
  edit: async edit => {
    const { project, history, busy } = get();
    if (!project || busy) return;
    set({ busy: true, playing: false, error: '' });
    try {
      const candidate = editTimeline(project, edit);
      const next = native ? validateProject(await invoke('edit_timeline', { projectId: project.id, revision: project.revision, tracks: candidate.tracks, undo: null })) : candidate;
      set({ project: next, saved: false, history: remember(history, project.tracks), message: 'Timeline updated · originals unchanged.' });
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
  applyAnalysis: async (action, projectId, revision) => {
    const { project, history, busy } = get();
    if (!project || busy) return;
    if (project.id !== projectId || project.revision !== revision) { set({ error: 'Project changed; run analysis again.' }); return; }
    set({ busy: true, playing: false, error: '' });
    try {
      const { rippleDelete, addCaptions } = await import('./analysis');
      const candidate = action.type === 'cuts' ? rippleDelete(project, action.cuts) : addCaptions(project, action.clipId, action.transcript, action.preset);
      const args = action.type === 'cuts' ? { cuts: action.cuts } : { clipId: action.clipId, transcript: action.transcript, preset: action.preset };
      const next = native ? validateProject(await invoke(action.type === 'cuts' ? 'ripple_delete' : 'add_captions', { projectId, revision, ...args })) : candidate;
      set({ project: next, history: remember(history, project.tracks), saved: false, selected: null, frame: 0, message: action.type === 'cuts' ? 'Cuts applied across all tracks · one undo · originals unchanged.' : 'Caption clips added · one undo.' });
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
  undo: async (redo = false) => {
    const { project, history, busy } = get();
    if (!project || busy) return;
    set({ busy: true, playing: false, error: '' });
    try {
      const result = travel(project, history, redo ? 'redo' : 'undo');
      if (native) result.project = validateProject(await invoke('edit_timeline', { projectId: project.id, revision: project.revision, tracks: null, undo: !redo }));
      set({ ...result, saved: false, selected: null, message: redo ? 'Edit restored.' : 'Edit undone.' });
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
  importMedia: async paths => {
    if (get().busy) return;
    if (!native) { set({ error: 'Media import requires the desktop app with FFmpeg and FFprobe installed.' }); return; }
    set({ busy: true, playing: false, error: '', message: '' });
    try {
      const result = await invoke<{ project: Project; errors: string[] } | null>('import_media', { paths: paths ?? null });
      if (result) set({ project: validateProject(result.project), saved: false, message: `${result.project.assets.length} media assets indexed.`, error: result.errors.join('\n') });
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
  createProject: async (name, width, height, fps) => {
    if (get().busy) return false;
    set({ busy: true, playing: false, error: '', message: '' });
    try {
      const project = native ? validateProject(await invoke('create_project', { name, width, height, fps })) : validateProject({ schemaVersion: 1, id: newId(), name: name.trim(), revision: 0, createdAt: Date.now(), settings: { width, height, fps, audioSampleRate: 48000 }, assets: [], tracks: [], markers: [] });
      set({ project, saved: false, projectPath: null, recovery: null, history: emptyHistory(), frame: 0, selected: null, message: 'Project created. Import media to start editing.' });
      return true;
    } catch (e) { set({ error: String(e) }); return false; }
    finally { set({ busy: false }); }
  },
  openProject: async file => {
    if (get().busy) return;
    set({ busy: true, playing: false, error: '', message: '' });
    try {
      let data: unknown;
      let openedPath: string | null = null;
      if (native) {
        const opened = await invoke<{ project: Project; path: string } | null>('open_project');
        if (opened) { data = opened.project; openedPath = opened.path; }
      } else {
        if (!file) return;
        if (file.size > 1048576) throw new Error('Project file exceeds 1 MB.');
        data = JSON.parse(await file.text());
      }
      if (data) {
        const project = validateProject(data);
        set({ project, saved: true, projectPath: openedPath, recovery: null, history: emptyHistory(), frame: 0, selected: null, message: 'Project reopened. Browser playback needs local files reattached.' });
        if (openedPath) {
          addRecentProject(openedPath, project.name);
          const found = await invoke<RecoveryInfo | null>('check_recovery', { path: openedPath }).catch(() => null);
          if (found) set({ recovery: found });
        }
      }
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
  openProjectByPath: async path => {
    if (get().busy || !native) return;
    set({ busy: true, playing: false, error: '', message: '' });
    try {
      const project = validateProject(await invoke('open_project_path', { path }));
      set({ project, saved: true, projectPath: path, recovery: null, history: emptyHistory(), frame: 0, selected: null, message: 'Project reopened.' });
      addRecentProject(path, project.name);
      const found = await invoke<RecoveryInfo | null>('check_recovery', { path }).catch(() => null);
      if (found) set({ recovery: found });
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
  saveProject: async () => {
    const { project, busy } = get();
    if (!project || busy) return;
    set({ busy: true, error: '' });
    try {
      validateProject(project);
      if (native) {
        const path = await invoke<string | null>('save_project');
        if (path) { addRecentProject(path, project.name); set({ saved: true, projectPath: path, recovery: null, message: `Saved to ${path}` }); }
      } else {
        const url = URL.createObjectURL(new Blob([JSON.stringify(project, null, 2)], { type: 'application/json' }));
        const a = document.createElement('a'); a.href = url; a.download = `${project.name.replace(/[^a-z0-9_-]/gi, '_')}.studiocut`; a.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
        set({ saved: true, message: 'Project downloaded. Media stays at its original location; undo history is session-only.' });
      }
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
  autosave: async () => {
    const { project, saved, busy, autosaving } = get();
    if (!native || !project || saved || busy || autosaving) return;
    set({ autosaving: true });
    try {
      const path = await invoke<string | null>('save_project_silent');
      if (path) set({ saved: true, projectPath: path, message: `Autosaved to ${path}` });
      // No remembered path yet: stay dirty; the user saves once by hand.
    } catch {
      // Autosave never blocks editing: the toolbar keeps showing unsaved.
    } finally { set({ autosaving: false }); }
  },
  recoverProject: async () => {
    const { projectPath, busy } = get();
    if (!native || !projectPath || busy) return;
    set({ busy: true, error: '' });
    try {
      const project = validateProject(await invoke('recover_project', { path: projectPath }));
      set({ project, saved: false, recovery: null, history: emptyHistory(), frame: 0, selected: null, message: 'Recovered the autosave backup. Save to keep it.' });
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
  discardRecovery: async () => {
    const { projectPath } = get();
    if (!native || !projectPath) { set({ recovery: null }); return; }
    try { await invoke('discard_recovery', { path: projectPath }); } catch { /* backup may already be gone */ }
    set({ recovery: null });
  },
  // Markers and asset renames go through Rust so the workspace copy (the
  // one that gets saved) stays in sync; browser preview edits locally.
  setMarkers: async markers => {
    const { project, busy } = get();
    if (!project || busy) return;
    set({ busy: true, error: '' });
    try {
      const next = native
        ? validateProject(await invoke('set_markers', { markers }))
        : validateProject({ ...structuredClone(project), markers, revision: project.revision + 1 });
      set({ project: next, saved: false, message: 'Marker updated.' });
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
  addMarker: async frame => {
    const { project } = get();
    if (!project) return;
    const label = `Marker ${project.markers.length + 1}`;
    await get().setMarkers([...project.markers, { id: newId(), frame: Math.max(0, Math.round(frame)), label }].sort((a, b) => a.frame - b.frame));
  },
  removeMarker: async id => {
    const { project } = get();
    if (!project) return;
    await get().setMarkers(project.markers.filter(m => m.id !== id));
  },
  renameAsset: async (assetId, name) => {
    const { project, busy } = get();
    if (!project || busy) return;
    set({ busy: true, error: '' });
    try {
      const next = native
        ? validateProject(await invoke('rename_asset', { assetId, name }))
        : validateProject({ ...structuredClone(project), assets: project.assets.map(a => a.id === assetId ? { ...a, name: name.trim() } : a), revision: project.revision + 1 });
      set({ project: next, saved: false, message: 'Media renamed.' });
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
  removeAsset: async assetId => {
    const { project, busy } = get();
    if (!project || busy) return;
    set({ busy: true, error: '' });
    try {
      const next = native
        ? validateProject(await invoke('remove_asset', { assetId }))
        : (() => {
          if (project.tracks.flatMap(t => t.clips).some(c => c.assetId === assetId)) throw new Error('Remove the clips that use this media first');
          return validateProject({ ...structuredClone(project), assets: project.assets.filter(a => a.id !== assetId), revision: project.revision + 1 });
        })();
      set({ project: next, saved: false, message: 'Media removed from the project. Original file untouched.' });
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
  relinkAsset: async (assetId, path) => {
    const { project, busy } = get();
    if (!project || busy || !native) return;
    set({ busy: true, error: '' });
    try {
      const next = validateProject(await invoke('relink_asset', { assetId, path }));
      set({ project: next, saved: false, message: 'Media relinked to the new location.' });
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
}));
