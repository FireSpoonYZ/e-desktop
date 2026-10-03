import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdtempSync, existsSync, rmSync, readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { startHost } from '../src/server.mjs';
import { client } from './client.mjs';

test('actual upstream spawn uses host-owned userdata, no inherited agent preflight, feature-free Git Bash and DLL', {
  skip: process.platform !== 'win32', timeout: 15000
}, async () => {
  const dir = mkdtempSync(join(tmpdir(), 'e-session-isolation-'));
  const foreign = join(dir, 'foreign-orca-data');
  const keys = ['ORCA_USER_DATA_PATH', 'ORCA_CODEX_LAUNCH_PREFLIGHT'];
  const saved = Object.fromEntries(keys.map(key => [key, process.env[key]]));
  const pty = createRequire(import.meta.url)('node-pty');
  const spawn = pty.spawn;
  const calls = [];
  pty.spawn = (file, args, options) => {
    const proc = spawn(file, args, options);
    calls.push({ file, args, options, proc });
    return proc;
  };
  process.env.ORCA_USER_DATA_PATH = foreign;
  process.env.ORCA_CODEX_LAUNCH_PREFLIGHT = 'echo INHERITED_PREFLIGHT_MUST_NOT_RUN';
  let host, connection;
  try {
    host = await startHost({ dataDir: join(dir, 'host'), port: 0, address: '127.0.0.1' });
    connection = await client(host.ready.localUrl);
    await connection.request('auth', { token: host.ready.adminToken, clientId: 'isolation', clientType: 'desktop' });
    const session = await connection.request('terminal.create', { profileId: 'bash', cwd: dir });
    assert.equal(calls.length, 1, 'observed original native spawn invocation');
    const call = calls[0];
    assert.deepEqual(call.args, ['-c', 'chcp.com 65001 >/dev/null 2>&1; exec "$BASH" --login -i']);
    assert.equal(call.options.useConptyDll, true);
    // Upstream explicitly selects the DLL; node-pty selects ConPTY by its real
    // supported-Windows build predicate. Preserve the original spawn body.
    assert.equal(call.proc._agent._useConpty, true, 'actual ConPTY backend, not legacy winpty');
    assert.equal(call.proc._agent._useConptyDll, true, 'actual bundled DLL backend');
    const require = createRequire(import.meta.url);
    const loaded = require('node-pty/lib/utils').loadNativeModule('conpty');
    const addonPath = resolve(require.resolve('node-pty/lib/utils'), '..', loaded.dir, 'conpty.node');
    const resourcePath = resolve(addonPath, '..', 'conpty', 'conpty.dll');
    assert.equal(typeof loaded.module.terminateJob, 'function');
    assert.equal(typeof loaded.module.listJobProcessIds, 'function');
    assert.ok(existsSync(resourcePath), 'bundled DLL exists beside the actually resolved addon');
    assert.ok(existsSync(resolve(resourcePath, '..', 'OpenConsole.exe')));
    const moduleJson = execFileSync(join(process.env.SystemRoot, 'System32/WindowsPowerShell/v1.0/powershell.exe'),
      ['-NoProfile', '-NonInteractive', '-Command',
        "(Get-Process -Id " + process.pid + ").Modules | Where-Object { $_.ModuleName -ieq 'conpty.dll' } | Select-Object ModuleName,FileName | ConvertTo-Json -Compress"],
      { encoding: 'utf8', windowsHide: true, timeout: 3000 }).trim();
    const modules = moduleJson ? [JSON.parse(moduleJson)].flat() : [];
    assert.ok(modules.some(module => module.FileName.toLowerCase() === resourcePath.toLowerCase()),
      'the expected bundled DLL is actually loaded in this Node process');
    console.log(JSON.stringify({
      backend: { useConpty: call.proc._agent._useConpty, useConptyDll: call.proc._agent._useConptyDll },
      addonPath, bundledDllPath: resourcePath, loadedConptyDlls: modules,
      addonSha256: createHash('sha256').update(readFileSync(addonPath)).digest('hex')
    }));
    assert.equal(call.options.env.CHERE_INVOKING, '1');
    assert.equal(call.options.env.TERM_PROGRAM, 'Orca');
    assert.equal(call.options.env.ORCA_CODEX_LAUNCH_PREFLIGHT, undefined);
    assert.equal(call.options.env.ORCA_USER_DATA_PATH, resolve(dir, 'host', 'orca-runtime'));
    assert.equal(process.env.ORCA_USER_DATA_PATH, foreign, 'ambient storage binding restored');
    assert.ok(!existsSync(foreign), 'no writes to inherited Orca storage');
    await connection.request('terminal.close', { sessionId: session.id });
  } finally {
    connection?.close();
    await host?.stop();
    pty.spawn = spawn;
    for (const key of keys) {
      if (saved[key] === undefined) delete process.env[key];
      else process.env[key] = saved[key];
    }
    rmSync(dir, { recursive: true, force: true });
  }
});
