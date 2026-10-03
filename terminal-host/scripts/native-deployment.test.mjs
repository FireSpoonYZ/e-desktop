import test from 'node:test';
import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { hostRoot, provenance, sha256, verifyPatch } from './patch-node-pty.mjs';

const ptyRoot = resolve(hostRoot, 'node_modules/node-pty');
function fixture(t) {
  const root = mkdtempSync(resolve(tmpdir(), 'orca-native-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const file of [...provenance.files.map(file => file.path), 'package.json']) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    cpSync(resolve(ptyRoot, file), resolve(root, file));
  }
  return root;
}

test('exact Orca patch accepts already-patched source without rewriting it', t => {
  const root = fixture(t);
  const before = provenance.files.map(file => sha256(resolve(root, file.path)));
  assert.equal(verifyPatch(root, true), 'patched');
  assert.deepEqual(provenance.files.map(file => sha256(resolve(root, file.path))), before);
});

test('exact Orca patch applies to pristine 1.1.0 source and is idempotent', t => {
  const root = fixture(t);
  const patch = readFileSync(resolve(hostRoot, 'orca-patches/node-pty@1.1.0.patch'), 'utf8').replace(/\r\n/g, '\n');
  const reverse = spawnSync('git', ['-c', 'core.autocrlf=false', 'apply', '--reverse', '-'], {
    cwd: root, input: patch, encoding: 'utf8'
  });
  assert.equal(reverse.status, 0, reverse.stderr);
  for (const file of provenance.files) assert.equal(sha256(resolve(root, file.path)), file.stockSHA256);
  assert.throws(() => verifyPatch(root), /exact patched file hashes/);
  assert.equal(verifyPatch(root, true), 'applied');
  assert.equal(verifyPatch(root, true), 'patched');
});

test('modified source and version mismatch fail with actionable errors', t => {
  const root = fixture(t);
  const source = resolve(root, provenance.files[0].path);
  writeFileSync(source, readFileSync(source, 'utf8') + '\n');
  assert.throws(() => verifyPatch(root, true), /npm ci.*Stock prebuilds/);
  writeFileSync(resolve(root, 'package.json'), '{"version":"1.2.0"}');
  assert.throws(() => verifyPatch(root, true), /Expected node-pty 1.1.0/);
});

test('deployment gate loads actual native build and exact patched exports', () => {
  const result = spawnSync(process.execPath, [resolve(hostRoot, 'scripts/check-native-deployment.mjs')], {
    encoding: 'utf8', env: process.env
  });
  assert.equal(result.status, 0, result.stderr);
  const report = JSON.parse(result.stdout);
  assert.equal(report.loadedDir, '../build/Release/');
  if (process.platform === 'win32') {
    assert.ok(report.exports.includes('terminateJob'));
    assert.ok(report.exports.includes('listJobProcessIds'));
    assert.ok(report.exports.includes('assignCurrentProcessToJob'));
    assert.equal(Object.keys(report.files).length, 3);
  }
});

test('deployment gate rejects a wrong ABI and a stock fallback candidate', t => {
  const root = mkdtempSync(resolve(tmpdir(), 'orca-deployment-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  cpSync(resolve(hostRoot, 'scripts'), resolve(root, 'scripts'), { recursive: true });
  cpSync(resolve(hostRoot, 'orca-patches'), resolve(root, 'orca-patches'), { recursive: true });
  const target = resolve(root, 'node_modules/node-pty');
  mkdirSync(target, { recursive: true });
  for (const file of [...provenance.files.map(file => file.path), 'package.json']) {
    mkdirSync(dirname(resolve(target, file)), { recursive: true });
    cpSync(resolve(ptyRoot, file), resolve(target, file));
  }
  const recordPath = resolve(target, 'build/Release/orca-native-build.json');
  mkdirSync(dirname(recordPath), { recursive: true });
  const record = JSON.parse(readFileSync(resolve(ptyRoot, 'build/Release/orca-native-build.json')));
  writeFileSync(recordPath, JSON.stringify({ ...record, abi: 'not-the-runtime-abi' }));
  const run = () => spawnSync(process.execPath, [resolve(root, 'scripts/check-native-deployment.mjs')], { encoding: 'utf8' });
  let result = run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /native build mismatch: abi/);
  mkdirSync(resolve(target, 'prebuilds'));
  result = run();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /stock prebuilds must be absent/);
});

test('deployment gate rejects invalid addon bytes even with a matching recorded hash', { skip: process.platform !== 'win32' }, t => {
  const root = mkdtempSync(resolve(tmpdir(), 'orca-exports-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  cpSync(resolve(hostRoot, 'scripts'), resolve(root, 'scripts'), { recursive: true });
  cpSync(resolve(hostRoot, 'orca-patches'), resolve(root, 'orca-patches'), { recursive: true });
  const target = resolve(root, 'node_modules/node-pty');
  cpSync(ptyRoot, target, { recursive: true });
  const addon = resolve(target, 'build/Release/conpty.node');
  // Missing/stock addons cannot pass even with a self-consistent binary hash.
  writeFileSync(addon, 'not a native addon');
  const recordPath = resolve(target, 'build/Release/orca-native-build.json');
  const record = JSON.parse(readFileSync(recordPath));
  record.files['conpty.node'] = sha256(addon);
  writeFileSync(recordPath, JSON.stringify(record));
  const result = spawnSync(process.execPath, [resolve(root, 'scripts/check-native-deployment.mjs')], { encoding: 'utf8' });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Patched node-pty deployment check failed/);
});
