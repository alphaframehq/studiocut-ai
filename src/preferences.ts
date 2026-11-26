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
