import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startHost } from '../src/server.mjs';
import { client } from './client.mjs';

test('mobile display modes preserve the desktop grid across actors, late cleanup and indefinite holds', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'e-terminal-display-'));
  const clients = [];
  let host;
  try {
    host = await startHost({ dataDir: dir, port: 0, address: '127.0.0.1' });
    for (const [clientId, clientType] of [['desktop', 'desktop'], ['phone-a', 'mobile'], ['phone-b', 'mobile']]) {
      const connection = await client(host.ready.localUrl); clients.push(connection);
      const auth = await connection.request('auth', { token: host.ready.adminToken, clientId, clientType });
      assert.equal(auth.capabilities.terminalDisplayMode, true);
    }
    const [desktop, phone, peer] = clients;
    const profile = (await desktop.request('profiles.list')).find(p => p.available);
    assert.ok(profile, 'An installed shell is required');
    const session = await desktop.request('terminal.create', { profileId: profile.id, cols: 100, rows: 30 });
    const id = { sessionId: session.id };
    const grid = async () => {
      const value = (await desktop.request('terminal.list')).find(s => s.id === session.id);
      return [value.cols, value.rows, value.displayMode, value.ownerClientId];
    };
    const subscribe = (c, subscriptionId, viewport, displayMode = 'auto') =>
      c.request('terminal.subscribe', { ...id, subscriptionId, viewport, displayMode });
    const mode = (c, subscriptionId, displayMode, viewport) =>
      c.request('terminal.displayModeSet', { ...id, subscriptionId, displayMode, ...(viewport ? { viewport } : {}) });
    await desktop.request('terminal.claim', id);
    await subscribe(phone, 'a', { cols: 40, rows: 22 });
    await subscribe(peer, 'b', { cols: 52, rows: 28 });
    assert.deepEqual(await grid(), [100, 30, 'auto', 'desktop'], 'a passive subscriber cannot resize the desktop');
    await assert.rejects(mode(phone, 'a', 'auto'), { code: 'NOT_OWNER' });

    await phone.request('terminal.claim', id);
    assert.deepEqual(await grid(), [40, 22, 'phone', 'phone-a']);
    const restored = await mode(phone, 'a', 'desktop');
    assert.deepEqual([restored.session.cols, restored.session.rows, restored.session.displayMode], [100, 30, 'desktop']);
    assert.deepEqual([restored.snapshot.cols, restored.snapshot.rows], [100, 30]);
    await phone.request('terminal.updateViewport', { ...id, subscriptionId: 'a', cols: 36, rows: 20 });
    assert.deepEqual(await grid(), [100, 30, 'desktop', 'phone-a'], 'desktop mode does not follow phone frame changes');
    await mode(phone, 'a', 'auto');
    assert.deepEqual(await grid(), [36, 20, 'phone', 'phone-a']);

    await subscribe(phone, 'a-new', { cols: 38, rows: 24 });
    await phone.request('terminal.unsubscribe', { ...id, subscriptionId: 'a' });
    await assert.rejects(mode(phone, 'a', 'desktop'), { code: 'NOT_SUBSCRIBED' });
    await assert.rejects(phone.request('terminal.updateViewport', { ...id, subscriptionId: 'a', cols: 25, rows: 12 }),
      { code: 'NOT_SUBSCRIBED' });
    for (const params of [
      { displayMode: 'phone' }, { displayMode: 'auto', viewport: null },
      { displayMode: 'auto', viewport: { cols: 0, rows: 20 } },
      { displayMode: 'auto', viewport: { cols: 40, rows: 201 } },
    ]) {
      await assert.rejects(phone.request('terminal.displayModeSet', { ...id, subscriptionId: 'a-new', ...params }),
        { code: 'INVALID_PARAMS' });
    }
    assert.deepEqual(await grid(), [38, 24, 'phone', 'phone-a'], 'invalid/stale requests do not mutate mode or baseline');
    await mode(phone, 'a-new', 'desktop');
    assert.deepEqual(await grid(), [100, 30, 'desktop', 'phone-a'], 'resubscribe never captures the fitted grid as desktop baseline');

    await mode(phone, 'a-new', 'auto', { cols: 40, rows: 22 });
    await peer.request('terminal.claim', id);
    assert.deepEqual(await grid(), [52, 28, 'phone', 'phone-b'], 'the active mobile actor supplies its own measured viewport');
    await peer.request('terminal.unsubscribe', { ...id, subscriptionId: 'b' });
    await peer.request('terminal.release', id);
    await phone.request('terminal.unsubscribe', { ...id, subscriptionId: 'a-new' });
    assert.deepEqual(await grid(), [52, 28, 'phone', null], 'Orca default is indefinite fit hold after the last subscriber leaves');

    await subscribe(phone, 'a-return', { cols: 35, rows: 19 });
    await phone.request('terminal.claim', id);
    await mode(phone, 'a-return', 'desktop');
    assert.deepEqual(await grid(), [100, 30, 'desktop', 'phone-a'], 'the original baseline survives the no-subscriber gap');
    await mode(phone, 'a-return', 'auto', { cols: 2, rows: 2 });
    assert.deepEqual(await grid(), [20, 8, 'phone', 'phone-a'], 'phone fit uses Orca minimum grid dimensions');
    await desktop.request('terminal.claim', { ...id, cols: 120, rows: 36 });
    assert.deepEqual(await grid(), [120, 36, 'desktop', 'desktop'], 'explicit desktop geometry overrides the retained baseline');
    await assert.rejects(mode(phone, 'a-return', 'auto'), { code: 'NOT_OWNER' });
    await phone.request('terminal.claim', id);
    await mode(phone, 'a-return', 'auto', { cols: 42, rows: 25 });
    await desktop.request('terminal.claim', id);
    assert.deepEqual(await grid(), [120, 36, 'desktop', 'desktop'], 'desktop takeover restores the newest authoritative desktop grid');
    assert.ok(phone.messages.some(m => m.event === 'terminal.snapshot' && m.displayMode === 'phone'));
    assert.ok(phone.messages.some(m => m.event === 'terminal.snapshot' && m.displayMode === 'desktop'));

    await desktop.request('terminal.close', id);
  } finally {
    clients.forEach(c => c.close());
    await host?.stop();
    rmSync(dir, { recursive: true, force: true });
  }
});
