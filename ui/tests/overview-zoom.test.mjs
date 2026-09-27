import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

function load(relative) {
  const source = readFileSync(new URL(relative, import.meta.url), 'utf8');
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 },
  });
  const compiled = outputText.replace(/(['"])(react(?:\/jsx-runtime)?)\1/g,
    (_, quote, name) => `${quote}${import.meta.resolve(name)}${quote}`);
  return import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`);
}

const { emptySnapshot } = await load('../src/model.ts');
const { easeOut, lerpRect, desktopRect, clientArea, clipRect, zoomPlan, zoomSlots } = await load('../src/overview/zoom.ts');

const slot = (windowId, rect, clip = rect) => ({ windowId, rect, clip });

test('empty snapshot keeps the logical gap and does not animate before a real snapshot', () => {
  assert.equal(emptySnapshot.gaps, 0);
  assert.equal(emptySnapshot.animationDurationMs, 0);
});

test('ease-out cubic matches the Rust curve and clamps', () => {
  assert.equal(easeOut(0), 0);
  assert.equal(easeOut(1), 1);
  assert.equal(easeOut(0.5), 0.875);
  assert.equal(easeOut(-2), 0);
  assert.equal(easeOut(3), 1);
  assert.equal(easeOut(Number.NaN), 1);
});

test('lerp keeps endpoints exact and rounds half away from zero', () => {
  const from = { x: 0, y: -10, width: 100, height: 40 };
  const to = { x: 10, y: 10, width: 20, height: 80 };
  assert.deepEqual(lerpRect(from, to, 0), from);
  assert.deepEqual(lerpRect(from, to, -1), from);
  assert.deepEqual(lerpRect(from, to, 1), to);
  assert.deepEqual(lerpRect(from, to, 4), to);
  assert.deepEqual(lerpRect(from, to, 0.5), { x: 9, y: 8, width: 30, height: 75 });
  assert.equal(lerpRect({ x: 0, y: 0, width: 1, height: 1 }, { x: 1, y: 0, width: 1, height: 1 }, 0.05).x, 0);
  assert.equal(lerpRect({ x: 0, y: 0, width: 1, height: 1 }, { x: -1, y: 0, width: 1, height: 1 }, 0.5).x, -1);
});

test('desktop origin, client clip and active-page plans', () => {
  const native = { x: 110, y: 90, width: 300, height: 80 };
  const origin = { x: 100, y: 80 };
  const desktop = { x: 10, y: 10, width: 300, height: 80 };
  assert.deepEqual(desktopRect(native, origin), desktop);
  assert.deepEqual(desktopRect({ x: 110, y: 220, width: 300, height: 400 }, { x: -50, y: 0 }), { x: 160, y: 220, width: 300, height: 400 });
  const client = clientArea({ width: 200, height: 100 });
  assert.deepEqual(clipRect({ x: -20, y: 10, width: 50, height: 40 }, client), { x: 0, y: 10, width: 30, height: 40 });
  assert.equal(clipRect({ x: -30, y: 0, width: 10, height: 10 }, client), null);
  assert.equal(clipRect({ x: 200, y: 0, width: 10, height: 10 }, client), null);
  assert.equal(clipRect({ x: Number.NaN, y: 0, width: 10, height: 10 }, client), null);

  const active = slot('a', { x: 12, y: 24, width: 30, height: 40 }, { x: 13, y: 25, width: 8, height: 9 });
  const other = slot('b', { x: 1, y: 2, width: 3, height: 4 });
  const natives = new Map([['a', native], ['b', { x: 1, y: 2, width: 3, height: 4 }]]);
  const open = zoomPlan([active, other], natives, new Set(['a']), origin, false);
  assert.deepEqual(open.moves, [{ windowId: 'a', from: desktop, to: active.rect }]);
  assert.deepEqual(open.rest, [other]);
  const shown = new Map([['a', { x: 4, y: 5, width: 6, height: 7 }]]);
  const closing = zoomPlan([active, other], natives, new Set(['a']), origin, true, shown);
  assert.deepEqual(closing.moves, [{ windowId: 'a', from: shown.get('a'), to: desktop }]);
  assert.deepEqual(closing.rest, []);
  assert.deepEqual(zoomPlan([other], natives, new Set(['a']), origin, false).moves, []);
  assert.deepEqual(zoomPlan([active], new Map([['a', { x: 0, y: 0, width: 0, height: 10 }]]), new Set(['a']), origin, false).rest, [active]);

  const frame = zoomSlots(open.moves, 0, client, open.rest);
  assert.deepEqual(frame[0], { windowId: 'a', rect: desktop, clip: { x: 10, y: 10, width: 190, height: 80 } });
  assert.deepEqual(frame[1], other);
  assert.deepEqual(zoomSlots(open.moves, 1, client, []), [{ windowId: 'a', rect: active.rect, clip: active.rect }]);
  assert.equal(zoomSlots([{ windowId: 'a', from: { x: -40, y: 0, width: 10, height: 10 }, to: { x: -40, y: 0, width: 10, height: 10 } }], 1, client, []).length, 0);
  const partial = zoomSlots([{ windowId: 'a', from: { x: -20, y: 0, width: 100, height: 40 }, to: { x: -20, y: 0, width: 100, height: 40 } }], 0, client, []);
  assert.deepEqual(partial, [{ windowId: 'a', rect: { x: -20, y: 0, width: 100, height: 40 }, clip: { x: 0, y: 0, width: 80, height: 40 } }]);
});
