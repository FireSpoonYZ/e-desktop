import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { once } from 'node:events';
import { startHost } from '../src/server.mjs';
import { client } from './client.mjs';

test('same device replaces a half-open connection without transferring control or accepting another principal', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'e-terminal-reconnect-'));
  const clients = [];
  let host, old;
  try {
    host = await startHost({ dataDir: dir, port: 0 });
    const connect = async (url = host.ready.localUrl, tls) => {
      const c = await client(url, tls); clients.push(c); return c;
    };
    const admin = await connect();
    await admin.request('auth', { token: host.ready.adminToken, clientId: 'admin', clientType: 'desktop' });
    const tls = JSON.parse(readFileSync(join(dir, 'tls.json'), 'utf8'));
    const pair = async name => {
      const descriptor = JSON.parse((await admin.request('pairing.create')).descriptor);
      const c = await connect('wss://127.0.0.1:' + descriptor.port, { cert: tls.cert, fingerprint: descriptor.fingerprint });
      const { token } = await c.request('pair', { code: descriptor.code, deviceName: name });
      return { c, token, descriptor };
    };
    const phone = await pair('Phone');
    old = phone.c;
    const auth = { token: phone.token, clientId: 'phone', clientType: 'mobile' };
    await old.request('auth', auth);
    const other = await pair('Other device');
    await assert.rejects(other.c.request('auth', { ...auth, token: other.token }), { code: 'CLIENT_ID_IN_USE' });
    await assert.rejects(other.c.request('auth', { ...auth, token: 'invalid' }), { code: 'UNAUTHORIZED' });
    const adminImpersonator = await connect();
    await assert.rejects(adminImpersonator.request('auth', { ...auth, token: host.ready.adminToken }), { code: 'CLIENT_ID_IN_USE' });
    const profile = (await admin.request('profiles.list')).find(p => p.available);
    assert.ok(profile, 'An installed shell is required');
    const session = await admin.request('terminal.create', { profileId: profile.id });
    const params = { sessionId: session.id };
    await old.request('terminal.claim', params);
    // Hold the old close handshake until after the replacement claims control.
    old.ws._socket.pause();
    const replacement = await connect('wss://127.0.0.1:' + phone.descriptor.port,
      { cert: tls.cert, fingerprint: phone.descriptor.fingerprint });
    await replacement.request('auth', auth);
    assert.equal((await replacement.request('terminal.list'))[0].ownerClientId, null);
    await assert.rejects(replacement.request('terminal.send', { ...params, data: 'must not replay' }), { code: 'NOT_OWNER' });
    await replacement.request('terminal.subscribe', { ...params, subscriptionId: 'replacement' });
    await replacement.request('terminal.claim', params);
    // Even a release sent by the retired socket must not release the new owner.
    old.ws.send(JSON.stringify({ id: 'late-release', method: 'terminal.release', params }));
    const closed = once(old.ws, 'close');
    old.ws._socket.resume();
    await closed;
    await new Promise(resolve => setImmediate(resolve));
    assert.equal((await replacement.request('terminal.list'))[0].ownerClientId, 'phone');
    assert.equal((await replacement.request('terminal.list')).length, 1);
    await replacement.request('terminal.release', params);
    assert.ok(replacement.messages.some(m => m.event === 'terminal.control' && m.ownerClientId === null));
    await admin.request('terminal.close', params);
  } finally {
    old?.ws._socket?.resume();
    clients.forEach(c => c.close()); await host?.stop(); rmSync(dir, { recursive: true, force: true });
  }
});

test('subscription identities fence late cleanup, preserve legacy use and remain connection scoped', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'e-terminal-subscribe-'));
  const clients = [];
  let host;
  try {
    host = await startHost({ dataDir: dir, port: 0 });
    for (const clientId of ['owner', 'observer']) {
      const c = await client(host.ready.localUrl); clients.push(c);
      await c.request('auth', { token: host.ready.adminToken, clientId, clientType: 'desktop' });
    }
    const [owner, observer] = clients;
    const profile = (await owner.request('profiles.list')).find(p => p.available);
    assert.ok(profile, 'An installed shell is required');
    const session = await owner.request('terminal.create', { profileId: profile.id });
    const params = { sessionId: session.id };
    const old = { ...params, subscriptionId: 'old' }, current = { ...params, subscriptionId: 'current' };
    const controls = c => c.messages.filter(m => m.event === 'terminal.control').length;
    const assertDelivery = async expected => {
      const before = controls(observer);
      await owner.request('terminal.claim', params);
      await observer.request('terminal.list'); // Response fences earlier events on this socket.
      assert.equal(controls(observer) - before, expected);
    };
    await observer.request('terminal.subscribe', old);
    await observer.request('terminal.subscribe', current);
    await observer.request('terminal.unsubscribe', old);
    await assertDelivery(1);
    await observer.request('terminal.unsubscribe', params);
    await assertDelivery(1);
    await observer.request('terminal.subscribe', current); // Same ID stays one registration.
    await assertDelivery(1);
    await owner.request('terminal.subscribe', current);
    await owner.request('terminal.unsubscribe', current);
    await assertDelivery(1); // Another socket's identical ID is independent.
    for (const subscriptionId of ['', 'x'.repeat(129), 123]) {
      await assert.rejects(observer.request('terminal.subscribe', { ...params, subscriptionId }), { code: 'INVALID_PARAMS' });
      await assert.rejects(observer.request('terminal.unsubscribe', { ...params, subscriptionId }), { code: 'INVALID_PARAMS' });
    }
    await assertDelivery(1);
    await observer.request('terminal.unsubscribe', current);
    await assertDelivery(0);
    await observer.request('terminal.subscribe', params);
    await assertDelivery(1);
    await observer.request('terminal.unsubscribe', params);
    await assertDelivery(0);
    await owner.request('terminal.close', params);
  } finally {
    clients.forEach(c => c.close()); await host?.stop(); rmSync(dir, { recursive: true, force: true });
  }
});
