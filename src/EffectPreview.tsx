import { useEffect, useRef, useState } from 'react';
import type { Lut } from './timeline';
import { applyLut } from './effects';

export function EffectPreview({ source, image, preset, frame }: { source: React.RefObject<HTMLVideoElement | HTMLImageElement | null>; image: boolean; preset: Lut; frame: number }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [error, setError] = useState('');
  useEffect(() => {
    const element = source.current, target = canvas.current;
    if (!element || !target) return;
    const draw = () => {
      const width = image ? (element as HTMLImageElement).naturalWidth : (element as HTMLVideoElement).videoWidth;
      const height = image ? (element as HTMLImageElement).naturalHeight : (element as HTMLVideoElement).videoHeight;
      if (!width || !height) return;
      target.width = Math.min(width, 640); target.height = Math.round(target.width * height / width);
      const context = target.getContext('2d', { willReadFrequently: true });
      if (!context) { setError('LUT preview is unavailable in this WebView.'); return; }
      try { context.drawImage(element, 0, 0, target.width, target.height); const pixels = context.getImageData(0, 0, target.width, target.height); applyLut(pixels.data, preset); context.putImageData(pixels, 0, 0); setError(''); }
      catch { setError('LUT preview cannot read this media source.'); }
    };
    draw(); element.addEventListener(image ? 'load' : 'seeked', draw); element.addEventListener('loadeddata', draw);
    return () => { element.removeEventListener(image ? 'load' : 'seeked', draw); element.removeEventListener('loadeddata', draw); };
  }, [source, image, preset, frame]);
  return <><canvas ref={canvas} className="lut-preview" aria-label={`${preset} LUT preview`} />{error && <p role="alert">{error}</p>}</>;
}
