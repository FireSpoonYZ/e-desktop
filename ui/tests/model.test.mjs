import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(new URL('../src/model.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 },
});
const { emptySnapshot } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);

test('initial state is paused and advertises no native support', () => {
  assert.equal(emptySnapshot.enabled, false);
  assert.equal(emptySnapshot.backend.availability, 'notImplemented');
  assert.ok(Object.values(emptySnapshot.backend.capabilities).every((capability) => capability === false));
  assert.deepEqual(emptySnapshot.windows, []);
  assert.deepEqual(emptySnapshot.monitors, []);
  assert.equal(emptySnapshot.focusedWindow, null);
});
