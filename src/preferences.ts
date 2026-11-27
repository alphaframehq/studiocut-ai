// Shared, persisted user preferences — single source of truth.
import type { Edl } from './edl';
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
    // Storage unavailable (private mode, etc.) — preferences do not persist.
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

// --- Export proxy choice ---------------------------------------------------
// Whether exports render from ready 540p proxy files when available.
// Defaults off: an export never silently uses low-resolution proxies.
// Persisted like the encoder preference; the Export dialog reads it
// when it opens and writes it when the checkbox changes.
const EXPORT_PROXY_KEY = 'studiocut.exportUseProxy';
export const DEFAULT_EXPORT_USE_PROXY = false;

export function getExportUseProxy(): boolean {
  return read(EXPORT_PROXY_KEY) === '1';
}

export function setExportUseProxy(useProxy: boolean): void {
  write(EXPORT_PROXY_KEY, useProxy ? '1' : '0');
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
  /** The validated EDL behind this proposal, when small enough to keep. */
  edl?: Edl;
};

const DIRECTOR_HISTORY_PREFIX = 'studiocut.directorHistory.';
const DIRECTOR_HISTORY_MAX = 10;

export function getDirectorHistory(projectId: string): DirectorHistoryEntry[] {
  const raw = read(`${DIRECTOR_HISTORY_PREFIX}${projectId}`);
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as DirectorHistoryEntry[];
    return Array.isArray(parsed)
      ? parsed
          .filter(
            e =>
              (e?.kind === 'edit' || e?.kind === 'titles') &&
              typeof e?.instruction === 'string' &&
              typeof e?.summary === 'string' &&
              typeof e?.at === 'number',
          )
          .map(e => {
            // Keep the stored EDL only when it looks like an EDL and is
            // small enough to keep in localStorage (≤ 32 KB of JSON).
            if (e.edl === undefined) return e;
            if (
              !e.edl ||
              typeof e.edl !== 'object' ||
              !Array.isArray((e.edl as Edl).operations)
            ) {
              const { edl: _dropped, ...rest } = e;
              return rest;
            }
            try {
              if (JSON.stringify(e.edl).length > 32 * 1024) {
                const { edl: _dropped, ...rest } = e;
                return rest;
              }
            } catch {
              const { edl: _dropped, ...rest } = e;
              return rest;
            }
            return e;
          })
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

// --- Director feedback + learning (Phase 4) ------------------------------
// What the user accepted / rejected / asked to redo, per operation type,
// caption preset and pace. Local only; feeds the learned-preferences
// summary sent with each proposal and the 3-accept streak rule that
// updates Style DNA. Nothing here is sent to any provider except the
// short text summary built by buildLearnedPreferences().
export type DirectorFeedbackOutcome = 'accepted' | 'rejected' | 'modified';
export type DirectorFeedback = {
  ops: Record<string, { accepted: number; rejected: number; modified: number }>;
  presets: Record<string, { accepted: number; rejected: number }>;
  pace: Record<string, { accepted: number; rejected: number }>;
  streaks: Record<string, number>;
  updatedAt: number;
};

const DIRECTOR_FEEDBACK_KEY = 'studiocut.directorFeedback';
const DIRECTOR_FEEDBACK_MAX_OUTCOMES = 500;

function emptyDirectorFeedback(): DirectorFeedback {
  return { ops: {}, presets: {}, pace: {}, streaks: {}, updatedAt: 0 };
}

function countOutcome(value: unknown): number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0
    ? Math.floor(value)
    : 0;
}

function validCounts(
  value: unknown,
  withModified: boolean,
): Record<string, { accepted: number; rejected: number; modified: number }> | Record<string, { accepted: number; rejected: number }> {
  const out: Record<string, { accepted: number; rejected: number; modified?: number }> = {};
  if (!value || typeof value !== 'object' || Array.isArray(value)) return out;
  for (const [key, entry] of Object.entries(value as Record<string, unknown>)) {
    if (!key || !entry || typeof entry !== 'object' || Array.isArray(entry)) continue;
    const counts = entry as Record<string, unknown>;
    out[key] = withModified
      ? { accepted: countOutcome(counts.accepted), rejected: countOutcome(counts.rejected), modified: countOutcome(counts.modified) }
      : { accepted: countOutcome(counts.accepted), rejected: countOutcome(counts.rejected) };
  }
  return out;
}

export function getDirectorFeedback(): DirectorFeedback {
  const raw = read(DIRECTOR_FEEDBACK_KEY);
  if (!raw) return emptyDirectorFeedback();
  try {
    const parsed = JSON.parse(raw) as Partial<DirectorFeedback>;
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return emptyDirectorFeedback();
    const streaks: Record<string, number> = {};
    if (parsed.streaks && typeof parsed.streaks === 'object' && !Array.isArray(parsed.streaks)) {
      for (const [key, value] of Object.entries(parsed.streaks)) {
        if (key) streaks[key] = countOutcome(value);
      }
    }
    return {
      ops: validCounts(parsed.ops, true) as DirectorFeedback['ops'],
      presets: validCounts(parsed.presets, false) as DirectorFeedback['presets'],
      pace: validCounts(parsed.pace, false) as DirectorFeedback['pace'],
      streaks,
      updatedAt: typeof parsed.updatedAt === 'number' && Number.isFinite(parsed.updatedAt) ? parsed.updatedAt : 0,
    };
  } catch {
    return emptyDirectorFeedback();
  }
}

function saveDirectorFeedback(feedback: DirectorFeedback): void {
  feedback.updatedAt = Date.now();
  write(DIRECTOR_FEEDBACK_KEY, JSON.stringify(feedback));
}

function totalOpOutcomes(feedback: DirectorFeedback): number {
  return Object.values(feedback.ops).reduce(
    (sum, c) => sum + c.accepted + c.rejected + c.modified,
    0,
  );
}

function halveCounts(feedback: DirectorFeedback): void {
  // Over the 500-outcome cap: halve every recorded count (rounding down)
  // and drop entries that reach zero, so old learning fades instead of
  // being wiped outright.
  const halve = <T extends { accepted: number; rejected: number; modified?: number }>(
    record: Record<string, T>,
  ): Record<string, T> => {
    const next: Record<string, T> = {};
    for (const [key, counts] of Object.entries(record)) {
      const halved = {
        ...counts,
        accepted: Math.floor(counts.accepted / 2),
        rejected: Math.floor(counts.rejected / 2),
        ...(counts.modified !== undefined ? { modified: Math.floor(counts.modified / 2) } : {}),
      } as T;
      const total = halved.accepted + halved.rejected + (halved.modified ?? 0);
      if (total > 0) next[key] = halved;
    }
    return next;
  };
  feedback.ops = halve(feedback.ops);
  feedback.presets = halve(feedback.presets) as DirectorFeedback['presets'];
  feedback.pace = halve(feedback.pace) as DirectorFeedback['pace'];
}

function enforceDirectorFeedbackCap(feedback: DirectorFeedback): void {
  if (totalOpOutcomes(feedback) > DIRECTOR_FEEDBACK_MAX_OUTCOMES) halveCounts(feedback);
}

export function recordDirectorFeedback(
  outcomes: { opType: string; outcome: DirectorFeedbackOutcome }[],
): void {
  if (!outcomes.length) return;
  const feedback = getDirectorFeedback();
  for (const { opType, outcome } of outcomes) {
    if (!opType || (outcome !== 'accepted' && outcome !== 'rejected' && outcome !== 'modified')) continue;
    const entry = feedback.ops[opType] ?? { accepted: 0, rejected: 0, modified: 0 };
    entry[outcome] += 1;
    feedback.ops[opType] = entry;
  }
  enforceDirectorFeedbackCap(feedback);
  saveDirectorFeedback(feedback);
}

export function recordPresetFeedback(preset: string, outcome: 'accepted' | 'rejected'): void {
  if (!preset) return;
  const feedback = getDirectorFeedback();
  const entry = feedback.presets[preset] ?? { accepted: 0, rejected: 0 };
  entry[outcome] += 1;
  feedback.presets[preset] = entry;
  const streakKey = `preset:${preset}`;
  feedback.streaks[streakKey] = outcome === 'accepted' ? (feedback.streaks[streakKey] ?? 0) + 1 : 0;
  enforceDirectorFeedbackCap(feedback);
  saveDirectorFeedback(feedback);
}

export function recordPaceFeedback(pace: string, outcome: 'accepted' | 'rejected'): void {
  if (!pace) return;
  const feedback = getDirectorFeedback();
  const entry = feedback.pace[pace] ?? { accepted: 0, rejected: 0 };
  entry[outcome] += 1;
  feedback.pace[pace] = entry;
  const streakKey = `pace:${pace}`;
  feedback.streaks[streakKey] = outcome === 'accepted' ? (feedback.streaks[streakKey] ?? 0) + 1 : 0;
  enforceDirectorFeedbackCap(feedback);
  saveDirectorFeedback(feedback);
}

const DIRECTOR_OP_LABELS: Record<string, string> = {
  lut: 'Grade (lut)',
  captions: 'Captions',
  captionStyle: 'Caption style',
  trim: 'Trim',
  split: 'Split',
  delete: 'Delete',
  ripple: 'Ripple',
  move: 'Move',
  jCut: 'J cut',
  lCut: 'L cut',
  zoom: 'Zoom',
  duck: 'Duck',
  insert: 'Insert',
  broll: 'B-roll',
  sfx: 'SFX',
  addTrack: 'Add track',
};

export function directorOpLabel(opType: string): string {
  return DIRECTOR_OP_LABELS[opType] ?? (opType.charAt(0).toUpperCase() + opType.slice(1));
}

export function buildLearnedPreferences(): string {
  const feedback = getDirectorFeedback();
  const lines: string[] = [];
  for (const [opType, counts] of Object.entries(feedback.ops)) {
    const total = counts.accepted + counts.rejected + counts.modified;
    if (total < 2) continue;
    const label = directorOpLabel(opType);
    let line = `${label}: accepted ${counts.accepted}/${total}.`;
    if (counts.rejected >= 2 && counts.rejected > counts.accepted) {
      const noun = label.replace(/\s*\(.*?\)\s*/, '').toLowerCase();
      line = `${label}: rejected ${counts.rejected}/${total} — suggest fewer ${noun} changes.`;
    }
    lines.push(line);
  }
  for (const [preset, counts] of Object.entries(feedback.presets)) {
    const total = counts.accepted + counts.rejected;
    if (total < 2) continue;
    lines.push(`Caption preset: ${preset} accepted ${counts.accepted}/${total}.`);
  }
  for (const [pace, counts] of Object.entries(feedback.pace)) {
    const total = counts.accepted + counts.rejected;
    if (total < 2) continue;
    lines.push(`Pace: ${pace} accepted ${counts.accepted}/${total}.`);
  }
  return lines.join(' ');
}

export function maybeUpdateStyleDnaFromFeedback(): { changed: boolean; note: string } {
  const feedback = getDirectorFeedback();
  const dna = getStyleDna();
  const next = { ...dna };
  const notes: string[] = [];
  let changed = false;
  let spent = false;
  for (const [key, streak] of Object.entries(feedback.streaks)) {
    if (streak < 3) continue;
    spent = true;
    if (key.startsWith('preset:')) {
      const preset = key.slice('preset:'.length);
      if ((STYLE_DNA_FIELDS.captionPreset as readonly string[]).includes(preset)) {
        next.captionPreset = preset as StyleDNA['captionPreset'];
        notes.push(`caption preset → ${preset.charAt(0).toUpperCase() + preset.slice(1)}`);
        changed = true;
      }
    } else if (key.startsWith('pace:')) {
      const pace = key.slice('pace:'.length);
      if ((STYLE_DNA_FIELDS.pace as readonly string[]).includes(pace)) {
        next.pace = pace as StyleDNA['pace'];
        notes.push(`pace → ${pace.charAt(0).toUpperCase() + pace.slice(1)}`);
        changed = true;
      }
    }
    // The streak is spent whether or not the value was usable.
    feedback.streaks[key] = 0;
  }
  if (!changed) {
    // Still persist the spent streaks (if any were reset on invalid values).
    if (spent) saveDirectorFeedback(feedback);
    return { changed: false, note: '' };
  }
  setStyleDna(next);
  saveDirectorFeedback(feedback);
  return { changed: true, note: `Style updated from your choices: ${notes.join('; ')}` };
}

export function resetDirectorFeedback(): void {
  try {
    localStorage.removeItem(DIRECTOR_FEEDBACK_KEY);
  } catch {
    // Storage unavailable — nothing to clear.
  }
  setStyleDna({ ...DEFAULT_STYLE_DNA });
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

// --- MCP servers (Phase 5) ------------------------------------------------
// Saved MCP server configurations (remote URL or local stdio command).
// No secrets live here — API keys stay in the OS keyring / session vault
// managed by the Rust MCP module (see mcp_set_credential). At most 20
// servers are kept.
export type McpServerConfig = {
  id: string;
  name: string;
  presetId: string | null;
  transport: 'http' | 'stdio';
  url: string;
  command: string;
};

const MCP_SERVERS_KEY = 'studiocut.mcpServers';
const MCP_SELECTED_SERVER_KEY = 'studiocut.mcpSelectedServer';
const MCP_SERVERS_MAX = 20;

function validMcpServer(value: unknown): McpServerConfig | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
  const server = value as Record<string, unknown>;
  if (
    typeof server.id !== 'string' ||
    !server.id ||
    server.id.length > 80 ||
    /\s/.test(server.id) ||
    typeof server.name !== 'string' ||
    !server.name.trim() ||
    server.name.length > 120 ||
    (server.presetId !== null && typeof server.presetId !== 'string') ||
    (server.transport !== 'http' && server.transport !== 'stdio') ||
    typeof server.url !== 'string' ||
    server.url.length > 2048 ||
    typeof server.command !== 'string' ||
    server.command.length > 4096
  ) {
    return null;
  }
  return {
    id: server.id,
    name: server.name,
    presetId: server.presetId as string | null,
    transport: server.transport as 'http' | 'stdio',
    url: server.url,
    command: server.command,
  };
}

export function getMcpServers(): McpServerConfig[] {
  const raw = read(MCP_SERVERS_KEY);
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as unknown[];
    if (!Array.isArray(parsed)) return [];
    const seen = new Set<string>();
    return parsed
      .map(validMcpServer)
      .filter((s): s is McpServerConfig => {
        if (!s || seen.has(s.id)) return false;
        seen.add(s.id);
        return true;
      })
      .slice(0, MCP_SERVERS_MAX);
  } catch {
    return [];
  }
}

function saveMcpServers(servers: McpServerConfig[]): McpServerConfig[] {
  const next = servers.slice(0, MCP_SERVERS_MAX);
  write(MCP_SERVERS_KEY, JSON.stringify(next));
  return next;
}

export function addMcpServer(server: McpServerConfig): McpServerConfig[] {
  const valid = validMcpServer(server);
  if (!valid) return getMcpServers();
  return saveMcpServers([
    valid,
    ...getMcpServers().filter(s => s.id !== valid.id),
  ]);
}

export function updateMcpServer(server: McpServerConfig): McpServerConfig[] {
  const valid = validMcpServer(server);
  if (!valid) return getMcpServers();
  return saveMcpServers(getMcpServers().map(s => (s.id === valid.id ? valid : s)));
}

export function removeMcpServer(id: string): McpServerConfig[] {
  return saveMcpServers(getMcpServers().filter(s => s.id !== id));
}

export function getMcpSelectedServer(): string | null {
  return read(MCP_SELECTED_SERVER_KEY);
}

export function setMcpSelectedServer(id: string | null): void {
  if (id) {
    write(MCP_SELECTED_SERVER_KEY, id);
  } else {
    try {
      localStorage.removeItem(MCP_SELECTED_SERVER_KEY);
    } catch {
      // Storage unavailable — nothing to clear.
    }
  }
}
