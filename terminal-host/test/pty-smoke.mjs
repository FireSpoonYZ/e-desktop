import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startHost } from '../src/server.mjs';
import { client, until } from './client.mjs';

const dir = mkdtempSync(join(tmpdir(), 'e-terminal-pty-'));
let host, owner, observer;
try {
  host = await startHost({ dataDir: dir, port: 0 });
  owner = await client(host.ready.localUrl); observer = await client(host.ready.localUrl);
  await owner.request('auth', { token: host.ready.adminToken, clientId: 'owner', clientType: 'desktop' });
  await observer.request('auth', { token: host.ready.adminToken, clientId: 'observer', clientType: 'desktop' });
  const profiles = (await owner.request('profiles.list')).filter(p => p.available);
  assert.ok(profiles.length, 'At least one requested shell must be installed');
  for (const profile of profiles) {
    const s = await owner.request('terminal.create', { profileId: profile.id, cols: 80, rows: 24 });
    const params = { sessionId: s.id };
    const { snapshot } = await owner.request('terminal.subscribe', params);
    await observer.request('terminal.subscribe', params);
    await assert.rejects(observer.request('terminal.send', { ...params, data: 'bad' }), { code: 'NOT_OWNER' });
    await owner.request('terminal.claim', params);
    const command = profile.id === 'bash' ? "printf 'E_TERMINAL_%s\\n' OK\r"
      : profile.id === 'nu' ? "print ('E_TERMINAL_' + 'OK')\r"
      : "Write-Output ('E_TERMINAL_' + 'OK')\r";
    // Wait for shell startup / ConPTY initial terminal negotiation before typing.
    await until(() => owner.messages.some(m => m.event === 'terminal.output' && m.sessionId === s.id));
    await new Promise(resolve => setTimeout(resolve, 500));
    await assert.rejects(observer.request('terminal.send', { ...params, data: '\\xff', encoding: 'binary' }), { code: 'NOT_OWNER' });
    await assert.rejects(owner.request('terminal.send', { ...params, data: '汉', encoding: 'binary' }), { code: 'INVALID_PARAMS' });
    await owner.request('terminal.send', { ...params, data: command, encoding: 'binary' });
    const outputs = () => owner.messages.filter(m => m.event === 'terminal.output' && m.sessionId === s.id);
    await until(() => outputs().map(m => m.data).join('').includes('E_TERMINAL_OK'));
    const sequence = outputs().map(m => m.seq);
    assert.equal(sequence[0], snapshot.seq + 1);
    for (let i = 1; i < sequence.length; i++) assert.equal(sequence[i], sequence[i - 1] + 1);
    await observer.request('terminal.claim', { ...params, cols: 100, rows: 30 });
    await assert.rejects(owner.request('terminal.send', { ...params, data: 'bad' }), { code: 'NOT_OWNER' });
    await assert.rejects(owner.request('terminal.updateViewport', { ...params, cols: 80, rows: 24 }), { code: 'NOT_OWNER' });
    await observer.request('terminal.updateViewport', { ...params, cols: 90, rows: 28 });
    assert.ok(owner.messages.some(m => m.event === 'terminal.snapshot' && m.snapshot.cols === 90));
    const recovered = await owner.request('terminal.subscribe', params);
    assert.equal(recovered.snapshot.cols, 90);
    assert.equal(recovered.session.ownerClientId, 'observer');
    await observer.request('terminal.unsubscribe', params);
    observer.close();
    await until(() => owner.messages.some(m => m.event === 'terminal.control' && m.sessionId === s.id && m.ownerClientId === null));
    assert.equal((await owner.request('terminal.list')).find(item => item.id === s.id).status, 'running');
    await owner.request('terminal.close', params);
    assert.ok(!(await owner.request('terminal.list')).some(item => item.id === s.id));
    observer = await client(host.ready.localUrl);
    await observer.request('auth', { token: host.ready.adminToken, clientId: 'observer', clientType: 'desktop' });
    console.log('PASS real PTY:', profile.id, 'output/sequence/ownership/resize/disconnect/close');
  }
} finally {
  owner?.close(); observer?.close(); await host?.stop(); rmSync(dir, { recursive: true, force: true });
}
