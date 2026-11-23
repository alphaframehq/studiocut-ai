import type { GainKey, Lut, ZoomKey } from './timeline';

export function gainAt(keys: GainKey[], frame: number): number {
  if (!keys.length) return 1;
  const right = keys.findIndex(k => k.frame > frame);
  if (right === 0) return keys[0].gain;
  if (right < 0) return keys[keys.length - 1].gain;
  const a = keys[right - 1], b = keys[right];
  return a.gain + (b.gain - a.gain) * (frame - a.frame) / (b.frame - a.frame);
}
export function zoomAt(keys: ZoomKey[], frame: number): ZoomKey {
  if (!keys.length) return { frame, scale: 1, x: 0.5, y: 0.5 };
  const right = keys.findIndex(k => k.frame > frame);
  if (right === 0) return keys[0];
  if (right < 0) return keys[keys.length - 1];
  const a = keys[right - 1], b = keys[right], ratio = (frame - a.frame) / (b.frame - a.frame);
  return { frame, scale: a.scale + (b.scale - a.scale) * ratio, x: a.x + (b.x - a.x) * ratio, y: a.y + (b.y - a.y) * ratio };
}
const size = 17;
const tables = new Map<Lut, Float32Array>();
export function lutTable(preset: Lut): Float32Array {
  const cached = tables.get(preset); if (cached) return cached;
  const table = new Float32Array(size ** 3 * 3);
  for (let b = 0; b < size; b++) for (let g = 0; g < size; g++) for (let r = 0; r < size; r++) {
    const i = ((b * size + g) * size + r) * 3, red = r / 16, green = g / 16, blue = b / 16;
    const values = preset === 'warm' ? [red * 1.04 + green * 0.015, green, blue * 0.92] : preset === 'cool' ? [red * 0.94, green, blue * 1.04 + green * 0.01] : [red, green, blue];
    values.forEach((v, channel) => { table[i + channel] = Math.min(1, Math.max(0, v)); });
  }
  tables.set(preset, table); return table;
}
export function applyLut(pixels: Uint8ClampedArray, preset: Lut) {
  const table = lutTable(preset);
  for (let i = 0; i < pixels.length; i += 4) {
    const r = pixels[i] / 255 * 16, g = pixels[i + 1] / 255 * 16, b = pixels[i + 2] / 255 * 16;
    const r0 = Math.floor(r), g0 = Math.floor(g), b0 = Math.floor(b), dr = r - r0, dg = g - g0, db = b - b0;
    for (let c = 0; c < 3; c++) {
      let value = 0;
      for (let z = 0; z < 2; z++) for (let y = 0; y < 2; y++) for (let x = 0; x < 2; x++) {
        value += table[((Math.min(16, b0 + z) * size + Math.min(16, g0 + y)) * size + Math.min(16, r0 + x)) * 3 + c] * (x ? dr : 1 - dr) * (y ? dg : 1 - dg) * (z ? db : 1 - db);
      }
      pixels[i + c] = value * 255;
    }
  }
}
