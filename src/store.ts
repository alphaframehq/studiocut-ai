import { create } from 'zustand';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { validateProject, type Project } from './project';
import { editTimeline, emptyHistory, remember, travel, newId, type Edit, type History } from './timeline';

export const native = isTauri();
type State = {
  project: Project | null; saved: boolean; busy: boolean; message: string; error: string;
  history: History; frame: number; playing: boolean; selected: string | null;
  seek: (frame: number) => void;
  replaceTracks: (tracks: import('./timeline').Track[], projectId: string, revision: number) => Promise<void>;
  edit: (edit: Edit) => Promise<void>;
  undo: (redo?: boolean) => Promise<void>;
  applyEdl: (edl: import('./edl').Edl) => Promise<boolean>;
  applyAnalysis: (action: { type: 'cuts'; cuts: import('./analysis').Cut[] } | { type: 'captions'; clipId: string; transcript: import('./analysis').Transcript; preset: import('./timeline').CaptionPreset }, projectId: string, revision: number) => Promise<void>;
  createProject: (name: string, width: number, height: number, fps: number) => Promise<boolean>;
  openProject: (file?: File) => Promise<void>;
  saveProject: () => Promise<void>;
  importMedia: (paths?: string[]) => Promise<void>;
};
export const useProject = create<State>((set, get) => ({
  project: null, saved: false, busy: false, message: '', error: '', history: emptyHistory(), frame: 0, playing: false, selected: null,
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
      const project = native ? validateProject(await invoke('create_project', { name, width, height, fps })) : validateProject({ schemaVersion: 1, id: newId(), name: name.trim(), revision: 0, createdAt: Date.now(), settings: { width, height, fps, audioSampleRate: 48000 }, assets: [], tracks: [] });
      set({ project, saved: false, history: emptyHistory(), frame: 0, selected: null, message: 'Project created. Import media to start editing.' });
      return true;
    } catch (e) { set({ error: String(e) }); return false; }
    finally { set({ busy: false }); }
  },
  openProject: async file => {
    if (get().busy) return;
    set({ busy: true, playing: false, error: '', message: '' });
    try {
      let data: unknown;
      if (native) data = await invoke('open_project');
      else {
        if (!file) return;
        if (file.size > 1048576) throw new Error('Project file exceeds 1 MB.');
        data = JSON.parse(await file.text());
      }
      if (data) set({ project: validateProject(data), saved: true, history: emptyHistory(), frame: 0, selected: null, message: 'Project reopened. Browser playback needs local files reattached.' });
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
        if (path) set({ saved: true, message: `Saved to ${path}` });
      } else {
        const url = URL.createObjectURL(new Blob([JSON.stringify(project, null, 2)], { type: 'application/json' }));
        const a = document.createElement('a'); a.href = url; a.download = `${project.name.replace(/[^a-z0-9_-]/gi, '_')}.studiocut`; a.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
        set({ saved: true, message: 'Project downloaded. Media stays at its original location; undo history is session-only.' });
      }
    } catch (e) { set({ error: String(e) }); }
    finally { set({ busy: false }); }
  },
}));
