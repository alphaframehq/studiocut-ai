import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useProject } from './store';
import { audible } from './timeline';
import { getEncoderPreference } from './preferences';

type ProxyStatus = {
  state: 'missing' | 'ready' | 'generating' | 'failed';
  proxyPath: string | null; width: number | null; height: number | null;
  sizeBytes: number | null; encoder: string | null; error: string | null;
};

/** Event dispatched when the proxy/original preference changes for an
 * asset. The preview player does not listen to it yet — the coordinator
 * wires `Preview` to `isProxyPreferred` (see the Phase 5 handover). */
export const PROXY_TOGGLED_EVENT = 'studiocut-proxy-toggled';

function preferenceKey(assetId: string) {
  return `studiocut-proxy-preferred:${assetId}`;
}

/** Whether the user prefers the proxy for this asset in preview.
 * Read by the preview player once it is connected to this preference. */
export function isProxyPreferred(assetId: string): boolean {
  return localStorage.getItem(preferenceKey(assetId)) === '1';
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function ProxyToggle() {
  const { project, frame } = useProject();
  const [status, setStatus] = useState<ProxyStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [useProxy, setUseProxy] = useState(false);
  const active = project?.tracks.filter(t => t.kind === 'video' && audible(t, project.tracks)).flatMap(t => t.clips.filter(c => c.start <= frame && c.start + c.duration > frame))[0];
  const asset = project?.assets.find(a => a.id === active?.assetId);
  const assetId = asset?.id;

  const refresh = useCallback(async () => {
    if (!assetId) { setStatus(null); return; }
    try {
      setStatus(await invoke<ProxyStatus>('proxy_status', { id: assetId }));
    } catch (e) { setError(String(e)); }
  }, [assetId]);

  useEffect(() => {
    setError('');
    if (assetId) setUseProxy(isProxyPreferred(assetId));
    void refresh();
  }, [assetId, refresh]);

  const generate = async () => {
    if (!assetId) return;
    setBusy(true);
    setError('');
    try {
      setStatus(await invoke<ProxyStatus>('generate_proxy', { id: assetId, preference: getEncoderPreference() }));
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const remove = async () => {
    if (!assetId) return;
    setBusy(true);
    setError('');
    try {
      setStatus(await invoke<ProxyStatus>('delete_proxy', { id: assetId }));
      setUseProxy(false);
      localStorage.removeItem(preferenceKey(assetId));
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  };
  const togglePreference = () => {
    if (!assetId) return;
    const next = !useProxy;
    setUseProxy(next);
    localStorage.setItem(preferenceKey(assetId), next ? '1' : '0');
    window.dispatchEvent(new CustomEvent(PROXY_TOGGLED_EVENT, { detail: { assetId, useProxy: next } }));
  };

  if (!project) return null;
  return <section className="proxy-toggle" aria-label="Proxy workflow">
    <header><strong>Proxy</strong><span>540p PREVIEW PROXY</span></header>
    <p className="finish-note">
      Proxies are low-resolution copies for smoother preview. Originals are never modified. Export uses the original media unless the proxy option is enabled in the export settings.
    </p>
    {!asset
      ? <p className="finish-note">Place the playhead over a video clip to manage its proxy.</p>
      : <>
        <p className="finish-note">{asset.name} — {status?.state === 'ready'
          ? `proxy ready · ${status.width} × ${status.height}${status.sizeBytes ? ` · ${formatBytes(status.sizeBytes)}` : ''}${status.encoder ? ` · encoder ${status.encoder}` : ''}`
          : status?.state === 'generating' ? 'proxy generating…'
          : status?.state === 'failed' ? `proxy generation failed${status.error ? `: ${status.error}` : ''}`
          : 'no proxy yet'}</p>
        {error && <p className="error" role="alert">{error}</p>}
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
          <button className="button secondary" onClick={() => void generate()} disabled={busy || status?.state === 'generating' || status?.state === 'ready'}>
            {busy ? 'Generating…' : status?.state === 'ready' ? 'Proxy ready' : 'Generate 540p proxy'}
          </button>
          {status?.state === 'ready' && <button className="button secondary" onClick={() => void remove()} disabled={busy}>Delete proxy</button>}
          <button className="button secondary" onClick={() => void refresh()} disabled={busy}>Refresh status</button>
        </div>
        {status?.state === 'ready' && <label style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 11, marginTop: 10 }}>
          <input type="checkbox" checked={useProxy} onChange={togglePreference} />
          Use proxy in preview for this clip
        </label>}
        {useProxy && status?.state === 'ready' && <p className="finish-note">Preference saved. The preview player switches to the proxy once the preview wiring is connected.</p>}
      </>}
  </section>;
}
