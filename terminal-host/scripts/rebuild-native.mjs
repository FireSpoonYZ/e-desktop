import { readFileSync, rmSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { hostRoot, provenance, sha256, verifyPatch } from './patch-node-pty.mjs';
import { runNode, runtimeEnvironment, useSelectedRuntime } from './native-runtime.mjs';

const executable = useSelectedRuntime(fileURLToPath(import.meta.url));
const ptyRoot = resolve(hostRoot, 'node_modules/node-pty');
const release = resolve(ptyRoot, 'build/Release');
const addonApi = JSON.parse(readFileSync(resolve(hostRoot, 'node_modules/node-addon-api/package.json')));
const nodeGyp = JSON.parse(readFileSync(resolve(hostRoot, 'node_modules/node-gyp/package.json')));
if (addonApi.version !== provenance.nodeAddonApiVersion || nodeGyp.version !== provenance.nodeGypVersion) {
  throw new Error('Native build dependency versions differ from native-provenance.json; run npm ci.');
}
console.log('node-pty Orca patch: ' + verifyPatch(ptyRoot, true));
// Remove every stock/debug candidate before compiling. A failed build stays failed.
rmSync(resolve(ptyRoot, 'prebuilds'), { recursive: true, force: true });
rmSync(resolve(ptyRoot, 'build'), { recursive: true, force: true });
runNode(resolve(hostRoot, 'node_modules/node-gyp/bin/node-gyp.js'),
  ['rebuild', '--target=' + process.versions.node, '--arch=' + process.arch], { cwd: ptyRoot });
// Upstream primitive stages its shipped conpty.dll and OpenConsole beside the addon.
runNode(resolve(ptyRoot, 'scripts/post-install.js'), [], {
  cwd: ptyRoot, env: { ...runtimeEnvironment(executable), npm_config_arch: process.arch }
});
const files = process.platform === 'win32'
  ? ['conpty.node', 'conpty/conpty.dll', 'conpty/OpenConsole.exe']
  : process.platform === 'darwin' ? ['pty.node', 'spawn-helper'] : ['pty.node'];
const record = {
  executable, node: process.version, abi: process.versions.modules, arch: process.arch, platform: process.platform,
  patchSHA256: provenance.patchSHA256, nodePtyVersion: provenance.nodePtyVersion,
  nodeAddonApiVersion: addonApi.version, nodeGypVersion: nodeGyp.version,
  files: Object.fromEntries(files.map(file => [file, sha256(resolve(release, file))]))
};
writeFileSync(resolve(release, 'orca-native-build.json'), JSON.stringify(record, null, 2) + '\n');
runNode(resolve(hostRoot, 'scripts/check-native-deployment.mjs'));
