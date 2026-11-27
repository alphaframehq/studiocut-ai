import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { native } from './store';
import './director.css';

export type ApiConnectionKind = 'openai-compatible' | 'generic';
export type ApiConnectionAuthType = 'none' | 'api-key';
export type ApiConnectionKeyStorage = 'os' | 'session' | 'none';

// A saved API connection. The backend never returns a stored key to the
// frontend: the panel only ever sends a key when the user types one, and
// shows only the storage choice afterwards.
export type ApiConnection = {
  id: string;
  name: string;
  baseUrl: string;
  kind: ApiConnectionKind;
  authType: ApiConnectionAuthType;
  authHeader?: string;
  keyStorage: ApiConnectionKeyStorage;
};

export type ApiConnectionInput = {
  name: string;
  baseUrl: string;
  kind: ApiConnectionKind;
  authType: ApiConnectionAuthType;
  authHeader?: string;
  key?: string;
  keyStorage: ApiConnectionKeyStorage;
};

export type ApiConnectionTestResult = {
  ok: boolean;
  status?: number;
  latencyMs: number;
  modelCount?: number;
  error?: string;
};

/** Load the saved connections. The backend never returns a stored key. */
export async function fetchApiConnections(): Promise<ApiConnection[]> {
  return invoke<ApiConnection[]>('list_api_connections');
}

const EMPTY_FORM = {
  name: '',
  baseUrl: '',
  kind: 'openai-compatible' as ApiConnectionKind,
  authType: 'api-key' as ApiConnectionAuthType,
  authHeader: 'Authorization',
  key: '',
  keyStorage: 'os' as ApiConnectionKeyStorage,
};

function keyStorageLabel(storage: ApiConnectionKeyStorage): string {
  if (storage === 'os') return 'OS credential store';
  if (storage === 'session') return 'Session only';
  return 'Not stored';
}

function testLine(result: ApiConnectionTestResult): string {
  if (!result.ok) {
    return `Test failed${result.latencyMs ? ` after ${result.latencyMs} ms` : ''}: ${result.error ?? 'No error detail was returned.'}`;
  }
  const parts = [`Reachable in ${result.latencyMs} ms`];
  if (result.status !== undefined) parts.push(`HTTP ${result.status}`);
  if (result.modelCount !== undefined) parts.push(`${result.modelCount} model(s) listed`);
  return `${parts.join(' · ')}.`;
}

export function ApiConnectionsPanel() {
  const [open, setOpen] = useState(false);
  const [connections, setConnections] = useState<ApiConnection[]>([]);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState(EMPTY_FORM);
  const [testResults, setTestResults] = useState<Record<string, ApiConnectionTestResult>>({});
  const [testingId, setTestingId] = useState<string | null>(null);
  const [working, setWorking] = useState(false);
  const [status, setStatus] = useState('');
  const [error, setError] = useState('');

  async function refresh() {
    if (!native) return;
    try {
      setConnections(await fetchApiConnections());
    } catch (e) {
      setError(String(e));
    }
  }

  useEffect(() => {
    if (open) void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open ]);

  function startEdit(connection: ApiConnection) {
    setEditingId(connection.id);
    setForm({
      name: connection.name,
      baseUrl: connection.baseUrl,
      kind: connection.kind,
      authType: connection.authType,
      authHeader: connection.authHeader ?? 'Authorization',
      key: '',
      keyStorage: connection.keyStorage === 'none' ? 'os' : connection.keyStorage,
    });
    setError('');
    setStatus('');
  }

  function cancelEdit() {
    setEditingId(null);
    setForm(EMPTY_FORM);
    setError('');
  }

  async function saveConnection() {
    if (!form.name.trim()) {
      setError('Enter a connection name.');
      return;
    }
    if (!form.baseUrl.trim()) {
      setError('Enter a base URL.');
      return;
    }
    if (form.authType === 'api-key' && !editingId && !form.key.trim()) {
      setError('Enter an API key, or choose "No authentication".');
      return;
    }
    const input: ApiConnectionInput = {
      name: form.name.trim(),
      baseUrl: form.baseUrl.trim(),
      kind: form.kind,
      authType: form.authType,
      ...(form.authType === 'api-key' ? { authHeader: form.authHeader.trim() || 'Authorization' } : {}),
      // A key is sent only when the user types one. When editing, leaving
      // the field empty keeps the stored key unchanged.
      ...(form.authType === 'api-key' && form.key.trim() ? { key: form.key } : {}),
      keyStorage: form.authType === 'api-key' ? form.keyStorage : 'none',
    };
    setWorking(true);
    setError('');
    setStatus('');
    try {
      if (editingId) {
        await invoke('update_api_connection', { id: editingId, patch: input });
        setStatus(`Connection “${input.name}” updated.`);
      } else {
        await invoke('add_api_connection', { connection: input });
        setStatus(`Connection “${input.name}” added.`);
      }
      setForm(EMPTY_FORM);
      setEditingId(null);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  }

  async function deleteConnection(id: string, name: string) {
    setWorking(true);
    setError('');
    try {
      await invoke('delete_api_connection', { id });
      setConnections(prev => prev.filter(c => c.id !== id));
      setStatus(`Connection “${name}” removed.`);
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  }

  async function testConnection(id: string) {
    setTestingId(id);
    setError('');
    try {
      const result = await invoke<ApiConnectionTestResult>('test_api_connection', { id });
      setTestResults(prev => ({ ...prev, [id]: result }));
    } catch (e) {
      setError(String(e));
    } finally {
      setTestingId(null);
    }
  }

  return <section className="director-section" aria-label="API Connections">
    <div className="director-bar"><div><span className="eyebrow">SAVED API CONNECTIONS</span><h2>API Connections</h2><p>Save a base URL and key once, test it, then pick it in the AI Director or a custom MCP server.</p></div><button className="button secondary" aria-expanded={open} onClick={() => setOpen(v => !v)}>{open ? 'Close API Connections' : 'Open API Connections'}</button></div>
    {open && <div className="director-layout"><div className="director-setup">
      {!native && <p className="director-notice">API connections require the desktop app. Connections are stored by the backend; the browser preview cannot list or test them.</p>}
      <p className="director-notice">Selecting a connection in a feature is explicit — nothing is patched automatically. Saving a connection here only stores it. A feature uses it only when you choose it yourself in the AI Director or in a custom MCP server form; no feature ever switches to a connection on its own.</p>
      {connections.length > 0 && <div><h3>Saved connections</h3>{connections.map(c => <div key={c.id} className="director-notice">
        <strong>{c.name}</strong><p>{c.baseUrl}</p>
        <p>{c.kind === 'openai-compatible' ? 'OpenAI-compatible' : 'Generic'} · {c.authType === 'api-key' ? `API key (${keyStorageLabel(c.keyStorage)})` : 'No authentication'}</p>
        <div className="director-actions">
          <button className="button secondary" disabled={!native || working || testingId === c.id} onClick={() => void testConnection(c.id)}>{testingId === c.id ? 'Testing…' : 'Test'}</button>
          <button className="button secondary" disabled={!native || working} onClick={() => startEdit(c)}>Edit</button>
          <button className="button secondary" disabled={!native || working} onClick={() => void deleteConnection(c.id, c.name)}>Delete</button>
        </div>
        {testResults[c.id] && <p role="status">{testLine(testResults[c.id])}</p>}
      </div>)}</div>}
      <h3>{editingId ? 'Edit connection' : 'Add a connection'}</h3>
      <fieldset disabled={!native || working}>
        <label>Connection name<input value={form.name} onChange={e => setForm({ ...form, name: e.target.value })} maxLength={120} placeholder="My hosted model API" /></label>
        <label>Base URL<input value={form.baseUrl} onChange={e => setForm({ ...form, baseUrl: e.target.value })} spellCheck={false} maxLength={2048} placeholder="https://your-host/v1" /></label>
        <label>API kind<select value={form.kind} onChange={e => setForm({ ...form, kind: e.target.value as ApiConnectionKind })}><option value="openai-compatible">OpenAI-compatible</option><option value="generic">Generic</option></select></label>
        <label>Authentication<select value={form.authType} onChange={e => setForm({ ...form, authType: e.target.value as ApiConnectionAuthType })}><option value="api-key">API key</option><option value="none">No authentication</option></select></label>
        {form.authType === 'api-key' && <>
          <label>Authorization header<input value={form.authHeader} onChange={e => setForm({ ...form, authHeader: e.target.value })} spellCheck={false} maxLength={120} placeholder="Authorization" /></label>
          <label>API key<input type="password" autoComplete="off" spellCheck={false} value={form.key} onChange={e => setForm({ ...form, key: e.target.value })} maxLength={8192} placeholder={editingId ? 'Leave empty to keep the stored key' : 'Stored only by Rust, never in project data'} /></label>
          <label>Key storage<select value={form.keyStorage} onChange={e => setForm({ ...form, keyStorage: e.target.value as ApiConnectionKeyStorage })}><option value="os">OS credential store (recommended)</option><option value="session">Session only — cleared on app exit</option></select></label>
        </>}
        <div className="director-actions">
          <button className="button secondary" disabled={!form.name.trim() || !form.baseUrl.trim()} onClick={() => void saveConnection()}>{editingId ? 'Save changes' : 'Add connection'}</button>
          {editingId && <button className="button secondary" onClick={cancelEdit}>Cancel</button>}
        </div>
        <p>Keys are never shown again after saving. When editing, leave the key field empty to keep the stored key; enter a new key only to replace it.</p>
      </fieldset>
      {status && <p role="status">{status}</p>}
      {error && <p role="alert" className="error">{error}</p>}
    </div><aside className="director-review" aria-label="API Connections notes"><h3>Notes</h3>
      <p className="director-badge">EXPLICIT SELECTION ONLY</p>
      <p>OpenAI-compatible APIs work immediately once picked in the AI Director or a custom MCP server. Any other API needs a connector and is out of scope for this feature.</p>
      <p>Keys are stored by the backend in the OS credential store or in session memory only. They are never written to project files or preferences, and they are never returned to this panel.</p>
      <p>Test checks that the base URL responds and lists how many models it reports, if any. A failed test shows the error returned by the backend.</p>
    </aside></div>}
  </section>;
}
