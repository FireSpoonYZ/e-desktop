import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const hostRoot = fileURLToPath(new URL('../', import.meta.url));
export const provenance = JSON.parse(readFileSync(new URL('../orca-patches/native-provenance.json', import.meta.url)));
export const sha256 = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const patchPath = fileURLToPath(new URL('../orca-patches/node-pty@1.1.0.patch', import.meta.url));
const repair = 'Run npm ci in terminal-host and rebuild with E_DESKTOP_NODE set to the actual host Node executable; Git and native build tools are required. Stock prebuilds are not supported.';

export function verifyPatch(ptyRoot = resolve(hostRoot, 'node_modules/node-pty'), apply = false) {
  try {
    if (sha256(patchPath) !== provenance.patchSHA256) throw new Error('Orca patch SHA256 mismatch');
    for (const [file, expected] of Object.entries(provenance.licenseSHA256)) {
      if (sha256(resolve(hostRoot, 'orca-patches', file)) !== expected) throw new Error('Native source license SHA256 mismatch: ' + file);
    }
    if (JSON.parse(readFileSync(resolve(ptyRoot, 'package.json'))).version !== provenance.nodePtyVersion) {
      throw new Error('Expected node-pty ' + provenance.nodePtyVersion);
    }
    const states = provenance.files.map(file => {
      const hash = sha256(resolve(ptyRoot, file.path));
      return hash === file.patchedSHA256 ? 'patched' : hash === file.stockSHA256 ? 'stock' : 'unknown';
    });
    if (states.every(state => state === 'patched')) return 'patched';
    if (!apply || !states.every(state => state === 'stock')) {
      throw new Error('node-pty source is stock, mixed, or modified; exact patched file hashes are required');
    }
    // Git applies paths relative to the repository root, even without --index.
    const repository = spawnSync('git', ['rev-parse', '--show-toplevel'], { cwd: ptyRoot, encoding: 'utf8' });
    const cwd = repository.status === 0 ? repository.stdout.trim() : ptyRoot;
    const directory = relative(cwd, ptyRoot).replace(/\\/g, '/');
    const args = ['-c', 'core.autocrlf=false', '-c', 'core.eol=lf', 'apply', ...(directory ? ['--directory=' + directory] : []), '-'];
    const result = spawnSync('git', args, {
      cwd, input: readFileSync(patchPath, 'utf8').replace(/\r\n/g, '\n'), encoding: 'utf8'
    });
    if (result.error || result.status !== 0) throw new Error('Exact Orca patch application failed: ' + (result.error ?? result.stderr));
    verifyPatch(ptyRoot);
    return 'applied';
  } catch (error) {
    throw new Error(error.message + '. ' + repair, { cause: error });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  console.log('node-pty Orca patch: ' + verifyPatch(undefined, true));
}
