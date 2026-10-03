import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

// lane: ui-animation — Ctrl+wheel / pinch overview zoom keeps hit testing physical.
const source = readFileSync(new URL('../src/overview/pointer.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 },
});
const { OVERVIEW_ZOOM_MAX, OVERVIEW_ZOOM_MIN, dropCommand, overviewScale, pageGeometry, physicalWidth, wheelZoom } =
  await import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);

const rect = { x: 0, y: 0, width: 1000, height: 800 };
const snapshot = () => ({
  enabled: true, focusedWindow: 'w', activeMonitor: 'm', errors: [],
  backend: { availability: 'ready', capabilities: { placement: true, focus: true } },
  windows: [{ native: { id: 'w', rect }, floating: false, fullscreen: false }],
  monitors: [{ monitor: { id: 'm', name: 'Monitor', scaleFactor: 1.5 }, viewport: { x: -1000, y: 40, width: 1000, height: 800 },
    activePage: 'p', pages: [{ id: 'p', name: 'Page', columns: [{ id: 'c', width: 500, windows: ['w'] }], floatingWindows: [], viewportX: 200 }] }],
});

test('ctrl wheel and pinch zoom continuously, clamp and ignore garbage', () => {
  assert.ok(wheelZoom(1, -100) > 1, 'wheel up zooms in');
  assert.ok(wheelZoom(1, 100) < 1, 'wheel down zooms out');
  assert.ok(Math.abs(wheelZoom(1, -100) / wheelZoom(1, 0) - Math.exp(.4)) < 1e-12);
  // Pinch deltas are small fractions and change the zoom a little each event.
  const pinch = wheelZoom(1, -2.5);
  assert.ok(pinch > 1 && pinch < 1.02);
  assert.equal(wheelZoom(1, -3, 1), wheelZoom(1, -100)); // 3 lines clamp like one notch.
  assert.equal(wheelZoom(1, 1, 2), wheelZoom(1, 100));
  let zoom = 1;
  for (let i = 0; i < 20; i++) zoom = wheelZoom(zoom, -100);
  assert.equal(zoom, OVERVIEW_ZOOM_MAX);
  for (let i = 0; i < 40; i++) zoom = wheelZoom(zoom, 100);
  assert.equal(zoom, OVERVIEW_ZOOM_MIN);
  assert.equal(wheelZoom(1.25, Number.NaN), 1.25);
  assert.equal(wheelZoom(Number.NaN, 10), 1);
});

test('zoom multiplies the fitted scale and drops map back to the same physical point', () => {
  const fitted = overviewScale(1.5, 1000, 400);
  assert.equal(overviewScale(1.5, 1000, 400, 1.6), fitted * 1.6);
  const state = snapshot(), page = state.monitors[0].pages[0];
  for (const zoom of [OVERVIEW_ZOOM_MIN, 1, 1.37, OVERVIEW_ZOOM_MAX]) {
    const scale = overviewScale(1.5, 1000, 400, zoom);
    assert.deepEqual(pageGeometry(page, state.monitors[0].viewport, scale),
      { width: 1000 * scale, height: 800 * scale, leading: 0, stripWidth: 1200 * scale });
    // A CSS point at physical (-250, 640) with the strip scrolled to viewportX 200.
    assert.deepEqual(dropCommand(state, false, 'w', 'p', 750 * scale, 600 * scale, 200 * scale, scale),
      { type: 'dropWindow', windowId: 'w', pageId: 'p', x: -250, y: 640, viewportX: 200 });
    // Dragging a column edge 50 physical pixels at any zoom.
    assert.equal(physicalWidth(500, 50 * scale, scale, 1000), 550);
  }
});
