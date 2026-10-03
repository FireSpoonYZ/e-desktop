import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { once } from 'node:events';
import { startHost } from '../src/server.mjs';
import { client } from './client.mjs';

test('wire authentication, pinned TLS, one-use pairing, persistence and device revocation', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'e-terminal-test-'));
  let host;
  const clients = [];
  try {
    host = await startHost({ dataDir: dir, port: 0 });
    const admin = await client(host.ready.localUrl); clients.push(admin);
    await assert.rejects(admin.request('profiles.list'), { code: 'UNAUTHORIZED' });
    await admin.request('auth', { token: host.ready.adminToken, clientId: 'desktop', clientType: 'mobile' });
    const profiles = await admin.request('profiles.list');
    assert.ok(profiles.some(p => p.id === 'bash'));
    assert.ok(profiles.some(p => p.id === 'nu'));
    const pairing = await admin.request('pairing.create');
    const descriptor = JSON.parse(pairing.descriptor);
    assert.equal(descriptor.version, 1); assert.match(descriptor.fingerprint, /^[a-f0-9]{64}$/);
    assert.ok(descriptor.code.length >= 22);
    const tls = JSON.parse(readFileSync(join(dir, 'tls.json'), 'utf8'));
    const url = 'wss://127.0.0.1:' + descriptor.port;
    await assert.rejects(client(url, { cert: tls.cert, fingerprint: '0'.repeat(64) }), /Pin mismatch/);
    const phone = await client(url, { cert: tls.cert, fingerprint: descriptor.fingerprint }); clients.push(phone);
    const paired = await phone.request('pair', { code: descriptor.code, deviceName: 'Test phone' });
    await assert.rejects(phone.request('pair', { code: descriptor.code, deviceName: 'Replay' }), { code: 'INVALID_PAIRING_CODE' });
    await phone.request('auth', { token: paired.token, clientId: 'phone', clientType: 'desktop' });
    await assert.rejects(phone.request('devices.list'), { code: 'FORBIDDEN' });
    const devices = await admin.request('devices.list');
    assert.deepEqual(devices.map(d => d.name), ['Test phone']);
    assert.ok(!readFileSync(join(dir, 'devices.json'), 'utf8').includes(paired.token));
    const closed = once(phone.ws, 'close');
    await admin.request('devices.revoke', { deviceId: devices[0].id });
    await closed;
    assert.deepEqual(await admin.request('devices.list'), []);
    const second = await admin.request('pairing.create');
    const persistent = await client(url, { cert: tls.cert, fingerprint: descriptor.fingerprint }); clients.push(persistent);
    const saved = await persistent.request('pair', { code: JSON.parse(second.descriptor).code, deviceName: 'Persistent' });
    clients.forEach(c => c.close()); await host.stop();
    host = await startHost({ dataDir: dir, port: 0 });
    const restored = await client('wss://127.0.0.1:' + host.ready.remotePort,
      { cert: tls.cert, fingerprint: descriptor.fingerprint }); clients.push(restored);
    await restored.request('auth', { token: saved.token, clientId: 'restored', clientType: 'mobile' });
    await assert.rejects(restored.request('terminal.create', { profileId: 'invalid' }), { code: 'INVALID_PARAMS' });
    await assert.rejects(restored.request('terminal.create', { profileId: 'bash', cols: 0 }), { code: 'INVALID_PARAMS' });
  } finally {
    clients.forEach(c => c.close()); await host?.stop(); rmSync(dir, { recursive: true, force: true });
  }
});
