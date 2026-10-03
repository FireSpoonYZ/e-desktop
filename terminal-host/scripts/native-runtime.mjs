import { existsSync, realpathSync } from 'node:fs';
import { delimiter, dirname, isAbsolute, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

export function runtimeEnvironment(executable) {
  return { ...process.env, PATH: dirname(executable) + delimiter + (process.env.PATH ?? ''), E_DESKTOP_NODE: executable };
}

// The existing Rust host uses E_DESKTOP_NODE/PATH, not a bundled Node.
export function selectedRuntime() {
  const executable = process.env.E_DESKTOP_NODE ?? process.execPath;
  if (!isAbsolute(executable) || !existsSync(executable)) {
    throw new Error('E_DESKTOP_NODE must name an existing absolute Node executable.');
  }
  return realpathSync(executable);
}

export function useSelectedRuntime(script) {
  const executable = selectedRuntime();
  if (executable !== realpathSync(process.execPath)) {
    const result = spawnSync(executable, [script, ...process.argv.slice(2)], {
      stdio: 'inherit', env: runtimeEnvironment(executable)
    });
    if (result.error) throw new Error('Cannot run selected Node: ' + result.error.message);
    process.exit(result.status ?? 1);
  }
  const [major, minor] = process.versions.node.split('.').map(Number);
  if (major < 22 || (major === 22 && minor < 12)) throw new Error('Terminal host requires Node >=22.12.');
  process.env.PATH = runtimeEnvironment(executable).PATH;
  return executable;
}

export function runNode(script, args = [], options = {}) {
  const result = spawnSync(process.execPath, [resolve(script), ...args], {
    stdio: 'inherit', env: runtimeEnvironment(process.execPath), ...options
  });
  if (result.error || result.status !== 0) {
    throw new Error('Native deployment command failed: ' + script + ': ' + (result.error ?? result.status) +
      '. Install native build tools, then run npm ci with E_DESKTOP_NODE set to the Node used by the host. No stock fallback is allowed.');
  }
}
