import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { hostRoot, provenance, sha256, verifyPatch } from './patch-node-pty.mjs';
import { useSelectedRuntime } from './native-runtime.mjs';

// Validate the runtime actually hosting the server, without re-execution.
export function checkNativeDeployment() {
  const executable = process.execPath;
  const ptyRoot = resolve(hostRoot, 'node_modules/node-pty');
  const release = resolve(ptyRoot, 'build/Release');
  try {
    verifyPatch(ptyRoot);
    assert.ok(!existsSync(resolve(ptyRoot, 'prebuilds')), 'stock prebuilds must be absent');
    assert.ok(!existsSync(resolve(ptyRoot, 'build/Debug')), 'debug fallback must be absent');
    const build = JSON.parse(readFileSync(resolve(release, 'orca-native-build.json')));
    for (const [field, expected] of Object.entries({
      node: process.version, abi: process.versions.modules, platform: process.platform, arch: process.arch,
      patchSHA256: provenance.patchSHA256, nodePtyVersion: provenance.nodePtyVersion,
      nodeAddonApiVersion: provenance.nodeAddonApiVersion, nodeGypVersion: provenance.nodeGypVersion
    })) assert.equal(build[field], expected, 'native build mismatch: ' + field);
    const requiredFiles = process.platform === 'win32'
      ? ['conpty.node', 'conpty/conpty.dll', 'conpty/OpenConsole.exe']
      : process.platform === 'darwin' ? ['pty.node', 'spawn-helper'] : ['pty.node'];
    assert.deepEqual(Object.keys(build.files).sort(), requiredFiles.sort(), 'native resource inventory');
    for (const [file, expected] of Object.entries(build.files)) assert.equal(sha256(resolve(release, file)), expected, file + ' SHA256 mismatch');
    const require = createRequire(import.meta.url);
    const name = process.platform === 'win32' ? 'conpty' : 'pty';
    // Load the compiled file itself, then prove upstream resolution selects this same module.
    const addon = require(resolve(release, name + '.node'));
    const loaded = require(resolve(ptyRoot, 'lib/utils.js')).loadNativeModule(name);
    assert.equal(loaded.module, addon, 'node-pty must resolve the compiled Release addon');
    const exports = process.platform === 'win32'
      ? ['startProcess', 'connect', 'resize', 'clear', 'kill', 'terminateJob', 'listJobProcessIds', 'assignCurrentProcessToJob']
      : ['fork', 'open', 'resize', 'process'];
    assert.deepEqual(Object.keys(addon).sort(), exports.sort(), 'exact native exports');
    for (const name of exports) assert.equal(typeof addon[name], 'function', name);
    require(resolve(ptyRoot, 'lib/index.js'));
    return { executable, node: process.version, abi: process.versions.modules,
      arch: process.arch, platform: process.platform, loadedDir: loaded.dir, exports: Object.keys(addon), files: build.files };
  } catch (error) {
    throw Object.assign(new Error('Patched node-pty deployment check failed: ' + error.message +
      '. Rebuild with npm ci in terminal-host and E_DESKTOP_NODE set to the actual host runtime. Do not use stock prebuilds.', { cause: error }), { code: 'NATIVE_DEPLOYMENT_FAILED' });
  }

}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  useSelectedRuntime(fileURLToPath(import.meta.url));
  console.log(JSON.stringify(checkNativeDeployment(), null, 2));
}
