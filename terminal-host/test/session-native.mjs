// Standalone truthful Windows checks. Exit 1 means any scenario failed.
// No startup padding, forced wrapper/preflight, replay, input retry or disabled resize.
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startHost } from '../src/server.mjs';
import { HeadlessEmulator } from '../dist/emulator.mjs';
import { client, until } from './client.mjs';

const mode = process.argv[2] ?? 'shells';
const outputPath = process.argv[3];
const results = [];
const profiles = mode === 'shells' ? ['powershell', 'bash', 'nu'] : ['bash'];
for (const profileId of profiles) {
  const dir = mkdtempSync(join(tmpdir(), 'e-session-native-'));
  const result = { profileId, mode, rounds: 0, passed: false, cleanup: false, output: '' };
  let host, connection, render, s, params, renderWork = Promise.resolve();
  try {
    host = await startHost({ dataDir: join(dir, 'data'), port: 0, address: '127.0.0.1' });
    connection = await client(host.ready.localUrl);
    await connection.request('auth', { token: host.ready.adminToken, clientId: 'native', clientType: 'mobile' });
    s = await connection.request('terminal.create', { profileId, cwd: dir, cols: 80, rows: 24 });
    params = { sessionId: s.id };
    const subscription = await connection.request('terminal.subscribe', { ...params, subscriptionId: 'first' });
    render = new HeadlessEmulator(80, 24);
    await render.write(subscription.snapshot.ansi); // No reply authority for observation/hydration.
    let parsed = subscription.snapshot.seq;
    async function update() {
      for (const item of connection.messages) {
        if (item.event !== 'terminal.output' || item.sessionId !== s.id || item.seq <= parsed) continue;
        parsed = item.seq;
        result.output += item.data;
        renderWork = renderWork.then(() => render.write(item.data));
      }
      await renderWork;
    }
    const visible = () => {
      const buffer = render.terminal.buffer.active;
      return Array.from({ length: buffer.length }, (_, n) => buffer.getLine(n)?.translateToString(true) ?? '').join('\n');
    };
    async function wait(check, timeout = 15000) {
      const deadline = Date.now() + timeout;
      while (true) {
        await update();
        if (check()) return;
        if (Date.now() >= deadline) throw new Error('Native observation deadline');
        // Polling the condition, not delay before input; same event helper as host tests.
        await until(() => connection.messages.some(item =>
          item.event === 'terminal.output' && item.sessionId === s.id && item.seq > parsed), Math.max(1, deadline - Date.now()));
      }
    }
    await connection.request('terminal.claim', params);
    const promptVisible = () => {
      const buffer = render.terminal.buffer.active;
      const line = buffer.getLine(buffer.baseY + buffer.cursorY);
      return /[$#>❯〉]\s*$/.test(line?.translateToString(true, 0, buffer.cursorX) ?? '');
    };
    await wait(promptVisible);
    const outputCommand = (marker, value) => profileId === 'bash'
      ? "printf '" + marker + "_%s\\n' '" + value + "'\r"
      : profileId === 'nu' ? "print ('" + marker + "_' + '" + value + "')\r"
      : "Write-Output ('" + marker + "_' + '" + value + "')\r";

    if (mode !== 'shells') {
      for (let round = 0; round < 20; round++) {
        const cols = round % 2 ? 80 : 45;
        await connection.request('terminal.claim', { ...params, cols, rows: 33 });
        const geometry = connection.request('terminal.updateViewport', { ...params, cols: cols - 1, rows: 32 });
        const command = "printf 'FIRST_%s_OK\\n' " + round;
        if (mode === 'fragmented') {
          await connection.request('terminal.send', { ...params, data: command[0] });
          await connection.request('terminal.send', { ...params, data: command.slice(1) });
        } else {
          await connection.request('terminal.send', { ...params, data: command });
        }
        await connection.request('terminal.send', { ...params, data: '\r' });
        await geometry;
        await wait(() => visible().includes('FIRST_' + round + '_OK')
          || visible().includes('bash: rintf: command not found'), 8000);
        assert.ok(visible().includes('FIRST_' + round + '_OK'), 'first character retained round ' + round);
        result.rounds++;
      }
    } else {
      const unicode = 'hé汉👩‍💻';
      await connection.request('terminal.send', { ...params, data: outputCommand('UNICODE', unicode) });
      await wait(() => visible().includes('UNICODE_hé'));
      result.unicode = visible().includes('UNICODE_' + unicode);
      if (!result.unicode) result.unicodeError = 'Native text input lost Unicode scalar values';
      await connection.request('terminal.send', {
        ...params, data: Buffer.from(outputCommand('BINARY', unicode)).toString('latin1'), encoding: 'binary'
      });
      await wait(() => visible().includes('BINARY_hé'));
      result.binaryUtf8Command = visible().includes('BINARY_' + unicode);
      if (!result.binaryUtf8Command) result.binaryError = 'Native binary UTF8 command lost Unicode scalar values';

      const script = "process.stdin.setRawMode(true);process.stdin.resume();let data='';process.stdin.on('data',b=>{data+=b.toString();if(/\\x1b\\[[0-9]+;[0-9]+R/.test(data)){console.log('QUERY_OK');process.exit(0)}});process.stdout.write('\\x1b[6n');setTimeout(()=>process.exit(2),3000)";
      const scriptPath = join(dir, 'live-query.cjs').replaceAll('\\', '/');
      writeFileSync(scriptPath, script);
      const executable = process.execPath.replaceAll('\\', '/');
      const prefix = profileId === 'powershell' ? '& ' : profileId === 'nu' ? '^' : '';
      await connection.request('terminal.send', { ...params, data: prefix + "'" + executable + "' '" + scriptPath + "'\r" });
      await wait(() => visible().includes('QUERY_OK'), 8000);
      result.query = true;

      connection.close();
      connection = await client(host.ready.localUrl);
      await connection.request('auth', { token: host.ready.adminToken, clientId: 'native', clientType: 'mobile' });
      const recovered = await connection.request('terminal.subscribe', { ...params, subscriptionId: 'replacement' });
      assert.ok(recovered.snapshot.ansi.includes('QUERY_OK'));
      assert.equal(recovered.session.ownerClientId, null);
      await connection.request('terminal.unsubscribe', { ...params, subscriptionId: 'first' });
      await connection.request('terminal.claim', { ...params, cols: 91, rows: 29 });
      const geometry = await connection.request('terminal.subscribe', params);
      assert.equal(geometry.snapshot.cols, 91);
      result.reconnect = true;
      await connection.request('terminal.send', { ...params, data: 'exit\r' });
      await until(() => connection.messages.some(item => item.event === 'terminal.exit' && item.sessionId === s.id));
      result.naturalExit = true;
    }
    result.passed = result.unicode !== false && result.binaryUtf8Command !== false;
  } catch (error) {
    result.error = error.message;
  } finally {
    try {
      if (params && connection) await connection.request('terminal.close', params);
      connection?.close();
      await host?.stop();
      result.cleanup = true;
    } catch (error) { result.cleanupError = error.message; result.passed = false; }
    render?.dispose();
    if (result.cleanup) rmSync(dir, { recursive: true, force: true });
    else result.retainedDataDir = dir;
    results.push(result);
    console.log(JSON.stringify({ ...result, output: undefined }));
  }
}
if (outputPath) writeFileSync(outputPath, JSON.stringify({
  mode, runtime: { node: process.version, abi: process.versions.modules, platform: process.platform }, results
}, null, 2));
if (results.some(result => !result.passed)) process.exitCode = 1;
