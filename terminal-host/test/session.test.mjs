import test from 'node:test';
import assert from 'node:assert/strict';
import { HostSession } from '../dist/session.mjs';
import { HeadlessEmulator } from '../dist/emulator.mjs';

function subprocess(shellPath = 'bash') {
  const writes = [], sizes = [];
  let dataSink, exitSink;
  return {
    pid: 123, shellPath, writes, sizes,
    getForegroundProcess: () => 'bash', confirmShellForeground: async () => true,
    write: data => writes.push(data), resize: (cols, rows) => sizes.push([cols, rows]),
    kill() { exitSink(7); }, forceKill() { throw new Error('Root-only forceKill must not run'); },
    terminateOwnedTree: () => 'unavailable', signal() {}, pause() {}, resume() {},
    dispose() {}, onData: cb => { dataSink = cb; }, onExit: cb => { exitSink = cb; },
    output: data => dataSink(data)
  };
}

test('actual Session gates text, raw binary and CPR, but resize stays independent; marker flush preserves bytes', async () => {
  const proc = subprocess();
  const exits = [];
  const host = new HostSession({
    sessionId: 'gate', cols: 80, rows: 24, subprocess: proc,
    shellReadySupported: true, onData() {}, onExit: result => exits.push(result)
  });
  try {
    host.setOwner({ clientId: 'phone', clientType: 'mobile' });
    const bytes = Buffer.from(Array.from({ length: 256 }, (_, byte) => byte));
    await host.send('汉字👩‍💻', 'phone');
    await host.send(bytes, 'phone');
    host.write('\x1b[1;1R');
    host.resize(44, 32);
    assert.deepEqual(proc.writes, []);
    assert.deepEqual(proc.sizes, [[44, 32]]);
    assert.equal(host.shellState, 'pending');
    proc.output('\x1b]777;orca-shell-ready\x07prompt$ ');
    // The actual Bash marker is line-editor-ready: no fabricated settle delay.
    assert.equal(host.shellState, 'ready');
    assert.deepEqual(proc.writes, ['汉字👩‍💻', bytes, '\x1b[1;1R']);
    assert.equal(proc.writes[1], bytes, 'the original Buffer object survives the upstream startup queue');
    host.kill();
    assert.deepEqual(exits, [{ exitCode: 7 }]);
    host.dispose();
  } finally {
    host.kill();
    host.dispose();
  }
});

test('one-owner claim/release remains authoritative across writer chunks and desktop takeover', async () => {
  const proc = subprocess('bash.exe');
  const host = new HostSession({
    sessionId: 'ownership', cols: 80, rows: 24, subprocess: proc,
    onData() {}, onExit() {}
  });
  try {
    assert.equal(host.shellState, 'unsupported', 'ordinary feature-free profile never invents a marker');
    host.setOwner({ clientId: 'phone', clientType: 'mobile' });
    await host.send('p', 'phone');
    host.setOwner({ clientId: 'desktop', clientType: 'desktop' });
    await assert.rejects(host.send('stale', 'phone'), { code: 'NOT_OWNER' });
    await host.send('汉', 'desktop');
    host.setOwner(null);
    await assert.rejects(host.send(Buffer.from([255]), 'desktop'), { code: 'NOT_OWNER' });
    assert.deepEqual(proc.writes, ['p', '汉']);
  } finally { host.kill(); host.dispose(); }
});

test('host live-query responder uses Session.write; hydration never answers and daemon view never duplicates replies', async () => {
  const proc = subprocess('bash.exe');
  const outputs = [];
  let host;
  const view = new HeadlessEmulator(80, 24, reply => host.write(reply));
  host = new HostSession({
    sessionId: 'queries', cols: 80, rows: 24, subprocess: proc,
    onData: data => outputs.push(data), onExit() {}
  });
  try {
    proc.output('\x1b[6n');
    await view.write(outputs.join(''), { forwardQueryReplies: true });
    assert.deepEqual(proc.writes, ['\x1b[1;1R']);
    await view.write(view.getSnapshot(1).ansi + '\x1b[6n');
    assert.equal(proc.writes.length, 1);
    assert.ok(!Buffer.isBuffer(proc.writes[0]));
  } finally { host.kill(); host.dispose(); view.dispose(); }
});

test('actual writer rechecks owner between UTF8-safe chunks and cannot resume a retired mobile claim', async () => {
  const proc = subprocess('bash.exe');
  const host = new HostSession({
    sessionId: 'chunks', cols: 80, rows: 24, subprocess: proc, onData() {}, onExit() {}
  });
  try {
    host.setOwner({ clientId: 'phone', clientType: 'mobile' });
    const originalWrite = proc.write;
    proc.write = data => {
      originalWrite(data);
      host.setOwner({ clientId: 'desktop', clientType: 'desktop' });
    };
    await assert.rejects(host.send('汉👩‍💻'.repeat(3000), 'phone'), { code: 'NOT_OWNER' });
    assert.equal(proc.writes.length, 1, 'no second chunk after takeover');
    assert.ok(Buffer.byteLength(proc.writes[0]) <= 16384);
    assert.ok(!proc.writes[0].includes('\ufffd'), 'no broken UTF8 chunk boundary');
    proc.write = originalWrite;
    await host.send('desktop owns this', 'desktop');
    assert.equal(proc.writes.at(-1), 'desktop owns this');
  } finally { host.kill(); host.dispose(); }
});
