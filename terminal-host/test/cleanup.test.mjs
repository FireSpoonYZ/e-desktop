import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, existsSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startHost, pairingAddresses } from '../src/server.mjs';
import { processTable, terminateSession, observeOwnership, sameProcess, canSignalProcess, signalPosixTargets } from '../dist/termination.mjs';
import { client, until } from './client.mjs';

test('pairing candidates match the actual listening address family and interface', () => {
  const interfaces = { test: [
    { address: '10.0.0.2', internal: false },
    { address: 'fd12::2', internal: false },
    { address: 'fe80::2', internal: false },
    { address: '127.0.0.1', internal: true }
  ] };
  assert.deepEqual(pairingAddresses('0.0.0.0', interfaces), ['10.0.0.2']);
  assert.deepEqual(pairingAddresses('::', interfaces), ['fd12::2']);
  assert.deepEqual(pairingAddresses('10.0.0.2', interfaces), ['10.0.0.2']);
  assert.deepEqual(pairingAddresses('127.0.0.1', interfaces), ['127.0.0.1']);
  assert.deepEqual(pairingAddresses('::1', interfaces), ['::1']);
});

test('cleanup failure retains ownership evidence instead of declaring exit', async () => {
  const s = { pty: { pid: process.pid }, status: 'running' };
  await assert.rejects(terminateSession(s), { code: 'CLEANUP_FAILED' });
  assert.equal(s.status, 'running');
  assert.equal(s.exitResult, undefined);
});

async function cleanupRegression(platform, rootExitsFirst = false) {
  for (const operation of ['close', 'shutdown']) {
    const dir = mkdtempSync(join(tmpdir(), 'e-terminal-cleanup-'));
    let host, connection;
    try {
      host = await startHost({ dataDir: join(dir, 'data'), port: 0, address: '127.0.0.1' });
      connection = await client(host.ready.localUrl);
      await connection.request('auth', { token: host.ready.adminToken, clientId: 'cleanup', clientType: 'desktop' });
      const pairing = JSON.parse((await connection.request('pairing.create')).descriptor);
      assert.deepEqual(pairing.addresses, ['127.0.0.1']);
      const s = await connection.request('terminal.create', {
        profileId: platform === 'win32' ? 'powershell' : 'bash', cwd: dir
      });
      const params = { sessionId: s.id };
      await connection.request('terminal.subscribe', params);
      await connection.request('terminal.claim', params);
      await until(() => connection.messages.some(m => m.event === 'terminal.output'));
      await new Promise(resolve => setTimeout(resolve, 500));
      let command = platform === 'win32'
        ? "$child = Start-Process -FilePath 'powershell.exe' -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 120' -PassThru; $PID | Set-Content root.pid; $child.Id | Set-Content child.pid\r"
        : "trap '' HUP; trap 'exit 37' TERM; echo $$ > root.pid; bash -c 'trap \"\" HUP TERM; echo $$ > child.pid; while :; do sleep 1; done' &\r";
      if (rootExitsFirst) command = command.slice(0, -1) + (platform === 'win32'
        ? '; exit 23\r'
        : " while [ ! -s child.pid ]; do sleep 0.02; done; exit 23\r");
      await connection.request('terminal.send', { ...params, data: command });
      await until(() => existsSync(join(dir, 'root.pid')) && existsSync(join(dir, 'child.pid')));
      const rootPid = Number(readFileSync(join(dir, 'root.pid'), 'utf8').trim());
      const childPid = Number(readFileSync(join(dir, 'child.pid'), 'utf8').trim());
      assert.ok(rootPid > 0 && childPid > 0);
      if (rootExitsFirst) {
        await until(() => connection.messages.some(m => m.event === 'terminal.exit' && m.sessionId === s.id));
      }
      const before = await processTable();
      assert.equal(before.some(p => p.pid === rootPid), !rootExitsFirst);
      assert.ok(before.some(p => p.pid === childPid));
      if (operation === 'close') {
        // This request also guards against waiting on an exit event queued behind itself.
        await connection.request('terminal.close', params);
        assert.ok(!(await connection.request('terminal.list')).some(item => item.id === s.id));
        const exit = connection.messages.find(m => m.event === 'terminal.exit' && m.sessionId === s.id);
        assert.ok(exit);
        assert.equal(exit.exitCode, rootExitsFirst ? 23 : platform === 'win32' ? 1 : 37);
      } else {
        await host.stop();
      }
      const after = await processTable();
      assert.ok(!after.some(p => p.pid === rootPid), 'shell exited before success');
      assert.ok(!after.some(p => p.pid === childPid), 'descendant exited before success');
    } finally {
      connection?.close();
      await host?.stop();
      rmSync(dir, { recursive: true, force: true });
    }
  }
}

test('real Unix ignoring-HUP shell and resistant descendant are removed by close and shutdown',
  { skip: process.platform === 'win32', timeout: 30000 }, () => cleanupRegression('unix'));
test('real Windows PTY shell and descendant are removed by close and shutdown',
  { skip: process.platform !== 'win32', timeout: 30000 }, () => cleanupRegression('win32'));

test('known descendant identity survives pgid changes; recycled PIDs do not match', () => {
  const root = { pid: 101, ppid: process.pid, pgid: 101, sid: '101', startedAt: '2026-01-01T00:00:00Z' };
  const child = { pid: 102, ppid: 101, pgid: 101, sid: '101', startedAt: '2026-01-01T00:00:01Z' };
  const s = { pty: { pid: 101 } };
  observeOwnership(s, [root, child], 'linux');
  const changed = { ...child, ppid: 1, pgid: 102, sid: '102' };
  observeOwnership(s, [changed], 'linux');
  assert.ok(s.cleanupTargets.some(p => sameProcess(p, changed)));
  assert.ok(!sameProcess(child, { ...changed, startedAt: '2026-01-02T00:00:00Z' }));
});

test('lifecycle ownership discovers reparented Unix session members and filters stale Windows PPIDs', () => {
  const root = { pid: 101, ppid: process.pid, pgid: 101, sid: '101', startedAt: '2026-01-01T00:00:00Z' };
  const s = { pty: { pid: 101 } };
  observeOwnership(s, [root], 'linux');
  const orphan = { pid: 102, ppid: 1, pgid: 102, sid: '101', startedAt: '2026-01-01T00:00:01Z' };
  observeOwnership(s, [orphan], 'linux');
  assert.ok(s.cleanupTargets.some(p => p.pid === 102));
  const windows = { pty: { pid: 101 }, rootExitedAt: Date.parse('2026-01-01T00:00:03Z') };
  observeOwnership(windows, [root,
    { ...orphan, pid: 105, ppid: 101, startedAt: '2025-01-01T00:00:00Z' }
  ], 'win32');
  assert.deepEqual(windows.cleanupTargets.map(p => p.pid), [101]);
  observeOwnership(windows, [
    { ...orphan, ppid: 101 },
    { ...orphan, pid: 103, ppid: 101, startedAt: '2025-01-01T00:00:00Z' },
    { ...orphan, pid: 104, ppid: 101, startedAt: '2026-01-02T00:00:00Z' }
  ], 'win32');
  assert.deepEqual(windows.cleanupTargets.map(p => p.pid), [101, 102]);
  assert.throws(() => observeOwnership({ pty: { pid: 999 }, exitResult: { exitCode: 0 } }, [], 'win32'), /ownership/);
});

test('real Windows root exits first; surviving child is removed by close and shutdown',
  { skip: process.platform !== 'win32', timeout: 30000 }, () => cleanupRegression('win32', true));
test('real Unix root exits first; surviving session child is removed by close and shutdown',
  { skip: process.platform === 'win32', timeout: 30000 }, () => cleanupRegression('unix', true));

test('real Unix captured child changes pgid after HUP and is still escalated',
  { skip: process.platform === 'win32', timeout: 20000 }, async t => {
    const { execFileSync } = await import('node:child_process');
    try { execFileSync('python3', ['--version'], { stdio: 'ignore' }); }
    catch { t.skip('python3 is required for the setpgid regression'); return; }
    const { default: pty } = await import('node-pty');
    const { captureOwnership } = await import('../dist/termination.mjs');
    const dir = mkdtempSync(join(tmpdir(), 'e-terminal-pgid-'));
    const script = `import os, signal, sys, time
folder = sys.argv[1]
signal.signal(signal.SIGHUP, signal.SIG_IGN)
child = os.fork()
if child == 0:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    def regroup(sig, frame):
        os.setpgid(0, 0)
        open(folder + '/changed', 'w').write(str(os.getpid()))
    signal.signal(signal.SIGHUP, regroup)
    open(folder + '/child.pid', 'w').write(str(os.getpid()))
while True:
    time.sleep(0.1)
`;
    const s = { pty: pty.spawn('python3', ['-c', script, dir], { cols: 80, rows: 24, env: process.env }) };
    s.pty.onData(() => {});
    s.exitPromise = new Promise(resolve => s.pty.onExit(result => {
      s.rootExitedAt = Date.now(); s.exitResult = result; resolve(result);
    }));
    try {
      await until(() => existsSync(join(dir, 'child.pid')));
      const childPid = Number(readFileSync(join(dir, 'child.pid'), 'utf8'));
      await captureOwnership(s);
      const captured = s.cleanupTargets.find(p => p.pid === childPid);
      assert.ok(captured);
      assert.notEqual(captured.pgid, childPid);
      await terminateSession(s);
      assert.equal(Number(readFileSync(join(dir, 'changed'), 'utf8')), childPid);
      assert.ok(!(await processTable()).some(p => p.pid === childPid));
    } finally {
      await terminateSession(s);
      rmSync(dir, { recursive: true, force: true });
    }
  });

test('same-second recycled POSIX pid cannot authorize any delayed signal', () => {
  const second = Date.parse('2026-01-01T00:00:10Z');
  const root = { pid: 101, ppid: process.pid, pgid: 101, sid: '101',
    startedAt: '2026-01-01T00:00:08Z', capturedAtMs: second + 200 };
  const child = { pid: 102, ppid: 101, pgid: 101, sid: '101',
    startedAt: '2026-01-01T00:00:10Z', capturedAtMs: second + 200 };
  const s = { pty: { pid: 101 } };
  observeOwnership(s, [root, child], 'linux');
  s.exitResult = { exitCode: 0 };
  // Different process, identical pid and ps lstart; not in the owned session
  // or a fresh live-root ancestry walk. A later scan alone cannot authorize it.
  const replacement = { ...child, ppid: 900, pgid: 900, sid: '900', capturedAtMs: second + 1500 };
  assert.ok(sameProcess(child, replacement));
  observeOwnership(s, [replacement], 'linux');
  const retained = s.cleanupTargets.find(p => p.pid === child.pid);
  assert.equal(retained.capturedAtMs, second + 200);
  assert.equal(canSignalProcess(retained, 'linux'), false);
  const signals = [];
  assert.throws(() => signalPosixTargets(s.cleanupTargets, 'SIGKILL', (pid, signal) => {
    signals.push([pid, signal]); return true;
  }), { code: 'CLEANUP_FAILED' });
  assert.deepEqual(signals, [], 'reject the whole batch before even signalling its unambiguous root');
});

test('fresh terminal ownership may mature ambiguous descendants; pgid changes remain harmless', () => {
  const second = Date.parse('2026-01-01T00:00:10Z');
  const root = { pid: 101, ppid: process.pid, pgid: 101, sid: '101',
    startedAt: '2026-01-01T00:00:08Z', capturedAtMs: second + 200 };
  const child = { pid: 102, ppid: 101, pgid: 101, sid: '101',
    startedAt: '2026-01-01T00:00:10Z', capturedAtMs: second + 200 };
  const s = { pty: { pid: 101 } };
  observeOwnership(s, [root, child], 'linux');
  assert.equal(canSignalProcess(s.cleanupTargets[1], 'linux'), false);
  observeOwnership(s, [{ ...root, capturedAtMs: second + 1001 },
    { ...child, pgid: 102, capturedAtMs: second + 1001 }], 'linux');
  const proven = s.cleanupTargets.find(p => p.pid === 102);
  assert.equal(canSignalProcess(proven, 'linux'), true);
  // Once captured outside the birth second, group/session changes do not undo
  // the already proven identity or demand a new ownership shortcut.
  observeOwnership(s, [{ ...child, ppid: 1, pgid: 999, sid: '999', capturedAtMs: second + 2000 }], 'linux');
  const signals = [];
  signalPosixTargets([proven], 'SIGTERM', (pid, signal) => { signals.push([pid, signal]); return true; });
  assert.deepEqual(signals, [[102, 'SIGTERM']]);
  assert.equal(canSignalProcess({ ...child, capturedAtMs: second + 999 }, 'linux'), false);
  assert.equal(canSignalProcess({ ...child, capturedAtMs: second + 1000 }, 'linux'), true);
  assert.equal(canSignalProcess(child, 'win32'), true, 'Windows identity behavior is unchanged');
});

// Seed a previously captured identity known absent in every fresh real scan.
// No PID-directed operation is permitted/needed in these loop-order tests.
function absentTreeSession(kill) {
  const root = { pid: 2147483647, ppid: process.pid, pgid: 0, startedAt: '2020-01-01T00:00:00Z' };
  return { status: 'running', pty: { pid: root.pid, kill }, rootIdentity: root, cleanupTargets: [root] };
}

test('Windows tree-empty proof releases captured handle before waiting for its exit callback',
  { skip: process.platform !== 'win32', timeout: 10000 }, async () => {
    let releases = 0;
    const s = absentTreeSession(() => { releases++; s.exitResult = { exitCode: 7 }; });
    await terminateSession(s);
    assert.equal(releases, 1);
    assert.equal(s.ptyReleased, true);
    assert.deepEqual(s.exitResult, { exitCode: 7 });
  });

test('Windows release failure or missing physical exit cannot declare cleanup success; retry evidence stays',
  { skip: process.platform !== 'win32', timeout: 10000 }, async () => {
    const failure = absentTreeSession(() => { throw new Error('native close failed'); });
    await assert.rejects(terminateSession(failure), { code: 'CLEANUP_FAILED' });
    assert.equal(failure.ptyReleased, undefined);
    assert.equal(failure.exitResult, undefined);
    assert.equal(failure.status, 'running');
    assert.equal(failure.cleanupTargets.length, 1);
    let releases = 0;
    const missingExit = absentTreeSession(() => { releases++; });
    await assert.rejects(terminateSession(missingExit), { code: 'CLEANUP_FAILED' });
    assert.equal(releases, 1);
    assert.equal(missingExit.ptyReleased, true);
    assert.equal(missingExit.exitResult, undefined);
    assert.equal(missingExit.status, 'running');
    assert.equal(missingExit.cleanupTargets.length, 1);
  });
