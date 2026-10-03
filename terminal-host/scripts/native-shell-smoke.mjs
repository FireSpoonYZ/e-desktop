// Deployment smoke only: no interactive resize/first-character or Session parity claim.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { useSelectedRuntime } from './native-runtime.mjs';

useSelectedRuntime(fileURLToPath(import.meta.url));
await import('./check-native-deployment.mjs');
if (process.platform !== 'win32') throw new Error('This three-shell ConPTY deployment smoke requires Windows.');
const require = createRequire(import.meta.url);
const pty = require('node-pty');
const shells = [
  ['powershell.exe', ['-NoLogo', '-NoProfile', '-Command', "Write-Output 'ORCA_NATIVE_POWERSHELL'; Read-Host"], 'ORCA_NATIVE_POWERSHELL'],
  ['nu.exe', ['--no-config-file', '-c', "print 'ORCA_NATIVE_NU'; input"], 'ORCA_NATIVE_NU'],
  ['bash.exe', ['--noprofile', '--norc', '-c', "printf 'ORCA_NATIVE_BASH\\n'; read -r line"], 'ORCA_NATIVE_BASH']
];
for (const [shell, args, marker] of shells) {
  let terminal;
  try {
    await new Promise((resolve, reject) => {
      let output = '', closing = false, sawMarker = false;
      // Deadline bounds observed events, not a startup/input delay.
      const deadline = setTimeout(() => reject(new Error(shell + ': native output/exit deadline exceeded')), 15000);
      terminal = pty.spawn(shell, args, {
        cols: 80, rows: 24, cwd: process.cwd(), env: { ...process.env },
        encoding: 'binary', useConpty: true, useConptyDll: true
      });
      terminal.onData(data => {
        output += data;
        if (!closing && output.includes(marker)) {
          sawMarker = true;
          closing = true;
          terminal.kill();
        }
      });
      terminal.onExit(event => {
        clearTimeout(deadline);
        try {
          assert.ok(sawMarker, shell + ': expected actual shell output');
          console.log(JSON.stringify({ shell, pid: terminal.pid, marker, exitCode: event.exitCode }));
          resolve();
        } catch (error) { reject(error); }
      });
    });
  } finally { terminal?.kill(); }
}
