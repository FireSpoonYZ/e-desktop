import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import ts from 'typescript';

const source = readFileSync(new URL('../src/terminal/client.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } });
const code = outputText.replace("'@tauri-apps/api/core'", JSON.stringify(import.meta.resolve('@tauri-apps/api/core')));
const { TerminalClient, observeTerminalConnection } = await import(`data:text/javascript;base64,${Buffer.from(code).toString('base64')}`);
const tick = () => new Promise(resolve => setImmediate(resolve));
const endpoint = { protocolVersion: 1, localUrl: 'ws://127.0.0.1:7768', adminToken: 'test' };

function harness(t, invoke = async () => endpoint) {
  const previous = { window: globalThis.window, WebSocket: globalThis.WebSocket };
  const sockets = [];
  class Socket {
    static OPEN = 1;
    readyState = 0;
    bufferedAmount = 0;
    sent = [];
    closes = [];
    constructor() { sockets.push(this); }
    addEventListener(name, callback) { if (name === 'close') this.closes.push(callback); }
    open() { this.readyState = 1; this.onopen?.(); }
    send(value) { this.sent.push(JSON.parse(value)); }
    answer(request, result = {}, error) { this.onmessage?.({ data: JSON.stringify({ id: request.id, result, error }) }); }
    close() { this.readyState = 3; this.onclose?.(); this.closes.splice(0).forEach(callback => callback()); }
  }
  globalThis.WebSocket = Socket;
  globalThis.window = { __TAURI_INTERNALS__: { invoke } };
  t.after(() => { globalThis.window = previous.window; globalThis.WebSocket = previous.WebSocket; });
  async function authenticate(index = sockets.length - 1, error) {
    const socket = sockets[index]; socket.open(); await tick();
    assert.equal(socket.sent[0].method, 'auth'); socket.answer(socket.sent[0], {}, error); await tick();
    return socket;
  }
  return { sockets, authenticate };
}

test('TerminalClient coalesces connecting and fences old close/messages after replacement', async t => {
  const { sockets, authenticate } = harness(t);
  const client = new TerminalClient(); t.after(() => client.close());
  let disconnects = 0, events = 0;
  client.onDisconnect = () => disconnects++;
  client.onEvent(() => events++);
  const first = client.connect(); assert.equal(client.connect(), first);
  await tick(); assert.equal(sockets.length, 1);
  const old = await authenticate(); await first;
  const lateClose = old.onclose, lateMessage = old.onmessage;
  const mutation = client.request('terminal.send', { data: 'once' });
  const rejected = assert.rejects(mutation, /disconnected/);
  old.close(); await rejected;
  const next = client.connect(); await tick(); await authenticate(); await next;
  lateClose(); lateMessage({ data: JSON.stringify({ event: 'terminal.output' }) });
  assert.equal(disconnects, 1); assert.equal(events, 0);
  assert.deepEqual(sockets[1].sent.map(value => value.method), ['auth']);
  const read = client.request('terminal.list'); sockets[1].answer(sockets[1].sent.at(-1), []);
  assert.deepEqual(await read, []);
});

test('TerminalClient disposal during endpoint lookup settles immediately and prevents a late socket', async t => {
  let resolveEndpoint;
  const { sockets } = harness(t, () => new Promise(resolve => { resolveEndpoint = resolve; }));
  const client = new TerminalClient();
  const connecting = client.connect();
  const rejected = assert.rejects(connecting, /closed/); client.close(); await rejected;
  resolveEndpoint(endpoint); await tick();
  assert.equal(sockets.length, 0); await assert.rejects(client.connect(), /closed/);
});

test('connection observer recovers auth and one subscription, never replays mutations; disposal cancels retry', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const { sockets, authenticate } = harness(t);
  const ready = [], offline = [];
  const stop = observeTerminalConnection(client => {
    ready.push(client); void client.request('terminal.subscribe', { sessionId: 'shell' }).catch(() => {});
  }, (error, retrying) => offline.push([error.message, retrying]));
  t.after(stop);
  await tick(); const first = await authenticate();
  first.answer(first.sent.at(-1), {}); await tick();
  const pending = ready[0].request('terminal.send', { data: 'do not replay' });
  const rejected = assert.rejects(pending); first.close(); await rejected; await tick();
  assert.equal(offline.length, 1); assert.equal(offline[0][1], true);
  t.mock.timers.tick(1500); await tick(); const second = await authenticate();
  assert.equal(ready.length, 2);
  assert.deepEqual(second.sent.map(value => value.method), ['auth', 'terminal.subscribe']);
  first.onclose?.(); assert.equal(offline.length, 1);
  second.close(); stop(); t.mock.timers.tick(10000); await tick(); assert.equal(sockets.length, 2);
});

test('UNAUTHORIZED stops automatic recovery and stale auth completion cannot publish ready', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const { sockets, authenticate } = harness(t);
  const ready = [], offline = [];
  const stop = observeTerminalConnection(client => ready.push(client), (error, retrying) => offline.push([error.code, retrying]));
  await tick(); await authenticate(0, { code: 'UNAUTHORIZED', message: 'Invalid token' });
  t.mock.timers.tick(60000); await tick();
  assert.deepEqual(offline, [['UNAUTHORIZED', false]]); assert.equal(ready.length, 0); assert.equal(sockets.length, 1);
  stop();
  const stopNext = observeTerminalConnection(client => ready.push(client), () => {});
  await tick(); sockets[1].open(); await tick(); const late = sockets[1].onmessage;
  const auth = sockets[1].sent[0]; stopNext();
  late({ data: JSON.stringify({ id: auth.id, result: {} }) }); await tick();
  assert.equal(ready.length, 0);
});

test('endpoint timeout retires a dial so a late lookup cannot replace the next authenticated socket', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  let resolveOld, calls = 0;
  const { sockets, authenticate } = harness(t, () => ++calls === 1
    ? new Promise(resolve => { resolveOld = resolve; }) : Promise.resolve(endpoint));
  const client = new TerminalClient(); t.after(() => client.close());
  const first = client.connect(); const rejected = assert.rejects(first, /timed out/);
  t.mock.timers.tick(20000); await rejected;
  const second = client.connect(); await tick(); await authenticate(); await second;
  resolveOld(endpoint); await tick(); assert.equal(sockets.length, 1);
});

test('PIN_MISMATCH is terminal rather than a network retry', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const { sockets, authenticate } = harness(t);
  const offline = [];
  const stop = observeTerminalConnection(() => assert.fail('pin rejection cannot connect'),
    (error, retrying) => offline.push([error.code, retrying]));
  t.after(stop);
  await tick(); await authenticate(0, { code: 'PIN_MISMATCH', message: 'Host identity changed' });
  t.mock.timers.tick(60000); await tick();
  assert.deepEqual(offline, [['PIN_MISMATCH', false]]);
  assert.equal(sockets.length, 1);
});

test('view scopes subscribe and pending unsubscribe to one registration and exposes missing-session navigation', () => {
  const view = readFileSync(new URL('../src/terminal/Terminal.tsx', import.meta.url), 'utf8');
  assert.match(view, /const subscriptionId = crypto.randomUUID\(\)/);
  assert.match(view, /'terminal.subscribe', \{ sessionId, subscriptionId \}/);
  assert.match(view, /'terminal.unsubscribe', \{ sessionId, subscriptionId \}/);
  assert.doesNotMatch(view, /if \(subscribed\) void client.request\('terminal.unsubscribe'/);
  assert.match(view, /if \(!active \|\| event.sessionId !== sessionId\) return/);
  assert.match(view, /cause.code === 'NOT_FOUND'/);
  assert.match(view, /返回会话列表/);
});
