import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

const modules = new Map();
function moduleUrl(relative) {
  const file = new URL(relative, import.meta.url);
  if (modules.has(file.href)) return modules.get(file.href);
  const source = readFileSync(file, 'utf8').replace(/import '\.\/style\.css';/g, '');
  let { outputText } = ts.transpileModule(source, { compilerOptions: {
    target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022, jsx: ts.JsxEmit.ReactJSX,
  } });
  outputText = outputText.replace(/from ['"]([^'"]+)['"]/g, (_, specifier) => `from '${specifier.startsWith('.')
    ? moduleUrl(new URL(`${specifier}.ts`, file).href) : import.meta.resolve(specifier)}'`);
  const url = `data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`;
  modules.set(file.href, url);
  return url;
}
const { moveCommand, widthCommand, overviewScale, physicalWidth, trackDrag, trackResize } = await import(moduleUrl('../src/overview/pointer.ts'));
const { Overview } = await import(moduleUrl('../src/overview/index.tsx'));
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
function capture(props) {
  let tree;
  function Capture() { tree = Overview(props); return tree; }
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
function emit(host, type, fields = {}) {
  const event = Object.assign(new Event(type, { cancelable: true }), fields);
  host.dispatchEvent(event);
  return event;
}

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
      globalThis.document = { elementFromPoint: () => stale === 'outside' ? null : { closest: () => ({ dataset: { overviewPage: 'q' } }) } };
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
      assert.deepEqual(calls, stale === 'none' ? [{ type: 'moveWindowToPage', windowId: 'w', pageId: 'q' }] : []);
      emit(window, 'pointerup', { pointerId: 7, clientX: 50, clientY: 50 });
      assert.equal(calls.length, stale === 'none' ? 1 : 0);
    }
  } finally { globalThis.window = previousWindow; globalThis.document = previousDocument; }
});

test('overview Escape and lost pointer capture cancel drop without dismissing', () => {
  const previousWindow = globalThis.window, previousDocument = globalThis.document;
  try {
    globalThis.window = new EventTarget();
    globalThis.document = { elementFromPoint: () => ({ closest: () => ({ dataset: { overviewPage: 'q' } }) }) };
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
  assert.equal(overviewScale(2), .12);
  assert.equal(physicalWidth(500, 24, overviewScale(2), 1000), 700);
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
      emit(window, 'pointermove', { pointerId: 7, clientX: 34 });
      assert.deepEqual(calls, []);
      if (ending === 'gone') state.windows = [];
      if (ending === 'paused') state.enabled = false;
      if (ending === 'moved') state.monitors[0].pages[0].columns[0] = { ...state.monitors[0].pages[0].columns[0], id: 'new-column' };
      if (ending === 'lost-capture') handle.props.onLostPointerCapture();
      emit(window, 'pointerup', { pointerId: 7, clientX: 34 });
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
