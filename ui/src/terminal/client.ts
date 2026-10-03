import { invoke } from '@tauri-apps/api/core';

export interface Profile { id: string; name: string; executable: string; available: boolean; reason?: string }
export interface Session {
  id: string; title: string; profileId: string; cwd: string; cols: number; rows: number;
  status: 'running' | 'exited'; exitCode?: number; ownerClientId: string | null;
}
export interface Snapshot { ansi: string; cols: number; rows: number; seq: number; kittyKeyboardFlags?: number }
export interface WireEvent {
  event: string; sessionId?: string; data?: string; seq?: number;
  snapshot?: Snapshot; ownerClientId?: string | null; exitCode?: number;
}
export class TerminalRequestError extends Error {
  constructor(message: string, readonly code: string) { super(message); }
}
export const retryableTerminalError = (error: unknown) => !(error instanceof TerminalRequestError
  && ['UNAUTHORIZED', 'PIN_MISMATCH', 'UNSUPPORTED_HOST'].includes(error.code));

interface Endpoint { localUrl: string; adminToken: string; protocolVersion: number }
interface Pending { resolve: (value: unknown) => void; reject: (error: Error) => void; timer: ReturnType<typeof setTimeout> }

export const openTerminal = (sessionId?: string) => invoke<void>('open_terminal_window', { sessionId: sessionId ?? null });

/** One authenticated socket per view. Disconnect rejects, never replays mutations. */
export class TerminalClient {
  readonly clientId = crypto.randomUUID();
  private socket?: WebSocket;
  private pending = new Map<string, Pending>();
  private listeners = new Set<(event: WireEvent) => void>();
  private disposed = false;
  private generation = 0;
  private connecting?: Promise<void>;
  private cancelConnect?: (error: Error) => void;
  private authenticated = false;
  onDisconnect: (error: Error) => void = () => {};
  onEvent(handler: (event: WireEvent) => void) {
    this.listeners.add(handler);
    return () => { this.listeners.delete(handler); };
  }
  connect(): Promise<void> {
    if (this.disposed) return Promise.reject(new Error('Terminal view closed'));
    if (this.connecting) return this.connecting;
    if (this.authenticated) return Promise.resolve();
    const generation = ++this.generation;
    const cancelled = new Promise<never>((_, reject) => { this.cancelConnect = reject; });
    const attempt = this.dial(generation);
    // Includes a stalled native endpoint lookup, not just the WebSocket handshake.
    let deadline: ReturnType<typeof setTimeout>;
    const timeout = new Promise<never>((_, reject) => {
      deadline = setTimeout(() => reject(new Error('Terminal host connection timed out')), 20000);
    });
    const connecting = Promise.race([attempt, cancelled, timeout]).catch(error => {
      if (generation === this.generation) this.fail(error);
      throw error;
    }).finally(() => {
      clearTimeout(deadline);
      if (this.connecting === connecting) { this.connecting = undefined; this.cancelConnect = undefined; }
    });
    this.connecting = connecting;
    return connecting;
  }
  private async dial(generation: number) {
    const endpoint = await invoke<Endpoint>('terminal_endpoint');
    if (this.disposed || generation !== this.generation) throw new Error('Terminal connection superseded');
    if (endpoint.protocolVersion !== 1 || !/^ws:\/\/127\.0\.0\.1:\d+$/.test(endpoint.localUrl))
      throw new TerminalRequestError('Unsupported terminal host', 'UNSUPPORTED_HOST');
    const socket = this.socket = new WebSocket(endpoint.localUrl);
    const current = () => !this.disposed && generation === this.generation && socket === this.socket;
    socket.onmessage = ({ data }: MessageEvent<string>) => {
      if (!current()) return;
      try {
        if (typeof data !== 'string' || data.length > 8 * 1024 * 1024) throw new Error('Terminal host message too large');
        const message = JSON.parse(data);
        if (typeof message.id === 'string') {
          const pending = this.pending.get(message.id);
          if (!pending) return;
          clearTimeout(pending.timer);
          this.pending.delete(message.id);
          if (message.error) pending.reject(new TerminalRequestError(message.error.message || 'Terminal request failed', message.error.code));
          else pending.resolve(message.result);
        } else if (typeof message.event === 'string') {
          if (this.authenticated) this.listeners.forEach(handler => handler(message));
        } else throw new Error('Invalid terminal host message');
      } catch { this.fail('Invalid terminal host message; reconnect to resynchronize'); }
    };
    socket.onclose = () => { if (current()) this.fail('Terminal host disconnected. Reconnecting; input will not be replayed.'); };
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => { reject(new Error('Terminal host connection timed out')); socket.close(); }, 10000);
      socket.onopen = () => { clearTimeout(timer); resolve(); };
      socket.onerror = () => { clearTimeout(timer); reject(new Error('Cannot connect to terminal host')); };
      socket.addEventListener('close', () => { clearTimeout(timer); reject(new Error('Terminal host connection closed')); }, { once: true });
    });
    if (!current()) throw new Error('Terminal connection superseded');
    await this.request('auth', { token: endpoint.adminToken, clientId: this.clientId, clientType: 'desktop' });
    if (!current()) throw new Error('Terminal connection superseded');
    this.authenticated = true;
  }
  request<T = unknown>(method: string, params: object = {}): Promise<T> {
    const socket = this.socket;
    if (this.disposed || socket?.readyState !== WebSocket.OPEN || (!this.authenticated && method !== 'auth')) return Promise.reject(new Error('Terminal host not connected'));
    if (this.pending.size >= 128 || socket.bufferedAmount > 1024 * 1024) {
      this.fail('Terminal connection overloaded. Reconnect; input was not replayed.');
      return Promise.reject(new Error('Terminal connection overloaded'));
    }
    const id = crypto.randomUUID();
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out; outcome unknown. Refresh before retrying.`));
        this.fail('Terminal request timed out; reconnect to resynchronize.');
      }, 15000);
      this.pending.set(id, { resolve: value => resolve(value as T), reject, timer });
      try { socket.send(JSON.stringify({ id, method, params })); }
      catch { this.fail('Terminal send failed; input was not replayed.'); }
    });
  }
  private fail(cause: string | Error) {
    const error = typeof cause === 'string' ? new Error(cause) : cause;
    const hadConnection = !!this.socket || !!this.cancelConnect;
    this.generation++; this.authenticated = false;
    this.cancelConnect?.(error); this.cancelConnect = undefined; this.connecting = undefined;
    this.pending.forEach(({ reject, timer }) => { clearTimeout(timer); reject(error); });
    this.pending.clear();
    const socket = this.socket;
    this.socket = undefined;
    if (socket) { socket.onclose = null; socket.close(); }
    if (!this.disposed && hadConnection) this.onDisconnect(error);
  }
  close() { this.disposed = true; this.fail('Terminal view closed'); this.listeners.clear(); }
}

/** Each recovery owns a fresh socket; only callers' read/subscription setup runs again. */
export function observeTerminalConnection(
  ready: (client: TerminalClient) => void,
  offline: (error: Error, retrying: boolean) => void,
) {
  let active = true;
  let connection: TerminalClient;
  let timer: ReturnType<typeof setTimeout> | undefined;
  function connect() {
    const current = connection = new TerminalClient();
    let failed = false;
    const fail = (error: Error) => {
      if (!active || current !== connection || failed) return;
      failed = true; current.close();
      const retrying = retryableTerminalError(error);
      offline(error, retrying);
      if (retrying) timer = setTimeout(connect, 1500);
    };
    current.onDisconnect = fail;
    void current.connect().then(() => {
      if (active && current === connection && !failed) ready(current);
    }).catch(fail);
  }
  connect();
  return () => { active = false; clearTimeout(timer); connection.close(); };
}
