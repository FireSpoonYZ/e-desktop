import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
const root = join(dirname(fileURLToPath(import.meta.url)), '../src/orca');
const hash = bytes => createHash('sha256').update(bytes).digest('hex');

test('pinned Orca runtime import closure has only documented Node ESM binding changes, with MIT retained', () => {
  const manifest = JSON.parse(readFileSync(join(root, 'provenance.json')));
  assert.equal(manifest.commit, 'de8bffe24045b396212f4f63de8960ec8380ea07');
  const adaptations = new Map(manifest.adaptations.map(item => [item.path, item]));
  for (const entry of manifest.files) {
    let bytes = readFileSync(join(root, entry.path));
    const adaptation = adaptations.get(entry.path);
    if (adaptation) {
      assert.equal(hash(bytes), adaptation.adaptedSha256, entry.path);
      let source = bytes.toString();
      if (entry.path.endsWith('/headless-emulator.ts')) {
        source = source
          .replace("import headless from '@xterm/headless'\nconst { Terminal } = headless", "import { Terminal } from '@xterm/headless'")
          .replace("import serialize from '@xterm/addon-serialize'\nconst { SerializeAddon } = serialize", "import { SerializeAddon } from '@xterm/addon-serialize'")
          .replace("import unicode11 from '@xterm/addon-unicode11'\nconst { Unicode11Addon } = unicode11", "import { Unicode11Addon } from '@xterm/addon-unicode11'");
      } else {
        source = source.replace('createRequire(import.meta.url)', 'createRequire(__filename)');
      }
      bytes = Buffer.from(source);
    }
    assert.equal(bytes.length, entry.bytes, entry.path);
    assert.equal(hash(bytes), entry.sha256, entry.path + ': original pinned bytes');
    assert.match(entry.blob, /^[a-f0-9]{40}$/);
  }
  assert.equal(manifest.files.length, 294, '293 runtime dependency inputs plus license');
  assert.equal(adaptations.size, 4, 'imports and ESM platform bindings only');
  assert.match(readFileSync(join(root, 'LICENSE'), 'utf8'), /MIT License/);
  const walk = folder => readdirSync(folder, { withFileTypes: true }).flatMap(entry =>
    entry.isDirectory() ? walk(join(folder, entry.name)) : [join(folder, entry.name)]);
  assert.equal(walk(join(root, 'src')).length, 293, 'no unused diagnostic/test/app entry dump');
});
