// Bounded negative control: fresh Node process per variant; no input retries.
// baseline uses an external byte-identical incoming host copy. baseline-dll
// adds ONLY a documented test-boundary DLL option, matching current backend.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';

const [variant, hostRootArgument, resultPath] = process.argv.slice(2);
assert.ok(['current', 'baseline', 'baseline-dll'].includes(variant));
const hostRoot = resolve(hostRootArgument);
const require = createRequire(pathToFileURL(join(hostRoot, 'src/server.mjs')));
const pty = require('node-pty');
const native = require('node-pty/lib/utils').loadNativeModule('conpty');
const addonPath = resolve(require.resolve('node-pty/lib/utils'), '..', native.dir, 'conpty.node');
const bytes = data => ({
  type: Buffer.isBuffer(data) ? 'Buffer' : 'string',
  utf8Hex: Buffer.isBuffer(data) ? data.toString('hex') : Buffer.from(data).toString('hex'),
  ...(typeof data === 'string' ? {
    utf16: Array.from({ length: data.length }, (_, i) => data.charCodeAt(i)),
    scalars: Array.from(data, item => item.codePointAt(0))
  } : {})
});
const result = {
  variant, node: process.execPath, version: process.version, abi: process.versions.modules,
  input: 'hé汉👩‍💻', expectedHex: Buffer.from('hé汉👩‍💻').toString('hex'),
  trace: [], cases: [], cleanup: false,
  native: { addonPath, sha256: createHash('sha256').update(readFileSync(addonPath)).digest('hex'),
    bundledDllPath: resolve(addonPath, '..', 'conpty/conpty.dll') }
};
const spawn = pty.spawn;
pty.spawn = (file, args, options) => {
  const effectiveOptions = variant === 'baseline-dll'
    ? { ...options, useConptyDll: true } : options;
  const proc = spawn(file, args, effectiveOptions);
  const moduleJson = execFileSync(join(process.env.SystemRoot, 'System32/WindowsPowerShell/v1.0/powershell.exe'),
    ['-NoProfile', '-NonInteractive', '-Command',
      "(Get-Process -Id " + process.pid + ").Modules | Where-Object { $_.ModuleName -ieq 'conpty.dll' } | Select-Object ModuleName,FileName | ConvertTo-Json -Compress"],
    { encoding: 'utf8', windowsHide: true, timeout: 3000 }).trim();
  result.native.loadedConptyDlls = moduleJson ? [JSON.parse(moduleJson)].flat() : [];
  result.launch = {
    file, args, cwd: options.cwd, cols: options.cols, rows: options.rows,
    options: { useConpty: effectiveOptions.useConpty ?? null, useConptyDll: effectiveOptions.useConptyDll ?? null },
    actual: { useConpty: proc._agent._useConpty, useConptyDll: proc._agent._useConptyDll }
  };
  const write = proc.write.bind(proc);
  proc.write = data => {
    result.trace.push({ boundary: 'native-pty.write.invocation', ...bytes(data) });
    return write(data);
  };
  const inputSocket = proc._agent.inSocket;
  const socketWrite = inputSocket.write.bind(inputSocket);
  inputSocket.write = (data, ...rest) => {
    result.trace.push({ boundary: 'native-input-socket.write.invocation', ...bytes(data) });
    return socketWrite(data, ...rest);
  };
  proc.onData(data => result.trace.push({ boundary: 'native-pty.output', data, ...bytes(data) }));
  proc.onExit(event => { result.physicalExit = event; });
  return proc;
};
if (variant === 'current') {
  const { HostSession } = await import(pathToFileURL(join(hostRoot, 'dist/session.mjs')));
  const send = HostSession.prototype.send;
  HostSession.prototype.send = function(data, clientId) {
    result.trace.push({ boundary: 'facade.send', ...bytes(data) });
    if (!this.session.nuControlObserved) {
      const write = this.session.write.bind(this.session);
      this.session.write = payload => {
        result.trace.push({ boundary: 'actual-Session.write', ...bytes(payload) });
        return write(payload);
      };
      this.session.nuControlObserved = true;
    }
    return send.call(this, data, clientId);
  };
}
const { startHost } = await import(pathToFileURL(join(hostRoot, 'src/server.mjs')));
const { client, until } = await import(pathToFileURL(join(hostRoot, 'test/client.mjs')));
const { HeadlessEmulator } = await import(pathToFileURL(join(hostRoot, 'dist/emulator.mjs')));
const dir = mkdtempSync(join(tmpdir(), 'e-nu-control-'));
let host, connection, session, view;
let renderedSeq = -1, renderWork = Promise.resolve();
try {
  host = await startHost({ dataDir: join(dir, 'data'), address: '127.0.0.1', port: 0 });
  connection = await client(host.ready.localUrl);
  await connection.request('auth', { token: host.ready.adminToken, clientId: 'nu-control', clientType: 'mobile' });
  const profiles = await connection.request('profiles.list');
  result.profile = profiles.find(profile => profile.id === 'nu');
  session = await connection.request('terminal.create', { profileId: 'nu', cols: 80, rows: 24, cwd: dir });
  const params = { sessionId: session.id };
  const subscription = await connection.request('terminal.subscribe', params);
  view = new HeadlessEmulator(80, 24);
  await view.write(subscription.snapshot.ansi);
  renderedSeq = subscription.snapshot.seq;
  await connection.request('terminal.claim', params);
  const promptVisible = () => {
    const buffer = view.terminal.buffer.active;
    return /[>❯〉]\s*$/.test(buffer.getLine(buffer.baseY + buffer.cursorY)?.translateToString(true, 0, buffer.cursorX) ?? '');
  };
  async function render() {
    for (const event of connection.messages) {
      if (event.event !== 'terminal.output' || event.sessionId !== session.id || event.seq <= renderedSeq) continue;
      renderedSeq = event.seq;
      renderWork = renderWork.then(() => view.write(event.data));
    }
    await renderWork;
  }
  async function awaitPrompt() {
    while (true) {
      await render();
      if (promptVisible()) return;
      await until(() => connection.messages.some(event =>
        event.event === 'terminal.output' && event.sessionId === session.id && event.seq > renderedSeq));
    }
  }
  await awaitPrompt();
  for (const encoding of [undefined, 'binary']) {
    const caseId = encoding ?? 'text';
    const marker = 'NU_' + caseId.toUpperCase() + '=';
    const command = "print ('" + marker + "' + ('" + result.input + "' | into binary | encode hex))\r";
    const data = encoding ? Buffer.from(command).toString('latin1') : command;
    result.trace.push({ boundary: 'wire-input-intent', encoding: caseId, ...bytes(command) });
    await connection.request('terminal.send', { ...params, data, ...(encoding ? { encoding } : {}) });
    let actualHex;
    await until(() => {
      const output = result.trace.filter(entry => entry.boundary === 'native-pty.output').map(entry => entry.data).join('');
      actualHex = output.match(new RegExp(marker + '([0-9A-Fa-f]{2,})'))?.[1]?.toLowerCase();
      const wireOutput = connection.messages.filter(event =>
        event.event === 'terminal.output' && event.sessionId === session.id).map(event => event.data).join('');
      return !!actualHex && new RegExp(marker + '[0-9A-Fa-f]{2,}').test(wireOutput);
    });
    result.cases.push({
      encoding: caseId, actualShellUtf8Hex: actualHex,
      actualShellText: Buffer.from(actualHex, 'hex').toString(),
      actualShellScalars: Array.from(Buffer.from(actualHex, 'hex').toString(), item => item.codePointAt(0)),
      passed: actualHex === result.expectedHex
    });
    // Observe next real prompt before the next distinct test, no startup padding.
    await awaitPrompt();
  }
  await connection.request('terminal.send', { ...params, data: 'exit\r' });
  await until(() => result.physicalExit !== undefined);
  await connection.request('terminal.close', params);
  await host.stop();
  result.cleanup = true;
} catch (error) {
  result.error = error.message;
} finally {
  connection?.close();
  if (!result.cleanup) {
    // Existing host identity cleanup only; never kill an unknown PID/handle.
    try { await host?.stop(); result.cleanup = true; }
    catch (error) { result.cleanupError = error.message; result.retainedDataDir = dir; }
  }
  view?.dispose();
  pty.spawn = spawn;
  if (result.cleanup) rmSync(dir, { recursive: true, force: true });
  if (resultPath) writeFileSync(resultPath, JSON.stringify(result, null, 2));
}
console.log(JSON.stringify({ ...result, trace: undefined }));
if (!result.cleanup || result.error || result.cases.some(item => !item.passed)) process.exitCode = 1;
