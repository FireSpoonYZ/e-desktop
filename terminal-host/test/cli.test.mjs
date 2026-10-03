import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, toNamespacedPath } from 'node:path';
import { once } from 'node:events';
import { fileURLToPath } from 'node:url';
import { client, until } from './client.mjs';

test('CLI emits one exact readiness record and accepts stdin shutdown independently of cwd', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'e-terminal-cli-'));
  const child = spawn(process.execPath, [fileURLToPath(new URL('../src/server.mjs', import.meta.url)),
    '--data-dir', dir, '--port', '0'], { cwd: tmpdir(), stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '', stderr = '', connection;
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.on('data', chunk => { stderr += chunk; });
  try {
    await until(() => stdout.includes('\n'));
    const ready = JSON.parse(stdout.trim());
    assert.deepEqual(Object.keys(ready).sort(), ['adminToken', 'localUrl', 'protocolVersion', 'remotePort', 'type'].sort());
    assert.equal(ready.type, 'ready'); assert.equal(ready.protocolVersion, 1);
    assert.match(ready.localUrl, /^ws:\/\/127\.0\.0\.1:\d+$/);
    connection = await client(ready.localUrl);
    await connection.request('auth', { token: ready.adminToken, clientId: 'cli-test', clientType: 'desktop' });
    const profile = (await connection.request('profiles.list')).find(p => p.available);
    if (profile) {
      const session = await connection.request('terminal.create', { profileId: profile.id });
      await connection.request('terminal.subscribe', { sessionId: session.id });
      await until(() => connection.messages.some(m => m.event === 'terminal.output'));
    }
    const exited = once(child, 'exit');
    child.stdin.write('shutdown\n');
    const result = await Promise.race([exited, new Promise((_, reject) => {
      const timer = setTimeout(() => reject(new Error('Shutdown timed out')), 10000); timer.unref();
    })]);
    assert.equal(result[0], 0, stderr);
    assert.equal(stdout.trim().split('\n').length, 1);
    assert.ok(!stderr.includes(ready.adminToken));
  } finally { connection?.close(); child.kill(); rmSync(dir, { recursive: true, force: true }); }
});

test('CLI accepts Windows namespaced entry paths and exits on parent stdin EOF', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'e-terminal-eof-'));
  const child = spawn(process.execPath, [toNamespacedPath(fileURLToPath(new URL('../src/server.mjs', import.meta.url))),
    '--data-dir', dir, '--port', '0'], { cwd: tmpdir(), stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '';
  child.stdout.on('data', chunk => { stdout += chunk; });
  child.stderr.resume();
  try {
    await until(() => stdout.includes('\n'));
    const exited = once(child, 'exit');
    child.stdin.end();
    const result = await Promise.race([exited, new Promise((_, reject) => {
      const timer = setTimeout(() => reject(new Error('EOF shutdown timed out')), 10000); timer.unref();
    })]);
    assert.equal(result[0], 0);
    assert.equal(stdout.trim().split('\n').length, 1);
  } finally { child.kill(); rmSync(dir, { recursive: true, force: true }); }
});
