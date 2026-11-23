export type TransformKey = { frame: number; scale: number; x: number; y: number; opacity: number };
export type Finishing = {
  speed: number; brightness: number; contrast: number; saturation: number; balance: [number, number, number];
  cube: string | null; transforms: TransformKey[]; denoise: boolean; level: boolean;
  role: 'normal' | 'voice' | 'music'; autoDuck: boolean; gain: number;
};
export const defaultFinishing = (): Finishing => ({ speed: 1, brightness: 0, contrast: 1, saturation: 1, balance: [0, 0, 0], cube: null, transforms: [], denoise: false, level: false, role: 'normal', autoDuck: false, gain: 1 });
export function validateCube(text: string): number {
  if (typeof text !== 'string' || new TextEncoder().encode(text).length > 262144 || text.includes('\0')) throw new Error('LUT exceeds 256 KB or contains NUL.');
  let size = 0, rows = 0; const domains = new Set<string>();
  const numeric = (s: string) => /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(s) && Number.isFinite(Number(s));
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.split('#')[0].trim(); if (!line) continue;
    const fields = line.split(/\s+/);
    if (fields[0] === 'TITLE' && !rows && /^TITLE\s+".*"$/.test(line)) continue;
    if (fields[0] === 'LUT_3D_SIZE' && fields.length === 2 && !size && !rows && /^\d+$/.test(fields[1])) {
      size = Number(fields[1]); if (size < 2 || size > 33) throw new Error('LUT size must be 2–33.');
    } else if (['DOMAIN_MIN', 'DOMAIN_MAX'].includes(fields[0]) && fields.length === 4 && !rows) {
      if (domains.has(fields[0]) || fields.slice(1).some(n => !numeric(n) || Number(n) !== (fields[0] === 'DOMAIN_MIN' ? 0 : 1))) throw new Error('Only RGB domain 0..1 LUTs are supported.');
      domains.add(fields[0]);
    } else if (fields.length === 3 && size) {
      if (fields.some(n => !numeric(n) || Number(n) < 0 || Number(n) > 1)) throw new Error('LUT values must be finite RGB 0..1.'); rows++;
    } else throw new Error('Unsupported or malformed .cube directive.');
  }
  if (!size || rows !== size ** 3) throw new Error('LUT row count does not match size cubed.');
  return size;
}
export function validateFinishing(value: unknown, kind: string, max: number): asserts value is Finishing {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid finishing settings.');
  const f = value as Finishing;
  const keys = Object.keys(defaultFinishing());
  if (Object.keys(f).length !== keys.length || keys.some(k => !(k in f))) throw new Error('Invalid finishing fields.');
  const valid = (n: number, min: number, max: number) => typeof n === 'number' && Number.isFinite(n) && n >= min && n <= max;
  if (!valid(f.speed, .25, 4) || !valid(f.brightness, -1, 1) || !valid(f.contrast, 0, 2) || !valid(f.saturation, 0, 3) || !valid(f.gain, 0, 2)
    || !Array.isArray(f.balance) || f.balance.length !== 3 || f.balance.some(n => !valid(n, -1, 1)) || !['normal', 'voice', 'music'].includes(f.role)
    || [f.denoise, f.level, f.autoDuck].some(n => typeof n !== 'boolean') || (f.autoDuck && f.role !== 'music')
    || !Array.isArray(f.transforms) || f.transforms.length > 64) throw new Error('Invalid finishing settings.');
  if ((kind !== 'video' && (f.brightness !== 0 || f.contrast !== 1 || f.saturation !== 1 || f.balance.some(n => n !== 0) || f.cube !== null || f.transforms.length))
    || (kind !== 'audio' && (f.denoise || f.level || f.autoDuck || f.role !== 'normal' || f.gain !== 1)) || (kind === 'caption' && f.speed !== 1)) throw new Error('Finishing settings incompatible with track.');
  f.transforms.forEach((k, i) => {
    if (!k || Object.keys(k).sort().join(',') !== 'frame,opacity,scale,x,y' || !Number.isSafeInteger(k.frame) || k.frame < 0 || k.frame > max || (i > 0 && k.frame <= f.transforms[i - 1].frame)
      || !valid(k.scale, .1, 4) || !valid(k.x, -1, 1) || !valid(k.y, -1, 1) || !valid(k.opacity, 0, 1)) throw new Error('Invalid transform keyframes.');
  });
  if (f.cube !== null) validateCube(f.cube);
}
