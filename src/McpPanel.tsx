import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { native, useProject } from './store';
import {
  addMcpServer,
  getMcpSelectedServer,
  getMcpServers,
  removeMcpServer,
  setMcpSelectedServer,
  type McpServerConfig,
} from './preferences';

type McpPreset = {
  id: string;
  name: string;
  description: string;
  kind: string;
  url?: string | null;
  command?: string | null;
  auth: string;
  available: boolean;
  tools: string[];
  notes: string[];
};

type McpTool = { name: string; description?: string | null; inputSchema: unknown };

type FormField = {
  name: string;
  fieldType: string;
  title?: string | null;
  description?: string | null;
  required: boolean;
  default?: unknown;
  enumValues: unknown[];
  minimum?: number | null;
  maximum?: number | null;
  minLength?: number | null;
  maxLength?: number | null;
  itemsType?: string | null;
};

type OAuthDiscovery = {
  resource: string;
  authorizationServers: string[];
  authorizationEndpoint: string;
  tokenEndpoint: string;
  registrationEndpoint?: string | null;
  scopesSupported: string[];
  codeChallengeMethodsSupported: string[];
  clientIdMetadataDocumentSupported: boolean;
  grantTypesSupported: string[];
  responseTypesSupported: string[];
};

type OAuthClientRegistration = {
  clientId: string;
  clientSecret?: string | null;
  clientIdIssuedAt?: number | null;
  clientSecretExpiresAt?: number | null;
};

type OAuthTokenSummary = {
  tokenType: string;
  expiresIn?: number | null;
  hasRefreshToken: boolean;
  scope?: string | null;
};

type PkcePair = { verifier: string; challenge: string };

type McpTestResult = {
  serverId: string;
  serverName: string;
  protocolVersion?: string | null;
  serverInfo?: unknown;
  tools: McpTool[];
};

type MediaCandidate = { url: string; kind: string; label?: string | null };

type ToolResult = {
  content: unknown[];
  isError: boolean;
  structuredContent?: unknown;
  text: string;
  mediaUrls: MediaCandidate[];
};

type HiggsfieldModel = {
  id: string;
  name: string;
  kind: string;
  description: string;
  note?: string | null;
};

type HiggsfieldJob = {
  requestId: string;
  statusUrl?: string | null;
  cancelUrl?: string | null;
};

type HiggsfieldStatus = {
  requestId: string;
  status: string;
  state: 'pending' | 'succeeded' | 'failed';
  mediaUrls: MediaCandidate[];
  detail: unknown;
};

function parseJsonObject(text: string): Record<string, unknown> {
  const parsed: unknown = JSON.parse(text);
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error('Parameters must be a JSON object.');
  }
  return parsed as Record<string, unknown>;
}

function tryParseJsonObject(text: string): Record<string, unknown> {
  try {
    return parseJsonObject(text);
  } catch {
    return {};
  }
}

/**
 * Convert a tool inputSchema (JSON Schema) into form fields. This
 * mirrors `form_fields_from_schema` in src-tauri/src/mcp.rs: fields
 * come from the server's live tools/list response, never from
 * parameter names written in this codebase. When the schema has no
 * properties, the caller falls back to the free-form JSON editor.
 */
function fieldsFromSchema(schema: unknown): FormField[] {
  if (!schema || typeof schema !== 'object' || Array.isArray(schema)) return [];
  const record = schema as Record<string, unknown>;
  const properties = record.properties;
  if (!properties || typeof properties !== 'object' || Array.isArray(properties)) return [];
  const required = new Set(
    Array.isArray(record.required) ? record.required.filter((r): r is string => typeof r === 'string') : [],
  );
  const fieldTypeOf = (property: Record<string, unknown>): string => {
    const type = property.type;
    if (['string', 'number', 'integer', 'boolean', 'array', 'object'].includes(type as string)) return type as string;
    if (Array.isArray(property.enum) && property.enum.length > 0) {
      const first = property.enum[0];
      if (typeof first === 'string') return 'string';
      if (typeof first === 'boolean') return 'boolean';
      if (typeof first === 'number') return 'number';
    }
    return 'string';
  };
  return Object.entries(properties as Record<string, unknown>)
    .filter(([name]) => name.length > 0 && name.length <= 128)
    .map(([name, raw]) => {
      const property = (raw && typeof raw === 'object' && !Array.isArray(raw) ? raw : {}) as Record<string, unknown>;
      const items = property.items && typeof property.items === 'object' && !Array.isArray(property.items)
        ? (property.items as Record<string, unknown>)
        : null;
      return {
        name,
        fieldType: fieldTypeOf(property),
        title: typeof property.title === 'string' ? property.title : null,
        description: typeof property.description === 'string' ? property.description : null,
        required: required.has(name),
        default: property.default,
        enumValues: Array.isArray(property.enum) ? property.enum : [],
        minimum: typeof property.minimum === 'number' ? property.minimum : null,
        maximum: typeof property.maximum === 'number' ? property.maximum : null,
        minLength: typeof property.minLength === 'number' ? property.minLength : null,
        maxLength: typeof property.maxLength === 'number' ? property.maxLength : null,
        itemsType: items && typeof items.type === 'string' ? items.type : null,
      };
    });
}

function extensionOf(url: string): string {
  const path = url.split(/[?#]/)[0] ?? url;
  const extension = path.split('.').pop()?.toLowerCase() ?? '';
  return ['mp4', 'mov', 'webm', 'm4v', 'png', 'jpg', 'jpeg', 'webp', 'gif'].includes(extension)
    ? extension
    : 'mp4';
}

function toServerConfig(server: McpServerConfig): Record<string, unknown> {
  return {
    id: server.id,
    name: server.name,
    transport: server.transport,
    url: server.url || null,
    command: server.command || null,
    presetId: server.presetId,
  };
}

const POLL_INTERVAL_MS = 5000;
const POLL_MAX_ATTEMPTS = 120; // 10 minutes at 5-second intervals.

export function McpPanel() {
  const { project, busy, importMedia } = useProject();
  const [open, setOpen] = useState(false);
  const [presets, setPresets] = useState<McpPreset[]>([]);
  const [presetsError, setPresetsError] = useState('');
  const [servers, setServers] = useState<McpServerConfig[]>(() => getMcpServers());
  const [selectedId, setSelectedId] = useState<string | null>(() => getMcpSelectedServer());
  const [presetId, setPresetId] = useState('hyperframes');
  const [name, setName] = useState('HyperFrames');
  const [transport, setTransport] = useState<'http' | 'stdio'>('http');
  const [url, setUrl] = useState('https://hyperframes.net/api/mcp');
  const [command, setCommand] = useState('');
  const [storage, setStorage] = useState<'os' | 'session'>('os');
  const keyRef = useRef<HTMLInputElement>(null);
  const [testResult, setTestResult] = useState<McpTestResult | null>(null);
  const [tools, setTools] = useState<McpTool[]>([]);
  const [selectedTool, setSelectedTool] = useState('');
  const [paramsText, setParamsText] = useState('{}');
  const [toolResult, setToolResult] = useState<ToolResult | null>(null);
  const [statusTool, setStatusTool] = useState('get_generation');
  const [statusArgsText, setStatusArgsText] = useState('{}');
  const [polling, setPolling] = useState(false);
  const [pollNote, setPollNote] = useState('');
  const [hfModels, setHfModels] = useState<HiggsfieldModel[]>([]);
  const [hfModelsNote, setHfModelsNote] = useState('');
  const [hfEndpoint, setHfEndpoint] = useState('bytedance/seedance-2.5/image-to-video');
  const [hfParamsText, setHfParamsText] = useState('{\n  "prompt": "",\n  "image_url": ""\n}');
  const [hfJob, setHfJob] = useState<HiggsfieldJob | null>(null);
  const [hfStatus, setHfStatus] = useState<HiggsfieldStatus | null>(null);
  const [hfPolling, setHfPolling] = useState(false);
  const [downloading, setDownloading] = useState('');
  const [oauthDiscovery, setOauthDiscovery] = useState<OAuthDiscovery | null>(null);
  const [oauthClient, setOauthClient] = useState<OAuthClientRegistration | null>(null);
  const [oauthRedirectUri, setOauthRedirectUri] = useState('http://127.0.0.1:49152/callback');
  const [oauthUrl, setOauthUrl] = useState('');
  const [oauthCode, setOauthCode] = useState('');
  const [oauthToken, setOauthToken] = useState<OAuthTokenSummary | null>(null);
  const oauthPkceRef = useRef<PkcePair | null>(null);
  const [working, setWorking] = useState(false);
  const [status, setStatus] = useState('');
  const [error, setError] = useState('');
  const statusPollRef = useRef<{ active: boolean; timer: number | null }>({ active: false, timer: null });
  const hfPollRef = useRef<{ active: boolean; timer: number | null }>({ active: false, timer: null });

  useEffect(() => {
    return () => {
      statusPollRef.current.active = false;
      hfPollRef.current.active = false;
      if (statusPollRef.current.timer !== null) window.clearTimeout(statusPollRef.current.timer);
      if (hfPollRef.current.timer !== null) window.clearTimeout(hfPollRef.current.timer);
    };
  }, []);

  useEffect(() => {
    if (!native) return;
    void invoke<McpPreset[]>('mcp_list_presets')
      .then(setPresets)
      .catch(e => setPresetsError(String(e)));
    void invoke<{ models: HiggsfieldModel[]; note: string | null }>('mcp_higgsfield_models')
      .then(data => {
        setHfModels(data.models);
        setHfModelsNote(data.note ?? '');
      })
      .catch(() => setHfModels([]));
  }, []);

  const selectedPreset = presets.find(p => p.id === presetId) ?? null;
  const selectedServer = servers.find(s => s.id === selectedId) ?? null;
  const selectedToolDef = tools.find(t => t.name === selectedTool) ?? null;
  const formFields = selectedToolDef ? fieldsFromSchema(selectedToolDef.inputSchema) : [];

  function setParam(name: string, value: unknown): void {
    const next = tryParseJsonObject(paramsText);
    if (value === undefined) delete next[name];
    else next[name] = value;
    setParamsText(JSON.stringify(next, null, 2));
  }

  function paramValue(name: string): unknown {
    return tryParseJsonObject(paramsText)[name];
  }

  function selectTool(name: string, list: McpTool[] = tools): void {
    setSelectedTool(name);
    const tool = list.find(t => t.name === name);
    const seeded: Record<string, unknown> = {};
    if (tool) {
      for (const field of fieldsFromSchema(tool.inputSchema)) {
        if (field.default !== undefined) seeded[field.name] = field.default;
      }
    }
    setParamsText(JSON.stringify(seeded, null, 2));
    setToolResult(null);
  }

  function applyPreset(id: string) {
    setPresetId(id);
    const preset = presets.find(p => p.id === id);
    if (!preset) return;
    if (preset.kind === 'mcpHttp' && preset.url) {
      setTransport('http');
      setUrl(preset.url);
      setCommand('');
      setName(preset.name);
    } else if (preset.kind === 'mcpStdio' || preset.kind === 'custom') {
      setTransport('stdio');
      setUrl('');
      setCommand(preset.command ?? '');
      setName(preset.kind === 'custom' ? '' : preset.name);
    }
  }

  async function task<T>(fn: () => Promise<T>): Promise<T | null> {
    setWorking(true);
    setError('');
    try {
      return await fn();
    } catch (e) {
      setError(String(e));
      return null;
    } finally {
      setWorking(false);
    }
  }

  function takePerCallKey(): string | null {
    const value = keyRef.current?.value ?? '';
    if (keyRef.current) keyRef.current.value = '';
    return value.trim() ? value : null;
  }

  function addServer() {
    if (!name.trim()) {
      setError('Enter a server name.');
      return;
    }
    if (transport === 'http' && !url.trim()) {
      setError('Enter a server URL.');
      return;
    }
    if (transport === 'stdio' && !command.trim()) {
      setError('Enter a server command.');
      return;
    }
    const server: McpServerConfig = {
      id: `mcp-${crypto.randomUUID()}`,
      name: name.trim(),
      presetId: presetId === 'custom' ? null : presetId,
      transport,
      url: url.trim(),
      command: command.trim(),
    };
    const next = addMcpServer(server);
    setServers(next);
    setSelectedId(server.id);
    setMcpSelectedServer(server.id);
    setTestResult(null);
    setTools([]);
    setToolResult(null);
    setStatus(`Server “${server.name}” added. Save a credential, then test the connection.`);
  }

  function selectServer(id: string) {
    setSelectedId(id);
    setMcpSelectedServer(id);
    setTestResult(null);
    setTools([]);
    setSelectedTool('');
    setParamsText('{}');
    setToolResult(null);
    setPollNote('');
  }

  function removeServer(id: string) {
    const next = removeMcpServer(id);
    setServers(next);
    if (selectedId === id) {
      setSelectedId(null);
      setMcpSelectedServer(null);
      setTestResult(null);
      setTools([]);
      setToolResult(null);
    }
    setStatus('Server removed. Its saved credential is not deleted automatically — use Forget credential first if one was saved.');
  }

  async function saveCredential() {
    if (!selectedServer) return;
    const secret = keyRef.current?.value ?? '';
    if (keyRef.current) keyRef.current.value = '';
    if (!secret) {
      setError('Enter a credential.');
      return;
    }
    await task(async () => {
      await invoke('mcp_set_credential', { serverId: selectedServer.id, key: secret, storage });
      setStatus(storage === 'os' ? 'Credential saved in the OS credential store.' : 'Credential stored in Rust memory for this session only.');
    });
  }

  async function forgetCredential() {
    if (!selectedServer) return;
    await task(async () => {
      await invoke('mcp_set_credential', { serverId: selectedServer.id, key: null, storage: null });
      setStatus('Saved credential removed for this server.');
    });
  }

  async function saveHiggsfieldCredential() {
    const secret = keyRef.current?.value ?? '';
    if (keyRef.current) keyRef.current.value = '';
    if (!secret) {
      setError('Enter the Higgsfield KEY_ID:KEY_SECRET pair.');
      return;
    }
    await task(async () => {
      await invoke('mcp_set_credential', { serverId: 'higgsfield-rest', key: secret, storage });
      setStatus(storage === 'os' ? 'Higgsfield credential saved in the OS credential store.' : 'Higgsfield credential stored for this session only.');
    });
  }

  async function testConnection() {
    if (!selectedServer) return;
    const apiKey = takePerCallKey();
    const result = await task(() =>
      invoke<McpTestResult>('mcp_test_connection', {
        server: toServerConfig(selectedServer),
        apiKey,
      }),
    );
    if (result) {
      setTestResult(result);
      setTools(result.tools);
      if (result.tools.length > 0) selectTool(result.tools[0].name, result.tools);
      if (result.tools.some(t => t.name === 'get_generation')) setStatusTool('get_generation');
      setStatus(`Connected to ${result.serverName}. ${result.tools.length} tool(s) listed.`);
    }
  }

  async function listTools() {
    if (!selectedServer) return;
    const result = await task(() =>
      invoke<McpTool[]>('mcp_list_tools', {
        server: toServerConfig(selectedServer),
        apiKey: null,
      }),
    );
    if (result) {
      setTools(result);
      if (result.length > 0) selectTool(result[0].name, result);
      if (result.some(t => t.name === 'get_generation')) setStatusTool('get_generation');
      setStatus(`${result.length} tool(s) listed.`);
    }
  }

  async function runTool() {
    if (!selectedServer || !selectedTool) return;
    let args: Record<string, unknown>;
    try {
      args = parseJsonObject(paramsText);
    } catch (e) {
      setError(String(e));
      return;
    }
    const result = await task(async () => {
      if (selectedToolDef) {
        await invoke('mcp_validate_tool_arguments', {
          tool: selectedTool,
          schema: selectedToolDef.inputSchema ?? {},
          arguments: args,
        });
      }
      return invoke<ToolResult>('mcp_call_tool', {
        server: toServerConfig(selectedServer),
        tool: selectedTool,
        arguments: args,
        apiKey: null,
      });
    });
    if (result) {
      setToolResult(result);
      setStatus(result.isError ? 'The tool reported an error — see the result.' : 'Tool call completed.');
    }
  }

  function stopStatusPoll() {
    statusPollRef.current.active = false;
    if (statusPollRef.current.timer !== null) window.clearTimeout(statusPollRef.current.timer);
    setPolling(false);
  }

  async function pollStatusTool() {
    if (!selectedServer || polling) return;
    let args: Record<string, unknown>;
    try {
      args = parseJsonObject(statusArgsText);
    } catch (e) {
      setError(String(e));
      return;
    }
    setPolling(true);
    setPollNote('');
    statusPollRef.current.active = true;
    let attempts = 0;
    const tick = async () => {
      if (!statusPollRef.current.active) return;
      attempts += 1;
      try {
        const result = await invoke<ToolResult>('mcp_call_tool', {
          server: toServerConfig(selectedServer),
          tool: statusTool,
          arguments: args,
          apiKey: null,
        });
        setToolResult(result);
        if (result.isError) {
          setPollNote(`Status check ${attempts}: the tool reported an error. Polling stopped.`);
          stopStatusPoll();
          return;
        }
        if (result.mediaUrls.length > 0) {
          setPollNote(`Status check ${attempts}: media is ready — download it below.`);
          stopStatusPoll();
          return;
        }
        setPollNote(`Status check ${attempts}: still pending.`);
      } catch (e) {
        setPollNote(`Status check ${attempts} failed: ${String(e)} Polling stopped.`);
        stopStatusPoll();
        return;
      }
      if (attempts >= POLL_MAX_ATTEMPTS) {
        setPollNote('Polling stopped after 10 minutes without a finished result.');
        stopStatusPoll();
        return;
      }
      statusPollRef.current.timer = window.setTimeout(() => void tick(), POLL_INTERVAL_MS);
    };
    void tick();
  }

  async function submitHiggsfield() {
    let params: Record<string, unknown>;
    try {
      params = parseJsonObject(hfParamsText);
    } catch (e) {
      setError(String(e));
      return;
    }
    const apiKey = takePerCallKey();
    const job = await task(() =>
      invoke<HiggsfieldJob>('mcp_higgsfield_submit', {
        endpoint: hfEndpoint.trim(),
        params,
        apiKey,
      }),
    );
    if (job) {
      setHfJob(job);
      setHfStatus(null);
      setStatus(`Higgsfield request ${job.requestId} submitted. Poll the status until it completes.`);
    }
  }

  function stopHfPoll() {
    hfPollRef.current.active = false;
    if (hfPollRef.current.timer !== null) window.clearTimeout(hfPollRef.current.timer);
    setHfPolling(false);
  }

  async function pollHiggsfield() {
    if (!hfJob || hfPolling) return;
    setHfPolling(true);
    hfPollRef.current.active = true;
    let attempts = 0;
    const tick = async () => {
      if (!hfPollRef.current.active || !hfJob) return;
      attempts += 1;
      try {
        const result = await invoke<HiggsfieldStatus>('mcp_higgsfield_status', {
          requestId: hfJob.requestId,
          apiKey: null,
        });
        setHfStatus(result);
        if (result.state !== 'pending') {
          setStatus(result.state === 'succeeded' ? 'Higgsfield generation completed.' : `Higgsfield generation ${result.status}.`);
          stopHfPoll();
          return;
        }
      } catch (e) {
        setError(String(e));
        stopHfPoll();
        return;
      }
      if (attempts >= POLL_MAX_ATTEMPTS) {
        setStatus('Polling stopped after 10 minutes. The request id is kept above — poll again later.');
        stopHfPoll();
        return;
      }
      hfPollRef.current.timer = window.setTimeout(() => void tick(), POLL_INTERVAL_MS);
    };
    void tick();
  }

  async function cancelHiggsfield() {
    if (!hfJob) return;
    stopHfPoll();
    await task(async () => {
      await invoke('mcp_higgsfield_cancel', { requestId: hfJob.requestId, apiKey: null });
      setStatus('Cancel requested. Queued requests stop; requests already in progress continue.');
    });
  }

  async function discoverOAuth() {
    const discovery = await task(() => invoke<OAuthDiscovery>('mcp_oauth_discover_higgsfield'));
    if (discovery) {
      setOauthDiscovery(discovery);
      setStatus('Higgsfield OAuth endpoints discovered. No account or credential was used.');
    }
  }

  async function registerOAuthClient() {
    if (!oauthRedirectUri.trim()) {
      setError('Enter a redirect URI.');
      return;
    }
    const registration = await task(() =>
      invoke<OAuthClientRegistration>('mcp_oauth_register_client', { redirectUri: oauthRedirectUri.trim() }),
    );
    if (registration) {
      setOauthClient(registration);
      setStatus(
        registration.clientSecret
          ? 'Client registered. The client secret is kept in memory for this session only and is not shown.'
          : 'Client registered as a public client (no client secret was issued).',
      );
    }
  }

  async function buildOAuthUrl() {
    if (!oauthDiscovery || !oauthClient) return;
    const pair = await task(() => invoke<PkcePair>('mcp_oauth_pkce_pair'));
    if (!pair) return;
    oauthPkceRef.current = pair;
    const url = await task(() =>
      invoke<string>('mcp_oauth_authorization_url', {
        authorizationEndpoint: oauthDiscovery.authorizationEndpoint,
        clientId: oauthClient.clientId,
        redirectUri: oauthRedirectUri.trim(),
        codeChallenge: pair.challenge,
        state: crypto.randomUUID(),
      }),
    );
    if (url) {
      setOauthUrl(url);
      setStatus('Sign-in URL built. Paste it into a browser and sign in with the Higgsfield account.');
    }
  }

  async function copyOAuthUrl() {
    if (!oauthUrl) return;
    try {
      await navigator.clipboard.writeText(oauthUrl);
      setStatus('Sign-in URL copied.');
    } catch {
      setStatus('Copy did not work in this browser — select the sign-in URL and copy it by hand.');
    }
  }

  async function exchangeOAuthCode() {
    if (!oauthDiscovery || !oauthClient) return;
    if (!selectedServer) {
      setError('Add and select the Higgsfield (hosted MCP) server above first — the token is stored under that server.');
      return;
    }
    if (!oauthCode.trim()) {
      setError('Paste the authorization code from the browser address bar.');
      return;
    }
    if (!oauthPkceRef.current) {
      setError('Build the sign-in URL first — the PKCE verifier from that step is required to exchange the code.');
      return;
    }
    const summary = await task(() =>
      invoke<OAuthTokenSummary>('mcp_oauth_exchange_code', {
        tokenEndpoint: oauthDiscovery.tokenEndpoint,
        serverId: selectedServer.id,
        clientId: oauthClient.clientId,
        clientSecret: oauthClient.clientSecret ?? null,
        code: oauthCode.trim(),
        redirectUri: oauthRedirectUri.trim(),
        codeVerifier: oauthPkceRef.current?.verifier ?? '',
        storage,
      }),
    );
    if (summary) {
      setOauthToken(summary);
      setOauthCode('');
      setStatus('Higgsfield access token stored. Test the connection above to list the hosted tools.');
    }
  }

  async function refreshOAuthToken() {
    if (!oauthDiscovery || !oauthClient) return;
    if (!selectedServer) {
      setError('Add and select the Higgsfield (hosted MCP) server above first — the token is stored under that server.');
      return;
    }
    const summary = await task(() =>
      invoke<OAuthTokenSummary>('mcp_oauth_refresh_token', {
        tokenEndpoint: oauthDiscovery.tokenEndpoint,
        serverId: selectedServer.id,
        clientId: oauthClient.clientId,
        clientSecret: oauthClient.clientSecret ?? null,
        storage,
      }),
    );
    if (summary) {
      setOauthToken(summary);
      setStatus('Higgsfield access token refreshed and stored.');
    }
  }

  async function downloadAndImport(media: MediaCandidate) {
    if (!project) {
      setError('Create or open a project first — imports go into the media bin.');
      return;
    }
    const fileName = `mcp-${Date.now()}.${extensionOf(media.url)}`;
    setDownloading(media.url);
    setError('');
    try {
      const path = await invoke<string>('mcp_download_result', { url: media.url, fileName });
      await importMedia([path]);
      setStatus(`Downloaded and imported to the media bin: ${fileName}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setDownloading('');
    }
  }

  function formFieldInput(field: FormField) {
    const value = paramValue(field.name);
    const label = field.title ?? field.name;
    const suffix = field.required ? ' (required)' : '';
    const hint = field.description ?? '';
    if (field.enumValues.length > 0) {
      return <label key={field.name}>{label}{suffix}
        <select
          value={typeof value === 'string' || typeof value === 'number' ? String(value) : ''}
          disabled={working}
          onChange={e => {
            const next = e.target.value;
            if (next === '') {
              setParam(field.name, undefined);
              return;
            }
            const match = field.enumValues.find(v => String(v) === next);
            setParam(field.name, match === undefined ? next : match);
          }}
        >
          {!field.required && <option value="">—</option>}
          {field.enumValues.map(v => <option key={String(v)} value={String(v)}>{String(v)}</option>)}
        </select>
        {hint && <span>{hint}</span>}
      </label>;
    }
    if (field.fieldType === 'boolean') {
      return <label key={field.name}>{label}{suffix}
        <input type="checkbox" checked={value === true} disabled={working} onChange={e => setParam(field.name, e.target.checked)} />
        {hint && <span>{hint}</span>}
      </label>;
    }
    if (field.fieldType === 'integer' || field.fieldType === 'number') {
      return <label key={field.name}>{label}{suffix}
        <input
          type="number"
          value={typeof value === 'number' ? value : ''}
          min={field.minimum ?? undefined}
          max={field.maximum ?? undefined}
          step={field.fieldType === 'integer' ? 1 : 'any'}
          disabled={working}
          onChange={e => {
            const next = e.target.value === '' ? Number.NaN : Number(e.target.value);
            setParam(field.name, Number.isNaN(next) ? undefined : next);
          }}
        />
        {hint && <span>{hint}</span>}
      </label>;
    }
    if (field.fieldType === 'array') {
      return <label key={field.name}>{label}{suffix}
        <textarea
          rows={3}
          value={Array.isArray(value) ? (value as unknown[]).join('\n') : ''}
          disabled={working}
          spellCheck={false}
          placeholder="One value per line"
          onChange={e => {
            const lines = e.target.value.split('\n').map(l => l.trim()).filter(l => l !== '');
            setParam(field.name, lines.length > 0 ? lines : undefined);
          }}
        />
        {hint && <span>{hint}</span>}
      </label>;
    }
    if (field.fieldType === 'object') {
      return <p key={field.name}>{label}{suffix} — edit this object in Advanced parameters (JSON) below.{hint ? ` ${hint}` : ''}</p>;
    }
    if (field.name === 'prompt' || (field.maxLength !== null && field.maxLength !== undefined && field.maxLength > 200)) {
      return <label key={field.name}>{label}{suffix}
        <textarea
          rows={3}
          value={typeof value === 'string' ? value : ''}
          maxLength={field.maxLength ?? undefined}
          disabled={working}
          spellCheck={false}
          onChange={e => setParam(field.name, e.target.value === '' && !field.required ? undefined : e.target.value)}
        />
        {hint && <span>{hint}</span>}
      </label>;
    }
    return <label key={field.name}>{label}{suffix}
      <input
        type="text"
        value={typeof value === 'string' ? value : ''}
        maxLength={field.maxLength ?? undefined}
        disabled={working}
        spellCheck={false}
        onChange={e => setParam(field.name, e.target.value === '' && !field.required ? undefined : e.target.value)}
      />
      {hint && <span>{hint}</span>}
    </label>;
  }

  const formBlocked = presetId === 'higgsfield-rest';
  const presetNote = selectedPreset && !selectedPreset.available
    ? 'OAuth sign-in is implemented but untested end to end. Add the server, complete the OAuth steps in the Higgsfield (hosted MCP) section below, then test the connection.'
    : selectedPreset?.id === 'higgsfield-rest'
      ? 'The Higgsfield REST API does not use an MCP server entry. Use the Higgsfield section below.'
      : null;

  return <section className="director-section" aria-label="MCP servers and generation">
    <div className="director-bar"><div><span className="eyebrow">EXTERNAL GENERATION TOOLS</span><h2>MCP Servers</h2><p>Connect a Model Context Protocol server or the Higgsfield API, run a generation tool, and import the result into the media bin.</p></div><button className="button secondary" aria-expanded={open} onClick={() => setOpen(v => !v)}>{open ? 'Close MCP' : 'Open MCP'}</button></div>
    {open && <div className="director-layout"><div className="director-setup">
      {!native && <p className="director-notice">MCP requires the desktop app. Server settings saved here are kept locally; credentials are never stored in the browser.</p>}
      {presetsError && <p role="alert" className="error">{presetsError}</p>}
      <h3>1. Add a server</h3>
      <label>Preset<select value={presetId} disabled={!native || working} onChange={e => applyPreset(e.target.value)}>
        {presets.length === 0 && <option value="hyperframes">HyperFrames</option>}
        {presets.map(p => <option key={p.id} value={p.id}>{p.name}{p.available ? '' : ' (unavailable)'}</option>)}
        {!presets.some(p => p.id === 'custom') && <option value="custom">Custom server</option>}
      </select></label>
      {selectedPreset && <p className="director-notice">{selectedPreset.description}</p>}
      {presetNote && <p className="director-notice">{presetNote}</p>}
      {!formBlocked && <fieldset disabled={!native || working}>
        <label>Server name<input value={name} onChange={e => setName(e.target.value)} maxLength={120} placeholder="HyperFrames" /></label>
        <label>Transport<select value={transport} onChange={e => setTransport(e.target.value as 'http' | 'stdio')}><option value="http">Remote (Streamable HTTP)</option><option value="stdio">Local (stdio command)</option></select></label>
        {transport === 'http'
          ? <label>Server URL<input value={url} onChange={e => setUrl(e.target.value)} spellCheck={false} placeholder="https://hyperframes.net/api/mcp" maxLength={2048} /></label>
          : <label>Command<input value={command} onChange={e => setCommand(e.target.value)} spellCheck={false} placeholder="npx -y my-mcp-server" maxLength={4096} /></label>}
        <div className="director-actions"><button className="button secondary" disabled={!name.trim()} onClick={addServer}>Add server</button></div>
        <p>Remote servers require HTTPS; plain HTTP is accepted only for localhost. Local servers run as a child process and are terminated after each operation.</p>
      </fieldset>}
      {servers.length > 0 && <div><h3>Saved servers</h3>{servers.map(s => <div key={s.id} className="director-notice">
        <strong>{s.name}</strong><p>{s.transport === 'http' ? s.url : s.command}</p>
        <div className="director-actions">
          <button className="button secondary" disabled={working} onClick={() => selectServer(s.id)}>{selectedId === s.id ? 'Selected' : 'Select'}</button>
          <button className="button secondary" disabled={working} onClick={() => removeServer(s.id)}>Remove</button>
        </div>
      </div>)}</div>}
      {selectedServer && <div><h3>2. Credential</h3>
        <label>Credential storage<select value={storage} disabled={!native} onChange={e => setStorage(e.target.value as 'os' | 'session')}><option value="os">OS credential store (recommended)</option><option value="session">Session only — cleared on app exit</option></select></label>
        <label>API key<input ref={keyRef} type="password" autoComplete="off" spellCheck={false} disabled={!native} maxLength={8192} placeholder="Stored only by Rust, never in project data" /></label>
        <div className="director-actions">
          <button className="button secondary" disabled={!native || working} onClick={() => void saveCredential()}>Save credential</button>
          <button className="button secondary" disabled={!native || working} onClick={() => void forgetCredential()}>Forget credential</button>
        </div>
        <p>For a connection test you may also type a key and test without saving it: the key is used for that call only and cleared from the field afterwards.</p>
        <div className="director-actions">
          <button className="button secondary" disabled={!native || working} onClick={() => void testConnection()}>Test connection</button>
          <button className="button secondary" disabled={!native || working} onClick={() => void listTools()}>List tools</button>
        </div>
        {testResult && <div><p>Protocol: {testResult.protocolVersion ?? 'not reported'}</p>{testResult.tools.length > 0 ? <ul>{testResult.tools.map(t => <li key={t.name}><strong>{t.name}</strong>{t.description ? ` — ${t.description}` : ''}</li>)}</ul> : <p>No tools listed.</p>}</div>}
      </div>}
      {selectedServer && tools.length > 0 && <div><h3>3. Run a tool</h3>
        <label>Tool<select value={selectedTool} disabled={working} onChange={e => selectTool(e.target.value)}>{tools.map(t => <option key={t.name} value={t.name}>{t.name}</option>)}</select></label>
        {selectedToolDef?.description && <p>{selectedToolDef.description}</p>}
        {formFields.length > 0
          ? <div>
              <p>Fields are built from the server's live tool schema (tools/list). The values are assembled into the JSON below before the call.</p>
              {selectedServer.presetId === 'hyperframes' && <p className="director-notice">The first live generate_video call with these fields has not been tested in this environment.</p>}
              {formFields.map(f => formFieldInput(f))}
            </div>
          : <p className="director-notice">The server reported no parameter schema for this tool — use the JSON editor below.</p>}
        <details>
          <summary>Advanced parameters (JSON)</summary>
          <label>Parameters (JSON)<textarea rows={6} value={paramsText} onChange={e => setParamsText(e.target.value)} spellCheck={false} /></label>
          <p>The form above and this editor change the same parameters. Arguments are validated against the tool schema before the call is sent.</p>
        </details>
        <div className="director-actions"><button className="button primary" disabled={!native || working || !selectedTool} onClick={() => void runTool()}>Run tool</button></div>
        {toolResult && <div>
          <p>{toolResult.isError ? 'The tool reported an error.' : 'Tool call completed.'}</p>
          {toolResult.text && <pre>{toolResult.text}</pre>}
          {toolResult.structuredContent !== undefined && <details><summary>Structured result</summary><pre>{JSON.stringify(toolResult.structuredContent, null, 2)}</pre></details>}
          {toolResult.mediaUrls.length > 0 && <div><h4>Generated media</h4>{toolResult.mediaUrls.map(m => <div key={m.url} className="director-actions">
            <span>{m.kind} — {m.label ?? 'result'}</span>
            <button className="button secondary" disabled={!native || !project || busy || downloading === m.url} onClick={() => void downloadAndImport(m)}>{downloading === m.url ? 'Downloading…' : 'Download and import to media bin'}</button>
          </div>)}</div>}
        </div>}
        <h4>Poll a long-running job</h4>
        <p>For asynchronous tools, repeat a status tool until the media URL appears. The result above is replaced by each status check.</p>
        <label>Status tool<select value={statusTool} disabled={working} onChange={e => setStatusTool(e.target.value)}>{tools.map(t => <option key={t.name} value={t.name}>{t.name}</option>)}</select></label>
        <label>Status arguments (JSON)<textarea rows={3} value={statusArgsText} onChange={e => setStatusArgsText(e.target.value)} spellCheck={false} /></label>
        <div className="director-actions">
          {polling
            ? <button className="button secondary" onClick={stopStatusPoll}>Stop polling</button>
            : <button className="button secondary" disabled={!native || working || !statusTool} onClick={() => void pollStatusTool()}>Start polling (5 s interval, 10 min limit)</button>}
        </div>
        {pollNote && <p role="status">{pollNote}</p>}
      </div>}
      <div><h3>Higgsfield (REST API)</h3>
        <p>Primary Higgsfield path: the official REST API with a KEY_ID:KEY_SECRET pair. Generations are asynchronous and billed per generation against a prepaid dollar balance.</p>
        {hfModelsNote && <p className="director-notice">{hfModelsNote}</p>}
        <label>Model<select value={hfEndpoint} disabled={!native || working} onChange={e => setHfEndpoint(e.target.value)}>
          {hfModels.map(m => <option key={m.id} value={m.id}>{m.name} ({m.kind})</option>)}
          {!hfModels.some(m => m.id === hfEndpoint) && <option value={hfEndpoint}>{hfEndpoint}</option>}
        </select></label>
        <label>Model endpoint (or paste another from the official API reference)<input value={hfEndpoint} onChange={e => setHfEndpoint(e.target.value)} spellCheck={false} disabled={!native || working} maxLength={200} /></label>
        <label>Parameters (JSON)<textarea rows={6} value={hfParamsText} onChange={e => setHfParamsText(e.target.value)} spellCheck={false} /></label>
        <p>Use the credential field above for the KEY_ID:KEY_SECRET pair, or type it here for one call only. Save it with the button below first if you want it stored.</p>
        <div className="director-actions">
          <button className="button secondary" disabled={!native || working} onClick={() => void saveHiggsfieldCredential()}>Save Higgsfield credential</button>
          <button className="button primary" disabled={!native || working || !hfEndpoint.trim()} onClick={() => void submitHiggsfield()}>Submit generation</button>
        </div>
        {hfJob && <div>
          <p>Request id: {hfJob.requestId}</p>
          <div className="director-actions">
            {hfPolling
              ? <button className="button secondary" onClick={stopHfPoll}>Stop polling</button>
              : <button className="button secondary" disabled={!native || working} onClick={() => void pollHiggsfield()}>Poll status (5 s interval, 10 min limit)</button>}
            <button className="button secondary" disabled={!native || working} onClick={() => void cancelHiggsfield()}>Cancel request</button>
          </div>
          {hfStatus && <div>
            <p>Status: {hfStatus.status} ({hfStatus.state})</p>
            {hfStatus.mediaUrls.length > 0 && <div><h4>Generated media</h4>{hfStatus.mediaUrls.map(m => <div key={m.url} className="director-actions">
              <span>{m.kind} — {m.label ?? 'result'}</span>
              <button className="button secondary" disabled={!native || !project || busy || downloading === m.url} onClick={() => void downloadAndImport(m)}>{downloading === m.url ? 'Downloading…' : 'Download and import to media bin'}</button>
            </div>)}</div>}
            <details><summary>Full status response</summary><pre>{JSON.stringify(hfStatus.detail, null, 2)}</pre></details>
          </div>}
        </div>}
      </div>
      <div><h3>Higgsfield (hosted MCP, OAuth)</h3>
        <p>Sign in with a Higgsfield account through the hosted MCP server's OAuth flow. This flow is implemented but untested end to end: no sign-in has been completed from this app, and the redirect receiver is not implemented yet.</p>
        <p>Steps: discover the endpoints, register this app, open the sign-in URL in a browser, then paste the authorization code back here. Tokens are stored with the credential storage selected above, under the selected server.</p>
        <label>Redirect URI<input value={oauthRedirectUri} onChange={e => setOauthRedirectUri(e.target.value)} spellCheck={false} disabled={!native || working} maxLength={2048} /></label>
        <p>Nothing listens on this address yet. After sign-in the browser shows a connection error — the authorization code is still in the address bar. Copy the code value from the address bar and paste it below.</p>
        <div className="director-actions">
          <button className="button secondary" disabled={!native || working} onClick={() => void discoverOAuth()}>1. Discover endpoints</button>
          <button className="button secondary" disabled={!native || working || !oauthDiscovery} onClick={() => void registerOAuthClient()}>2. Register this app</button>
          <button className="button secondary" disabled={!native || working || !oauthDiscovery || !oauthClient} onClick={() => void buildOAuthUrl()}>3. Build sign-in URL</button>
        </div>
        {oauthDiscovery && <div>
          <p>Authorization endpoint: {oauthDiscovery.authorizationEndpoint}</p>
          <p>Token endpoint: {oauthDiscovery.tokenEndpoint}</p>
          <p>Registration endpoint: {oauthDiscovery.registrationEndpoint ?? 'not published'}</p>
          <p>Scopes: {oauthDiscovery.scopesSupported.join(', ') || 'not listed'}</p>
          <p>PKCE methods: {oauthDiscovery.codeChallengeMethodsSupported.join(', ') || 'not listed'}</p>
          <p>Authorization servers: {oauthDiscovery.authorizationServers.join(', ') || 'not listed'}</p>
          {oauthDiscovery.clientIdMetadataDocumentSupported && <p>Note: the server reports support for client ID metadata documents. Whether dynamic client registration remains accepted is unverified.</p>}
        </div>}
        {oauthClient && <p>Client id: {oauthClient.clientId}{oauthClient.clientSecret ? ' — a client secret was issued and is kept in memory for this session only.' : ' — no client secret was issued.'}</p>}
        {oauthUrl && <div>
          <label>Sign-in URL<input value={oauthUrl} readOnly spellCheck={false} /></label>
          <div className="director-actions"><button className="button secondary" disabled={!native || working} onClick={() => void copyOAuthUrl()}>Copy sign-in URL</button></div>
          <p>Paste this URL into a browser, sign in with the Higgsfield account, and copy the code from the address bar.</p>
          <label>Authorization code<input value={oauthCode} onChange={e => setOauthCode(e.target.value)} spellCheck={false} disabled={!native || working} maxLength={2048} placeholder="Paste the code from the browser address bar" /></label>
          <div className="director-actions">
            <button className="button primary" disabled={!native || working || !selectedServer || !oauthCode.trim()} onClick={() => void exchangeOAuthCode()}>4. Exchange code for token</button>
            <button className="button secondary" disabled={!native || working || !selectedServer || !oauthDiscovery || !oauthClient} onClick={() => void refreshOAuthToken()}>Refresh token</button>
          </div>
          {!selectedServer && <p>Select a Higgsfield (hosted MCP) server above first — the token is stored under that server.</p>}
        </div>}
        {oauthToken && <p>Token stored: {oauthToken.tokenType}{oauthToken.expiresIn ? `, valid for ${oauthToken.expiresIn} seconds` : ''}{oauthToken.hasRefreshToken ? ', with a refresh token.' : ', without a refresh token.'}</p>}
      </div>
    </div><aside className="director-review" aria-label="MCP notes"><h3>Notes</h3>
      <p className="director-badge">KEYS STAY IN THE OS STORE OR SESSION MEMORY</p>
      <p>Credentials are never written to project files or preferences. Downloads are saved under the app data directory and imported through the existing media import, so thumbnails, waveforms, and the media index are built the same way as any other import.</p>
      <p>HyperFrames video generation is asynchronous: run generate_video, copy the task id into the status arguments, and poll get_generation until the media URL appears.</p>
      <p>The hosted Higgsfield MCP server signs in with browser OAuth. The authorization-code flow with PKCE is implemented in the section above but untested end to end; the REST API preset is the tested Higgsfield path.</p>
      {status && <p role="status">{status}</p>}
      {error && <p role="alert" className="error">{error}</p>}
    </aside></div>}
  </section>;
}
