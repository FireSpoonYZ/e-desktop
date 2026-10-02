import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

const modules = new Map();
function moduleUrl(relative, instrumented = false) {
  const file = new URL(relative, import.meta.url), key = file.href + instrumented;
  if (modules.has(key)) return modules.get(key);
  let source = readFileSync(file, 'utf8').replace(/import '\.\/style\.css';/g, '');
  if (instrumented) source = source
    .replace('useRef, useState }', 'useRef, useState as reactUseState }')
    .replace('usePreviewFeed, usePreviewSlots }', 'usePreviewFeed }')
    .replace("import { useOverviewZoom } from './zoom';", `
      const useState = (initial) => reactUseState(globalThis.overviewHooks.states.shift() ?? initial);
      const usePreviewSlots = (_root, options) => globalThis.overviewHooks.slots.push(options);
      const useOverviewZoom = () => ({ phase: null, onSlots() {}, requestClose: () => globalThis.overviewHooks.closes++ });
    `);
  let { outputText } = ts.transpileModule(source, { compilerOptions: {
    target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022, jsx: ts.JsxEmit.ReactJSX,
  } });
  outputText = outputText.replace(/from ['"]([^'"]+)['"]/g, (_, specifier) => `from '${specifier.startsWith('.')
    ? moduleUrl(new URL(`${specifier}${existsSync(new URL(`${specifier}.ts`, file)) ? ".ts" : ".tsx"}`, file).href) : import.meta.resolve(specifier)}'`);
  const url = `data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`;
  modules.set(key, url);
  return url;
}
const { dropCommand, moveCommand, widthCommand, overviewScale, pageGeometry, physicalWidth, trackDrag, trackResize } = await import(moduleUrl('../src/overview/pointer.ts'));
const { Overview } = await import(moduleUrl('../src/overview/index.tsx'));
const { Overview: InstrumentedOverview } = await import(moduleUrl('../src/overview/index.tsx', true));
const { emptySnapshot } = await import(moduleUrl('../src/model.ts'));
const rect = { x: 0, y: 0, width: 1000, height: 800 };
function snapshot() {
  return {
    ...emptySnapshot, enabled: true, focusedWindow: 'other', activeMonitor: 'other-monitor',
    backend: { ...emptySnapshot.backend, availability: 'ready', capabilities: { ...emptySnapshot.backend.capabilities, placement: true, focus: true } },
    windows: [{ native: { id: 'w', title: 'Editor', appName: 'App', rect }, floating: false, fullscreen: false }],
    monitors: [{ monitor: { id: 'm', name: 'Monitor', scaleFactor: 2 }, viewport: rect, activePage: 'p', pages: [
      { id: 'p', name: 'Page', columns: [{ id: 'c', width: 500, windows: ['w'] }], floatingWindows: [], viewportX: 0 },
      { id: 'q', name: 'Destination', columns: [], floatingWindows: [], viewportX: 0 },
    ] }],
  };
}
function capture(props, Component = Overview) {
  let tree;
  function Capture() { tree = Component(props); return tree; }
  renderToStaticMarkup(createElement(Capture));
  const elements = [];
  function visit(node) {
    if (Array.isArray(node)) return node.forEach(visit);
    if (!node?.props) return;
    elements.push(node); visit(node.props.children);
  }
  visit(tree);
  return elements;
}
function pageElement(pageId = 'q', scrollLeft = 0, left = 0, top = 0) {
  return { dataset: { overviewPage: pageId }, querySelector: () => ({
    getBoundingClientRect: () => ({ left, top }), clientLeft: 0, clientTop: 0, scrollLeft,
  }) };
}
function emit(host, type, fields = {}) {
  const event = Object.assign(new Event(type, { cancelable: true }), fields);
  host.dispatchEvent(event);
  return event;
}

test('overview scales the workspace, columns and signed viewport offsets together across DPI and screen sizes', () => {
  const page = snapshot().monitors[0].pages[0];
  assert.deepEqual(pageGeometry(page, rect, overviewScale(2)), {
    width: 250, height: 200, leading: 0, stripWidth: 250,
  });
  assert.equal(overviewScale(1, 3840, 960), .25);
  assert.equal(physicalWidth(500, 50, overviewScale(1, 3840, 960), 3840), 700);
  assert.deepEqual(pageGeometry({ ...page, viewportX: -250 }, rect, .5), {
    width: 500, height: 400, leading: 125, stripWidth: 500,
  });
  assert.deepEqual(pageGeometry({ ...page, viewportX: 750 }, rect, .5), {
    width: 500, height: 400, leading: 0, stripWidth: 875,
  });
});

test('overview move validates live window/page membership, pause, busy and capabilities', () => {
  const state = snapshot();
  assert.deepEqual(moveCommand(state, false, 'w', 'q'), { type: 'moveWindowToPage', windowId: 'w', pageId: 'q' });
  for (const [changed, busy, windowId, pageId] of [
    [state, true, 'w', 'q'], [state, false, 'gone', 'q'], [state, false, 'w', 'gone'], [state, false, 'w', 'p'],
    [{ ...state, enabled: false }, false, 'w', 'q'], [{ ...state, windows: [] }, false, 'w', 'q'],
    [{ ...state, monitors: [] }, false, 'w', 'q'],
    [{ ...state, backend: { ...state.backend, availability: 'unavailable' } }, false, 'w', 'q'],
    [{ ...state, backend: { ...state.backend, capabilities: { placement: false } } }, false, 'w', 'q'],
  ]) assert.equal(moveCommand(changed, busy, windowId, pageId), null);
});

test('overview pointer drop dispatches once, validates live targets/busy, and suppresses drag clicks', () => {
  const previousWindow = globalThis.window, previousDocument = globalThis.document;
  try {
    for (const stale of ['none', 'window', 'page', 'paused', 'busy', 'outside']) {
      globalThis.window = new EventTarget();
      globalThis.document = { elementFromPoint: () => stale === 'outside' ? null : { closest: () => pageElement() } };
      const state = snapshot(), calls = [];
      const elements = capture({ snapshot: state, busy: stale === 'busy', onCommand: (command) => calls.push(command), onDismiss() {} });
      const card = elements.find((node) => node.props.className === 'overview-window-focus');
      card.props.onPointerDown({ button: 0, pointerId: 7, clientX: 10, clientY: 10,
        currentTarget: { setPointerCapture() {}, hasPointerCapture() { return false; } } });
      emit(window, 'pointermove', { pointerId: 7, clientX: 50, clientY: 50 });
      assert.deepEqual(calls, []);
      if (stale === 'window') state.windows = [];
      if (stale === 'page') state.monitors[0].pages.pop();
      if (stale === 'paused') state.enabled = false;
      emit(window, 'pointerup', { pointerId: 7, clientX: 50, clientY: 50 });
      card.props.onClick({ detail: 1, preventDefault() {} });
      assert.deepEqual(calls, stale === 'none' ? [{ type: 'dropWindow', windowId: 'w', pageId: 'q', x: 200, y: 200, viewportX: 0 }] : []);
      emit(window, 'pointerup', { pointerId: 7, clientX: 50, clientY: 50 });
      assert.equal(calls.length, stale === 'none' ? 1 : 0);
    }
  } finally { globalThis.window = previousWindow; globalThis.document = previousDocument; }
});

test('overview Escape and lost pointer capture cancel drop without dismissing', () => {
  const previousWindow = globalThis.window, previousDocument = globalThis.document;
  try {
    globalThis.window = new EventTarget();
    globalThis.document = { elementFromPoint: () => ({ closest: () => pageElement() }) };
    const calls = [];
    let dismissed = 0;
    const elements = capture({ snapshot: snapshot(), onCommand: (command) => calls.push(command), onDismiss() { dismissed++; } });
    const card = elements.find((node) => node.props.className === 'overview-window-focus');
    for (const cancel of [() => elements[0].props.onKeyDown({ key: 'Escape', preventDefault() {}, stopPropagation() {} }), () => card.props.onLostPointerCapture()]) {
      card.props.onPointerDown({ button: 0, pointerId: 7, clientX: 10, clientY: 10,
        currentTarget: { setPointerCapture() {}, hasPointerCapture() { return false; } } });
      emit(window, 'pointermove', { pointerId: 7, clientX: 50, clientY: 50 }); cancel();
      emit(window, 'pointerup', { pointerId: 7, clientX: 50, clientY: 50 });
    }
    assert.deepEqual(calls, []); assert.equal(dismissed, 0);
  } finally { globalThis.window = previousWindow; globalThis.document = previousDocument; }
});

test('card drag threshold leaves clicks alone and disposal removes all drag callbacks', () => {
  const host = new EventTarget(), previews = [], finishes = [];
  const start = () => trackDrag(host, 7, 10, 10, (event) => previews.push(event.clientX), (event) => finishes.push(event?.clientX ?? null));
  start();
  emit(host, 'pointermove', { pointerId: 7, clientX: 12, clientY: 12 });
  emit(host, 'pointerup', { pointerId: 7, clientX: 12, clientY: 12 });
  assert.deepEqual(previews, []); assert.deepEqual(finishes, [null]);
  const dispose = start();
  emit(host, 'pointermove', { pointerId: 7, clientX: 40, clientY: 10 });
  assert.deepEqual(previews, [40]); dispose();
  emit(host, 'pointerup', { pointerId: 7, clientX: 40, clientY: 10 });
  assert.deepEqual(finishes, [null, null]);
});

test('physical resize uses overview scale, rounds/clamps and emits no focus command', () => {
  assert.equal(overviewScale(2), .25);
  assert.equal(physicalWidth(500, 50, overviewScale(2), 1000), 700);
  assert.equal(physicalWidth(500, -200, .12, 1000), 1);
  assert.equal(physicalWidth(500, 200, .12, 1000), 1000);
  assert.equal(physicalWidth(500, .2, .12, 1000), 502);
  const state = snapshot();
  const before = structuredClone(state);
  assert.deepEqual(widthCommand(state, false, 'w', 700), { type: 'setWindowColumnWidth', windowId: 'w', width: 700 });
  assert.deepEqual(widthCommand(state, false, 'w', 2000), { type: 'setWindowColumnWidth', windowId: 'w', width: 1000 });
  for (const width of [0, -1, 1.5, NaN, Infinity, 4294967296]) assert.equal(widthCommand(state, false, 'w', width), null);
  assert.equal(widthCommand(state, true, 'w', 700), null);
  assert.equal(widthCommand({ ...state, enabled: false }, false, 'w', 700), null);
  assert.equal(widthCommand(state, false, 'gone', 700), null);
  for (const flag of ['floating', 'fullscreen']) {
    const changed = structuredClone(state); changed.windows[0][flag] = true;
    assert.equal(widthCommand(changed, false, 'w', 700), null);
  }
  assert.deepEqual(state, before);
});

test('pointer resize previews only until release, ignores other pointers, cancels Escape/cancel/blur/disposal', () => {
  for (const ending of ['pointerup', 'pointercancel', 'Escape', 'blur', 'dispose']) {
    const host = new EventTarget(), previews = [], finishes = [];
    const cancel = trackResize(host, 7, 10, 500, .12, 1000, (width) => previews.push(width), (width) => finishes.push(width));
    emit(host, 'pointermove', { pointerId: 8, clientX: 100 });
    assert.deepEqual(previews, []);
    emit(host, 'pointermove', { pointerId: 7, clientX: 34 });
    assert.deepEqual(previews, [700]); assert.deepEqual(finishes, []);
    if (ending === 'dispose') cancel();
    else if (ending === 'Escape') assert.equal(emit(host, 'keydown', { key: 'Escape' }).defaultPrevented, true);
    else emit(host, ending, { pointerId: 7, clientX: 34 });
    assert.deepEqual(finishes, [ending === 'pointerup' ? 700 : null]);
    const count = previews.length;
    emit(host, 'pointermove', { pointerId: 7, clientX: 70 });
    emit(host, 'pointerup', { pointerId: 7, clientX: 70 }); cancel();
    assert.equal(previews.length, count); assert.equal(finishes.length, 1);
  }
});

test('overview resize handler commits once and rechecks stale or paused state on release', () => {
  const previousWindow = globalThis.window;
  try {
    for (const ending of ['valid', 'gone', 'paused', 'moved', 'lost-capture']) {
      globalThis.window = new EventTarget();
      const state = snapshot(), calls = [];
      const elements = capture({ snapshot: state, onCommand: (command) => calls.push(command), onDismiss() {} });
      const handle = elements.find((node) => node.props.className === 'overview-resize');
      handle.props.onPointerDown({ button: 0, pointerId: 7, clientX: 10, preventDefault() {},
        currentTarget: { setPointerCapture() {}, hasPointerCapture() { return false; } } });
      emit(window, 'pointermove', { pointerId: 7, clientX: 60 });
      assert.deepEqual(calls, []);
      if (ending === 'gone') state.windows = [];
      if (ending === 'paused') state.enabled = false;
      if (ending === 'moved') state.monitors[0].pages[0].columns[0] = { ...state.monitors[0].pages[0].columns[0], id: 'new-column' };
      if (ending === 'lost-capture') handle.props.onLostPointerCapture();
      emit(window, 'pointerup', { pointerId: 7, clientX: 60 });
      assert.deepEqual(calls, ending === 'valid' ? [{ type: 'setWindowColumnWidth', windowId: 'w', width: 700 }] : []);
    }
  } finally { globalThis.window = previousWindow; }
});

test('number input is physical, keyboard-operable and gated while busy; Escape discards edits', () => {
  const calls = [];
  const props = { snapshot: snapshot(), onCommand: (command) => calls.push(command), onDismiss() {} };
  const input = capture(props).find((node) => node.type === 'input');
  assert.equal(input.props.type, 'number'); assert.equal(input.props.defaultValue, 500);
  assert.equal(input.props.max, 1000); assert.equal(input.props.min, 1);
  const currentTarget = { value: '650', valueAsNumber: 650, validity: { valid: true }, blur() { input.props.onBlur({ currentTarget: this }); } };
  input.props.onKeyDown({ key: 'Enter', preventDefault() {}, currentTarget });
  assert.deepEqual(calls, [{ type: 'setWindowColumnWidth', windowId: 'w', width: 650 }]);
  let blurred = false;
  input.props.onKeyDown({ key: 'Escape', preventDefault() {}, stopPropagation() {}, currentTarget: { value: '800', blur() { blurred = true; assert.equal(this.value, '500'); } } });
  assert.equal(blurred, true);
  const busy = capture({ ...props, busy: true });
  assert.ok(busy.filter((node) => node.type === 'input' || node.props.className === 'overview-resize').every((node) => node.props.disabled));
});

test('overview drops keep desktop column-quarter and row geometry for same-page insertion/stacking', () => {
  const state = snapshot(), page = state.monitors[0].pages[0];
  page.columns.push({ id: 'second', width: 500, windows: ['other'] });
  state.windows.push({ native: { id: 'other', rect }, floating: false, fullscreen: false });
  const before = structuredClone(state);
  for (const [x, y] of [[140, 20], [187.5, 50], [237.5, 180]]) {
    assert.deepEqual(dropCommand(state, false, 'w', 'p', x, y, 0, .25), {
      type: 'dropWindow', windowId: 'w', pageId: 'p', x: x * 4, y: y * 4, viewportX: 0,
    });
  }
  // Never detach in the UI: backend resolves source-column removal/index changes atomically.
  assert.deepEqual(state, before);
  page.columns[0].windows.push('row');
  state.windows.push({ native: { id: 'row', rect }, floating: false, fullscreen: false });
  assert.deepEqual(dropCommand(state, false, 'w', 'p', 187.5, 50, 0, .25), {
    type: 'dropWindow', windowId: 'w', pageId: 'p', x: 750, y: 200, viewportX: 0,
  });
});

test('background-page drops restore negative monitor origin, fitted DPI scale, manual scroll and signed leading', () => {
  const state = snapshot(), page = state.monitors[0].pages.pop();
  const monitor = { ...state.monitors[0], monitor: { id: 'negative', scaleFactor: 1.25 },
    activePage: 'other-page', pages: [page], viewport: { x: -1600, y: -200, width: 1000, height: 800 } };
  state.monitors.push(monitor);
  const scale = overviewScale(1.25, 1000, 200);
  page.viewportX = 100;
  assert.deepEqual(dropCommand(state, false, 'w', 'q', 50, 40, 320, scale), {
    type: 'dropWindow', windowId: 'w', pageId: 'q', x: -1350, y: 0, viewportX: 1600,
  });
  page.viewportX = -250;
  assert.deepEqual(dropCommand(state, false, 'w', 'q', 50, 40, 80, scale), {
    type: 'dropWindow', windowId: 'w', pageId: 'q', x: -1350, y: 0, viewportX: 150,
  });
  assert.deepEqual(dropCommand(state, false, 'w', 'q', 50, 40, 0, scale), {
    type: 'dropWindow', windowId: 'w', pageId: 'q', x: -1350, y: 0, viewportX: -250,
  });
  assert.equal(dropCommand(state, false, 'w', 'q', 199.99, 159.99, 0, scale).x, -601);
  assert.equal(dropCommand(state, false, 'w', 'q', 199.99, 159.99, 0, scale).y, 599);
});

test('overview drop rejects stale, disabled, outside, nonfinite and i32-overflow geometry; floating preserves page moves', () => {
  const state = snapshot();
  const drop = (changed = state, busy = false, windowId = 'w', pageId = 'p', x = 50, y = 50, scroll = 0, scale = .25) =>
    dropCommand(changed, busy, windowId, pageId, x, y, scroll, scale);
  for (const args of [
    [state, true], [state, false, 'gone'], [state, false, 'w', 'gone'],
    [{ ...state, enabled: false }], [{ ...state, monitors: [] }],
    [{ ...state, backend: { ...state.backend, availability: 'unavailable' } }],
    [{ ...state, backend: { ...state.backend, capabilities: { placement: false } } }],
  ]) assert.equal(drop(...args), null);
  for (const [x, y, scroll, scale] of [
    [-1, 50, 0, .25], [250, 50, 0, .25], [50, 200, 0, .25],
    [NaN, 50, 0, .25], [50, Infinity, 0, .25], [50, 50, -1, .25],
    [50, 50, Infinity, .25], [50, 50, 2147483648, .25], [50, 50, 0, 0],
    [50, 50, 0, NaN], [50, 50, 0, Infinity],
  ]) assert.equal(drop(state, false, 'w', 'p', x, y, scroll, scale), null);
  state.monitors[0].viewport = { ...rect, x: 2147483647 };
  assert.equal(drop(), null);
  state.monitors[0].viewport = rect;
  state.windows[0].floating = true;
  state.monitors[0].pages[0].columns = [];
  state.monitors[0].pages[0].floatingWindows = ['w'];
  assert.equal(drop(), null);
  assert.deepEqual(drop(state, false, 'w', 'q'), { type: 'moveWindowToPage', windowId: 'w', pageId: 'q' });
  state.windows[0].floating = false;
  assert.equal(drop(), null);
});

test('overview handler emits one point-aware same-page drop including client border and live scroll', () => {
  const previousWindow = globalThis.window, previousDocument = globalThis.document;
  try {
    globalThis.window = new EventTarget();
    const page = pageElement('p', 125, 20, 30), scroll = page.querySelector();
    scroll.clientLeft = 2; scroll.clientTop = 3; page.querySelector = () => scroll;
    globalThis.document = { elementFromPoint: () => ({ closest: () => page }) };
    const calls = [], state = snapshot();
    const card = capture({ snapshot: state, onCommand: (command) => calls.push(command), onDismiss() {} })
      .find((node) => node.props.className === 'overview-window-focus');
    card.props.onPointerDown({ button: 0, pointerId: 7, clientX: 30, clientY: 40,
      currentTarget: { setPointerCapture() {}, hasPointerCapture() { return false; } } });
    emit(window, 'pointermove', { pointerId: 7, clientX: 72, clientY: 83 });
    scroll.scrollLeft = 250;
    emit(window, 'pointerup', { pointerId: 7, clientX: 72, clientY: 83 });
    assert.deepEqual(calls, [{ type: 'dropWindow', windowId: 'w', pageId: 'p', x: 200, y: 200, viewportX: 1000 }]);
  } finally { globalThis.window = previousWindow; globalThis.document = previousDocument; }
});

test('overview preview slots remain active in drag/resize renders; every close action requests zoom', async () => {
  const previous = globalThis.overviewHooks;
  try {
    for (const gesture of ['drag', 'resize', 'none']) {
      globalThis.overviewHooks = { states: [undefined, undefined,
        gesture === 'drag' ? { windowId: 'w', pageId: 'p' } : undefined,
        gesture === 'resize' ? { columnId: 'c', width: 700 } : undefined], slots: [], closes: 0 };
      const props = { snapshot: snapshot(), onCommand() {}, onDismiss() { assert.fail('bypassed zoom.requestClose'); } };
      const elements = capture(props, InstrumentedOverview);
      assert.equal(overviewHooks.slots.length, 1);
      assert.notEqual(overviewHooks.slots[0].active, false);
      if (gesture !== 'none') assert.equal(elements.find((node) => node.props['aria-pressed'] === true)?.props.disabled, true);
      if (gesture === 'resize') assert.ok(elements.some((node) => node.type === 'output' && node.props.children[0] === 700));
      const close = elements.find((node) => node.props.className === 'overview-close');
      close.props.onClick();
      elements[0].props.onKeyDown({ key: 'Escape', nativeEvent: {}, preventDefault() {}, stopPropagation() {} });
      const page = elements.find((node) => node.type === 'button' && node.props['aria-current'] === 'page');
      page.props.onClick(); await Promise.resolve();
      const card = elements.find((node) => node.props.className === 'overview-window-focus');
      card.props.onClick({ detail: 0 }); await Promise.resolve();
      assert.equal(overviewHooks.closes, 4);
    }
  } finally { globalThis.overviewHooks = previous; }
});
