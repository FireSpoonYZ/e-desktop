import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { basename, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { runtimeEnvironment, useSelectedRuntime } from '../terminal-host/scripts/native-runtime.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
function run(command, args, cwd = root, env = process.env) {
  const result = spawnSync(command, args, { cwd, env, stdio: 'inherit', shell: process.platform === 'win32' && command === 'npm' });
  if (result.error || result.status !== 0) throw new Error(`${command} failed: ${result.error ?? result.status}`);
}
// Exact Orca upstream ESM bundle patch, not a hand-port of CompositionHelper.
const patch = resolve(root, 'ui/src/terminal/orca/xterm.patch');
const args = ['apply', '--directory=node_modules/@xterm/xterm', '-'];
const input = readFileSync(patch, 'utf8').replace(/\r\n/g, '\n');
const applied = spawnSync('git', [...args, '--reverse', '--check'], { cwd: root, input, stdio: ['pipe', 'ignore', 'ignore'] });
if (applied.status !== 0) {
  const result = spawnSync('git', args, { cwd: root, input, stdio: ['pipe', 'inherit', 'inherit'] });
  if (result.error || result.status !== 0) throw new Error('Cannot apply exact Orca xterm patch. Install Git and use the pinned xterm version.');
}
if (!process.argv.includes('--patch-only')) {
  const source = resolve(root, 'terminal-host');
  if (!existsSync(resolve(source, 'src/server.mjs'))) throw new Error('terminal-host/src/server.mjs missing. Integrate the terminal-host lane before running Tauri.');
  // No Node is bundled: match the existing Rust E_DESKTOP_NODE/PATH contract.
  // Re-execution and PATH make npm, node-gyp and its Node subprocesses use this runtime.
  const executable = useSelectedRuntime(fileURLToPath(import.meta.url));
  const env = runtimeEnvironment(executable);
  if (!existsSync(resolve(source, 'package-lock.json'))) throw new Error('terminal-host/package-lock.json is required for reproducible native deployment.');
  run('npm', ['ci'], source, env);
  run('npm', ['run', 'build'], source, env);
  run(executable, [resolve(source, 'scripts/check-native-deployment.mjs')], source, env);
  const destination = resolve(root, 'src-tauri/terminal-host');
  mkdirSync(destination, { recursive: true });
  // Clear stale generated assets while retaining the tracked staging notes.
  for (const entry of readdirSync(destination)) {
    if (!['.gitignore', 'README.md'].includes(entry)) rmSync(resolve(destination, entry), { recursive: true, force: true });
  }
  // Native node-pty binaries and every host-generated asset travel with the resource.
  cpSync(source, destination, { recursive: true, dereference: true, filter: path => !['.git', '.pi', '.gitignore', 'README.md'].includes(basename(path)) });
  // Resolve and load from the copied resource, not the development dependency tree.
  run(executable, [resolve(destination, 'scripts/check-native-deployment.mjs')], destination, env);
}
