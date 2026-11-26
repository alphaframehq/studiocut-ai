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
