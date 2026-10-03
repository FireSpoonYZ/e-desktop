import http from 'node:http';
import https from 'node:https';
import { hostname, homedir, networkInterfaces } from 'node:os';
import { resolve, isAbsolute } from 'node:path';
import { statSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { WebSocketServer, WebSocket } from 'ws';
import { createHostSession } from '../dist/session.mjs';
import { checkNativeDeployment } from '../scripts/check-native-deployment.mjs';
import { terminateSession, captureOwnership } from '../dist/termination.mjs';
import { isIP } from 'node:net';
import { HeadlessEmulator } from '../dist/emulator.mjs';
import { profiles, shellArgs, existingFile } from './profiles.mjs';
import { credentials, secret, hash, equal } from './security.mjs';

const MAX_BUFFER = 8 * 1024 * 1024;
const MAX_REQUEST = 256 * 1024;
function fail(code, message) { throw Object.assign(new Error(message), { code }); }
function string(value, name, max = 256) {
  if (typeof value !== 'string' || !value.length || value.length > max || value.includes('\0'))
    fail('INVALID_PARAMS', 'Invalid ' + name);
  return value;
}
function size(value, fallback, max) {
  if (value === undefined && fallback !== undefined) return fallback;
  if (!Number.isInteger(value) || value < 2 || value > max) fail('INVALID_PARAMS', 'Invalid terminal dimensions');
  return value;
}
export function terminalInput(data, encoding, platform = process.platform) {
  if (encoding !== undefined && encoding !== 'binary') fail('INVALID_PARAMS', 'Unsupported input encoding');
  if (typeof data !== 'string' || (encoding === 'binary' ? data.length : Buffer.byteLength(data)) > 64 * 1024)
    fail('INVALID_PARAMS', 'data must be a string of at most 64 KiB');
  if (encoding !== 'binary') return data;
  for (let i = 0; i < data.length; i++) {
    if (data.charCodeAt(i) > 255) fail('INVALID_PARAMS', 'Binary data must contain only byte-valued code units');
  }
  // node-pty 1.1.0 accepts Buffer: Windows passes it to inSocket.write;
  // Unix CustomWriteStream copies the Buffer and uses fs.write (no UTF-8 step).
  if (!['win32', 'linux', 'darwin'].includes(platform))
    fail('UNSUPPORTED_ENCODING', 'Raw terminal input is unsupported on this platform');
  return Buffer.from(data, 'latin1');
}
const sessionInfo = s => ({ id: s.id, title: s.title, profileId: s.profileId, cwd: s.cwd,
  cols: s.cols, rows: s.rows, status: s.status, ...(s.exitCode !== undefined ? { exitCode: s.exitCode } : {}),
  ownerClientId: s.owner?.clientId ?? null });

export function pairingAddresses(boundAddress, interfaces = networkInterfaces()) {
  if (!['0.0.0.0', '::'].includes(boundAddress)) return [boundAddress];
  const family = isIP(boundAddress);
  return [...new Set(Object.values(interfaces).flat().filter(i =>
    i && !i.internal && isIP(i.address) === family && !i.address.toLowerCase().startsWith('fe80:'))
    .map(i => i.address))];
}

export async function startHost({ dataDir, port = 7768, address = '0.0.0.0' }) {
  // Check before binding sockets or accepting credentials under the selected runtime.
  checkNativeDeployment();
  const security = await credentials(dataDir);
  const adminToken = secret();
  const sessions = new Map(), connections = new Set(), pairingCodes = new Map();
  let stopping = false, remotePort, work = Promise.resolve(), pendingRequests = 0;
  // ponytail: one host queue makes snapshot/subscribe/resize boundaries atomic;
  // per-session queues only if measured multi-session throughput requires them.
  function enqueue(action) {
    const next = work.then(action);
    work = next.catch(() => {});
    return next;
  }
  function send(c, value) {
    if (c.ws.readyState !== WebSocket.OPEN) return;
    const json = JSON.stringify(value);
    if (c.ws.bufferedAmount + Buffer.byteLength(json) > MAX_BUFFER) {
      c.ws.close(1013, 'Slow client; reconnect and subscribe'); return;
    }
    c.ws.send(json);
  }
  function event(value, session) {
    for (const c of connections) if (c.auth && (!session || c.subscriptions.has(session.id))) send(c, value);
  }
  function control(s) { event({ event: 'terminal.control', sessionId: s.id, ownerClientId: s.owner?.clientId ?? null }, s); }
  function admin(c) { if (!c.admin) fail('FORBIDDEN', 'Administrator token required'); }
  function owned(c, s) { if (s.owner !== c) fail('NOT_OWNER', 'Claim terminal control first'); }
  function getSession(p) {
    const s = sessions.get(string(p.sessionId, 'sessionId'));
    if (!s) fail('NOT_FOUND', 'Terminal not found');
    return s;
  }
  function running(s) { if (s.status !== 'running') fail('EXITED', 'Terminal has exited'); }
  function resize(s, cols, rows) {
    running(s);
    cols = size(cols, undefined, 400); rows = size(rows, undefined, 200);
    s.pty.resize(cols, rows);
    s.emulator.resize(cols, rows); s.cols = cols; s.rows = rows;
    event({ event: 'terminal.snapshot', sessionId: s.id, snapshot: s.emulator.getSnapshot(s.seq) }, s);
  }
  function publishExit(s) {
    if (!s.exitResult || s.status === 'exited') return;
    s.status = 'exited'; s.exitCode = s.exitResult.exitCode; s.owner = null; s.pty.setOwner(null); control(s);
    event({ event: 'terminal.exit', sessionId: s.id, exitCode: s.exitCode }, s);
    event({ event: 'terminal.listChanged' });
  }
  function disconnect(c) {
    connections.delete(c);
    c.auth = false;
    for (const s of sessions.values()) if (s.owner === c) { s.owner = null; s.pty.setOwner(null); control(s); }
  }
  async function dispatch(c, method, p) {
    if (method === 'auth') {
      if (c.auth) fail('ALREADY_AUTHENTICATED', 'Connection already authenticated');
      const token = string(p.token, 'token');
      const clientId = string(p.clientId, 'clientId', 128);
      if (!['desktop', 'mobile'].includes(p.clientType)) fail('INVALID_PARAMS', 'Invalid clientType');
      const isAdmin = c.local && equal(token, adminToken);
      const device = security.devices.find(d => equal(d.tokenHash, hash(token)));
      if (!isAdmin && !device) fail('UNAUTHORIZED', 'Invalid token');
      const previous = [...connections].find(other => other !== c && other.auth && other.clientId === clientId);
      if (previous) {
        if (previous.admin !== isAdmin || previous.deviceId !== device?.id)
          fail('CLIENT_ID_IN_USE', 'Client belongs to another authenticated principal');
        // Retire before closing: queued requests and a delayed close cannot affect the replacement.
        disconnect(previous);
        previous.ws.close(1000, 'Connection replaced');
      }
      c.auth = true; c.admin = isAdmin; c.deviceId = device?.id; c.clientId = clientId; c.clientType = p.clientType;
      return { protocolVersion: 1, hostName: hostname() };
    }
    if (method === 'pair') {
      if (c.local || c.auth) fail('FORBIDDEN', 'Pair on an unauthenticated remote connection');
      const code = string(p.code, 'code'), name = string(p.deviceName, 'deviceName', 100);
      const expires = pairingCodes.get(code);
      if (!expires || expires < Date.now()) fail('INVALID_PAIRING_CODE', 'Pairing code expired or invalid');
      if (security.devices.length >= 100) fail('LIMIT', 'Revoke unused devices first');
      pairingCodes.delete(code);
      const token = secret();
      security.devices.push({ id: randomUUID(), name, tokenHash: hash(token) });
      try { security.saveDevices(); } catch (error) { security.devices.pop(); throw error; }
      return { token, hostName: hostname() };
    }
    if (!c.auth) fail('UNAUTHORIZED', 'Authenticate first');
    switch (method) {
      case 'pairing.create': {
        admin(c);
        for (const [code, expiry] of pairingCodes) if (expiry < Date.now()) pairingCodes.delete(code);
        if (pairingCodes.size >= 16) fail('LIMIT', 'Too many pending pairing codes');
        const code = secret(), expiresAt = Date.now() + 300000;
        pairingCodes.set(code, expiresAt);
        const addresses = pairingAddresses(remoteServer.address().address);
        return { descriptor: JSON.stringify({ version: 1, name: hostname(), port: remotePort,
          addresses, fingerprint: security.fingerprint, code }), expiresAt };
      }
      case 'devices.list': admin(c); return security.devices.map(({ id, name }) => ({ id, name }));
      case 'devices.revoke': {
        admin(c);
        const index = security.devices.findIndex(d => d.id === string(p.deviceId, 'deviceId'));
        if (index < 0) fail('NOT_FOUND', 'Device not found');
        const [removed] = security.devices.splice(index, 1);
        try { security.saveDevices(); } catch (error) { security.devices.splice(index, 0, removed); throw error; }
        for (const other of [...connections]) if (other.deviceId === p.deviceId) {
          disconnect(other); other.ws.close(1008, 'Device revoked');
        }
        return {};
      }
      case 'profiles.list': return profiles();
      case 'terminal.list': return [...sessions.values()].map(sessionInfo);
      case 'terminal.create': {
        if (sessions.size >= 32) fail('LIMIT', 'Close an existing terminal first (32 session limit)');
        const profile = profiles().find(item => item.id === p.profileId);
        if (!profile) fail('INVALID_PARAMS', 'Unknown profile');
        let executable = profile.executable;
        if (p.executable !== undefined) {
          if (profile.available) fail('INVALID_PARAMS', 'Executable overrides apply only to unavailable profiles');
          if (!existingFile(p.executable)) fail('INVALID_PARAMS', 'Executable must be an absolute existing executable file');
          executable = p.executable;
        }
        if (!executable) fail('UNAVAILABLE', profile.reason);
        const cwd = p.cwd === undefined ? homedir() : string(p.cwd, 'cwd', 4096);
        if (!isAbsolute(cwd)) fail('INVALID_PARAMS', 'cwd must be absolute');
        try { if (!statSync(cwd).isDirectory()) throw new Error(); }
        catch { fail('INVALID_PARAMS', 'cwd must be an existing directory'); }
        const cols = size(p.cols, 80, 400), rows = size(p.rows, 24, 200);
        const s = { id: randomUUID(), title: profile.name, profileId: profile.id, cwd, cols, rows,
          status: 'running', owner: null, seq: 0, queuedBytes: 0, paused: false };
        s.emulator = new HeadlessEmulator(cols, rows, reply => {
          if (s.status === 'running') s.pty.write(reply);
        });
        if (process.platform === 'win32') s.emulator.installConptyPrimaryDeviceAttributesOverride();
        const onData = data => {
          s.queuedBytes += Buffer.byteLength(data);
          if (s.queuedBytes > 1024 * 1024 && !s.paused) { s.paused = true; s.pty.pause(); }
          enqueue(async () => {
            if (!sessions.has(s.id)) return;
            try {
              await s.emulator.write(data, { forwardQueryReplies: true });
              event({ event: 'terminal.output', sessionId: s.id, data, seq: ++s.seq }, s);
            } finally {
              s.queuedBytes -= Buffer.byteLength(data);
              if (s.paused && s.queuedBytes < 256 * 1024) { s.paused = false; s.pty.resume(); }
            }
          }).catch(() => {
            console.error('Terminal output processing failed');
            void stop().catch(() => console.error('Terminal cleanup failed; host retained for retry.'));
          });
        };
        let resolveExit;
        s.exitPromise = new Promise(resolve => { resolveExit = resolve; });
        const onExit = result => {
          // Never queue this real public/subprocess exit evidence: cleanup may own the queue.
          s.rootExitedAt = Date.now();
          s.exitResult = result;
          resolveExit(result);
          enqueue(() => {
            if (!sessions.has(s.id)) return;
            publishExit(s);
          }).catch(() => {});
        };
        try {
          s.pty = await createHostSession({
            sessionId: s.id, executable, terminalShellArgs: shellArgs(profile.id),
            cols, rows, cwd, dataDir, onData, onExit
          });
        } catch (error) {
          s.emulator.dispose();
          console.error('Terminal spawn failed:', error.message);
          fail('SPAWN_FAILED', 'Could not launch shell; check executable, permissions and patched native deployment');
        }
        sessions.set(s.id, s);
        // Start lifecycle observation immediately, without blocking output or
        // subscribe. Input waits for this initial observation before it can exit
        // the root. Failed observation is not evidence of successful cleanup.
        s.ownershipPromise = captureOwnership(s).catch(() => {});
        event({ event: 'terminal.listChanged' });
        return sessionInfo(s);
      }
      case 'terminal.subscribe': {
        const s = getSession(p);
        const snapshot = s.emulator.getSnapshot(s.seq);
        const subscriptionId = p.subscriptionId === undefined ? undefined : string(p.subscriptionId, 'subscriptionId', 128);
        c.subscriptions.set(s.id, subscriptionId);
        return { session: sessionInfo(s), snapshot };
      }
      case 'terminal.unsubscribe': {
        const sessionId = string(p.sessionId, 'sessionId');
        const subscriptionId = p.subscriptionId === undefined ? undefined : string(p.subscriptionId, 'subscriptionId', 128);
        if (c.subscriptions.get(sessionId) === subscriptionId) c.subscriptions.delete(sessionId);
        return {};
      }
      case 'terminal.claim': {
        const s = getSession(p); running(s);
        if (p.cols !== undefined || p.rows !== undefined) {
          // Validate before changing ownership.
          size(p.cols, undefined, 400); size(p.rows, undefined, 200);
          resize(s, p.cols, p.rows);
        }
        s.owner = c; s.pty.setOwner(c); control(s); return { ownerClientId: c.clientId };
      }
      case 'terminal.release': {
        const s = getSession(p); owned(c, s); s.owner = null; s.pty.setOwner(null); control(s); return {};
      }
      case 'terminal.send': {
        const s = getSession(p); owned(c, s); running(s);
        // Ownership/auth checks precede validation and every write, including binary.
        await s.ownershipPromise;
        if (s.exitResult) fail('EXITED', 'Terminal has exited');
        await s.pty.send(terminalInput(p.data, p.encoding), c.clientId); return { accepted: true };
      }
      case 'terminal.updateViewport': {
        const s = getSession(p); owned(c, s); resize(s, p.cols, p.rows); return {};
      }
      case 'terminal.close': {
        const s = getSession(p);
        await terminateSession(s);
        publishExit(s);
        sessions.delete(s.id); s.pty.dispose(); s.emulator.dispose();
        for (const other of connections) other.subscriptions.delete(s.id);
        event({ event: 'terminal.listChanged' }); return {};
      }
      default: fail('METHOD_NOT_FOUND', 'Unknown method');
    }
  }
  function accept(ws, local) {
    if (connections.size >= 64 || stopping) { ws.close(1013, 'Connection limit'); return; }
    const c = { ws, local, auth: false, subscriptions: new Map(), pending: 0, requests: new Set(), attempts: 0, alive: true };
    connections.add(c);
    const authTimer = setTimeout(() => { if (!c.auth) ws.close(1008, 'Authentication timeout'); }, 15000);
    ws.on('pong', () => { c.alive = true; });
    ws.on('error', () => {});
    ws.on('close', () => { clearTimeout(authTimer); enqueue(() => disconnect(c)); });
    ws.on('message', (bytes, binary) => {
      if (binary || c.pending >= 32 || pendingRequests >= 1024) { ws.close(1008, 'Request limit'); return; }
      let req;
      try {
        req = JSON.parse(bytes.toString());
        if (!req || typeof req !== 'object' || Array.isArray(req)) throw new Error();
        string(req.id, 'id', 128); string(req.method, 'method', 80);
        if (!req.params || typeof req.params !== 'object' || Array.isArray(req.params)) throw new Error();
        if (c.requests.has(req.id)) throw new Error();
        // Bound ID history without permitting a duplicate request's side effects.
        if (c.requests.size >= 100000) { ws.close(1013, 'Reconnect to renew request ID budget'); return; }
        c.requests.add(req.id);
      } catch { send(c, { id: typeof req?.id === 'string' ? req.id : null,
        error: { code: 'INVALID_REQUEST', message: 'Expected a unique id, method and object params' } }); return; }
      c.pending++; pendingRequests++;
      enqueue(async () => {
        if (!connections.has(c) || ws.readyState !== WebSocket.OPEN || stopping) return;
        try {
          if (!c.auth && ++c.attempts > 8) { ws.close(1008, 'Authentication attempt limit'); return; }
          const result = await dispatch(c, req.method, req.params);
          // Response is sent INSIDE the sequencer, before any subsequent PTY event.
          send(c, { id: req.id, result });
        } catch (error) {
          send(c, { id: req.id, error: { code: error.code && typeof error.code === 'string' ? error.code : 'INTERNAL',
            message: error.code ? error.message : 'Host operation failed' } });
        }
      }).finally(() => { c.pending--; pendingRequests--; }).catch(() => {});
    });
  }
  const localServer = http.createServer((_, res) => { res.writeHead(404); res.end(); });
  const remoteServer = https.createServer({ ...security.tls, minVersion: 'TLSv1.2' },
    (_, res) => { res.writeHead(404); res.end(); });
  const localWss = new WebSocketServer({ server: localServer, maxPayload: MAX_REQUEST, perMessageDeflate: false });
  const remoteWss = new WebSocketServer({ server: remoteServer, maxPayload: MAX_REQUEST, perMessageDeflate: false });
  localWss.on('connection', ws => accept(ws, true)); remoteWss.on('connection', ws => accept(ws, false));
  const listen = (server, port, host) => new Promise((resolve, reject) => {
    server.once('error', reject); server.listen(port, host, () => { server.off('error', reject); resolve(); });
  });
  try {
    await listen(remoteServer, port, address); remotePort = remoteServer.address().port;
    await listen(localServer, 0, '127.0.0.1');
  } catch (error) { localWss.close(); remoteWss.close(); localServer.close(); remoteServer.close(); throw error; }
  const heartbeat = setInterval(() => {
    for (const c of connections) {
      if (!c.alive) { c.ws.terminate(); continue; }
      c.alive = false; c.ws.ping();
    }
  }, 30000);
  heartbeat.unref();
  let stopPromise;
  function stop() {
    if (stopPromise) return stopPromise;
    stopping = true;
    stopPromise = enqueue(async () => {
      const results = await Promise.allSettled([...sessions.values()].map(async s => {
        await terminateSession(s);
        publishExit(s);
        s.pty.dispose(); s.emulator.dispose(); sessions.delete(s.id);
      }));
      if (results.some(result => result.status === 'rejected')) {
        stopping = false; stopPromise = undefined;
        throw Object.assign(new Error('Shutdown cleanup failed; host and sessions retained for retry'), { code: 'CLEANUP_FAILED' });
      }
      clearInterval(heartbeat);
      for (const c of connections) c.ws.terminate();
      localWss.close(); remoteWss.close();
      return Promise.all([localServer, remoteServer].map(server => new Promise(resolve => server.close(resolve))));
    });
    return stopPromise;
  }
  return { ready: { type: 'ready', protocolVersion: 1,
    localUrl: 'ws://127.0.0.1:' + localServer.address().port, remotePort, adminToken }, stop };
}

async function main() {
  const args = process.argv.slice(2), options = {};
  for (let i = 0; i < args.length; i += 2) {
    if (!['--data-dir', '--port', '--address'].includes(args[i]) || args[i + 1] === undefined)
      throw new Error('Usage: node src/server.mjs --data-dir <private-directory> [--port 7768] [--address 0.0.0.0]');
    options[args[i].slice(2)] = args[i + 1];
  }
  if (!options['data-dir']) throw new Error('--data-dir is required');
  const port = options.port === undefined ? 7768 : Number(options.port);
  if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error('Invalid --port');
  const host = await startHost({ dataDir: resolve(options['data-dir']), port, address: options.address });
  const shutdown = () => {
    process.stdin.destroy();
    host.stop().then(() => { process.exitCode = 0; }, () => {
      console.error('Terminal cleanup failed; host retained for retry via terminal.close or shutdown signal.');
      process.exitCode = 1;
    });
  };
  process.on('SIGINT', shutdown); process.on('SIGTERM', shutdown);
  // Parent writes exactly "shutdown\\n"; EOF is also an explicit parent exit.
  process.stdin.setEncoding('utf8');
  let input = '';
  process.stdin.on('data', chunk => {
    input += chunk;
    if (input.length > 1024) input = input.slice(-1024);
    if (input.split(/\r?\n/).includes('shutdown')) shutdown();
  });
  process.stdin.once('end', shutdown);
  process.stdout.write(JSON.stringify(host.ready) + '\n');
}
if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main().catch(error => {
    console.error(error.code === 'NATIVE_DEPLOYMENT_FAILED' ? error.message
      : 'Terminal host startup failed. Check Node >=22.12, npm ci in terminal-host, the private data directory permissions, and remote port availability.');
    process.exitCode = 1;
  });
}
