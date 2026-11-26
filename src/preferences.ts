// Shared, persisted user preferences — single source of truth.
//
// Before this module, Settings and the Export/Analysis panels each kept
// their own copy: the encoder preference was saved but never read by the
// Export dialog, and the whisper settings reset every time Settings was
// reopened (and could disagree with AnalysisPanel). Every reader/writer
// below goes through these helpers so the panels can never drift apart.
//
// Storage is plain localStorage (desktop app + browser preview alike).
// Nothing secret lives here — API keys stay in the OS keyring / session
// vault managed by SettingsModal + the Rust credentials module.

export const ENCODERS = [
  'auto',
  'libx264',
  'h264_nvenc',
  'hevc_videotoolbox',
  'h264_amf',
  'h264_qsv',
] as const;
export type EncoderPreference = (typeof ENCODERS)[number];

const ENCODER_KEY = 'studiocut.encoderPreference';
const WHISPER_BINARY_KEY = 'studiocut.whisperBinary';
const WHISPER_MODEL_KEY = 'studiocut.whisperModel';

export const DEFAULT_ENCODER: EncoderPreference = 'auto';
export const DEFAULT_WHISPER_BINARY = 'whisper-cli';
export const DEFAULT_WHISPER_MODEL = '';

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage unavailable (private mode, etc.) — preferences just don't persist.
  }
}

export function getEncoderPreference(): EncoderPreference {
  const saved = read(ENCODER_KEY);
  return (ENCODERS as readonly string[]).includes(saved ?? '')
    ? (saved as EncoderPreference)
    : DEFAULT_ENCODER;
}

export function setEncoderPreference(encoder: string): void {
  write(
    ENCODER_KEY,
    (ENCODERS as readonly string[]).includes(encoder) ? encoder : DEFAULT_ENCODER,
  );
}

export function getWhisperBinary(): string {
  return read(WHISPER_BINARY_KEY) ?? DEFAULT_WHISPER_BINARY;
}

export function setWhisperBinary(binary: string): void {
  write(WHISPER_BINARY_KEY, binary);
}

export function getWhisperModel(): string {
  return read(WHISPER_MODEL_KEY) ?? DEFAULT_WHISPER_MODEL;
}

export function setWhisperModel(model: string): void {
  write(WHISPER_MODEL_KEY, model);
}

// --- Transcription models (Phase 2d) -------------------------------------
// Per-provider selected cloud model, e.g. groq -> whisper-large-v3.
// API keys are never stored here — only the model id.
export type ListedModel = { id: string; recommended: boolean; path: string | null };

const MODEL_KEY_PREFIX = 'studiocut.transcriptionModel.';
const MODELS_CACHE_PREFIX = 'studiocut.transcriptionModels.';
const MODELS_CACHE_TTL_MS = 60 * 60 * 1000; // 1 hour

export function getTranscriptionModel(provider: string): string | null {
  return read(`${MODEL_KEY_PREFIX}${provider}`);
}

export function setTranscriptionModel(provider: string, model: string): void {
  write(`${MODEL_KEY_PREFIX}${provider}`, model);
}

export function getCachedTranscriptionModels(
  provider: string,
): { models: ListedModel[]; fetchedAt: number } | null {
  const raw = read(`${MODELS_CACHE_PREFIX}${provider}`);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as { fetchedAt: number; models: ListedModel[] };
    if (
      typeof parsed.fetchedAt !== 'number' ||
      !Array.isArray(parsed.models) ||
      Date.now() - parsed.fetchedAt > MODELS_CACHE_TTL_MS
    ) {
      return null;
    }
    return parsed;
  } catch {
    return null;
  }
}

export function setCachedTranscriptionModels(provider: string, models: ListedModel[]): void {
  write(
    `${MODELS_CACHE_PREFIX}${provider}`,
    JSON.stringify({ fetchedAt: Date.now(), models }),
  );
}

// --- Director models (Phase 3) ------------------------------------------
// Per-provider selected Director model, e.g. openai -> gpt-4o.
// API keys are never stored here — only the model id.
export type DirectorListedModel = { id: string; recommended: boolean; note: string | null };

const DIRECTOR_MODEL_PREFIX = 'studiocut.directorModel.';
const DIRECTOR_MODELS_CACHE_PREFIX = 'studiocut.directorModels.';

export function getDirectorModel(provider: string): string | null {
  return read(`${DIRECTOR_MODEL_PREFIX}${provider}`);
}

export function setDirectorModel(provider: string, model: string): void {
  write(`${DIRECTOR_MODEL_PREFIX}${provider}`, model);
}

export function getCachedDirectorModels(
  provider: string,
): { models: DirectorListedModel[]; fetchedAt: number } | null {
  const raw = read(`${DIRECTOR_MODELS_CACHE_PREFIX}${provider}`);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as { fetchedAt: number; models: DirectorListedModel[] };
    if (
      typeof parsed.fetchedAt !== 'number' ||
      !Array.isArray(parsed.models) ||
      Date.now() - parsed.fetchedAt > MODELS_CACHE_TTL_MS
    ) {
      return null;
    }
    return parsed;
  } catch {
    return null;
  }
}

export function setCachedDirectorModels(provider: string, models: DirectorListedModel[]): void {
  write(
    `${DIRECTOR_MODELS_CACHE_PREFIX}${provider}`,
    JSON.stringify({ fetchedAt: Date.now(), models }),
  );
}

// --- Style DNA (Phase 3) -------------------------------------------------
// One saved look + pace for the whole studio. Panels read it when they
// open (Export reads aspect/fill, Analysis reads caption preset + silence
// padding); nothing here is sent to any provider.
export type StyleDNA = {
  captionPreset: 'hormozi' | 'minimal' | 'documentary' | 'neon';
  aspect: 'landscape' | 'vertical' | 'square';
  fill: 'fit' | 'cover';
  pace: 'preserve' | 'tight' | 'documentary' | 'shortform';
  introStyle: 'hook-first' | 'cold-open' | 'title-card' | 'none';
  musicMood: 'none' | 'upbeat' | 'calm' | 'dramatic' | 'cinematic';
};

export const DEFAULT_STYLE_DNA: StyleDNA = {
  captionPreset: 'hormozi',
  aspect: 'landscape',
  fill: 'fit',
  pace: 'preserve',
  introStyle: 'hook-first',
  musicMood: 'none',
};

const STYLE_DNA_KEY = 'studiocut.styleDna';

const STYLE_DNA_FIELDS: { [K in keyof StyleDNA]: readonly string[] } = {
  captionPreset: ['hormozi', 'minimal', 'documentary', 'neon'],
  aspect: ['landscape', 'vertical', 'square'],
  fill: ['fit', 'cover'],
  pace: ['preserve', 'tight', 'documentary', 'shortform'],
  introStyle: ['hook-first', 'cold-open', 'title-card', 'none'],
  musicMood: ['none', 'upbeat', 'calm', 'dramatic', 'cinematic'],
};

export function getStyleDna(): StyleDNA {
  const raw = read(STYLE_DNA_KEY);
  if (!raw) return { ...DEFAULT_STYLE_DNA };
  try {
    const parsed = JSON.parse(raw) as Partial<StyleDNA>;
    const next = { ...DEFAULT_STYLE_DNA };
    (Object.keys(STYLE_DNA_FIELDS) as (keyof StyleDNA)[]).forEach(key => {
      const value = parsed[key];
      if (typeof value === 'string' && (STYLE_DNA_FIELDS[key] as readonly string[]).includes(value)) {
        (next as Record<string, string>)[key] = value;
      }
    });
    return next;
  } catch {
    return { ...DEFAULT_STYLE_DNA };
  }
}

export function setStyleDna(style: StyleDNA): void {
  write(STYLE_DNA_KEY, JSON.stringify(style));
}

// --- Director history (Phase 3) ------------------------------------------
// Per-project conversation history (instruction + proposal summary).
// Local only, max 10 entries, newest last.
export type DirectorHistoryEntry = {
  kind: 'edit' | 'titles';
  instruction: string;
  summary: string;
  at: number;
};

const DIRECTOR_HISTORY_PREFIX = 'studiocut.directorHistory.';
const DIRECTOR_HISTORY_MAX = 10;

export function getDirectorHistory(projectId: string): DirectorHistoryEntry[] {
  const raw = read(`${DIRECTOR_HISTORY_PREFIX}${projectId}`);
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as DirectorHistoryEntry[];
    return Array.isArray(parsed)
      ? parsed.filter(
          e =>
            (e?.kind === 'edit' || e?.kind === 'titles') &&
            typeof e?.instruction === 'string' &&
            typeof e?.summary === 'string' &&
            typeof e?.at === 'number',
        )
      : [];
  } catch {
    return [];
  }
}

export function addDirectorHistory(projectId: string, entry: DirectorHistoryEntry): DirectorHistoryEntry[] {
  const next = [...getDirectorHistory(projectId), entry].slice(-DIRECTOR_HISTORY_MAX);
  write(`${DIRECTOR_HISTORY_PREFIX}${projectId}`, JSON.stringify(next));
  return next;
}

// --- Recent projects (Phase 2a) ------------------------------------------
export type RecentProject = { path: string; name: string; openedAt: number };

const RECENT_KEY = 'studiocut.recentProjects';
const RECENT_MAX = 8;

export function getRecentProjects(): RecentProject[] {
  const raw = read(RECENT_KEY);
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as RecentProject[];
    return Array.isArray(parsed)
      ? parsed.filter(p => typeof p?.path === 'string' && typeof p?.name === 'string')
      : [];
  } catch {
    return [];
  }
}

export function addRecentProject(path: string, name: string): RecentProject[] {
  const next = [
    { path, name, openedAt: Date.now() },
    ...getRecentProjects().filter(p => p.path !== path),
  ].slice(0, RECENT_MAX);
  write(RECENT_KEY, JSON.stringify(next));
  return next;
}
