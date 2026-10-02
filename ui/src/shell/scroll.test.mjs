import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { createRequire } from 'node:module';
import ts from 'typescript';

const source = readFileSync(new URL('./scroll.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 },
});
const { selectedMonitor, scrollTarget, wheelPixels, createScrollDispatcher } = await import(
  `data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);
const primary = { monitor: { id: 'primary', primary: true, scaleFactor: 1 }, viewport: { width: 1920 },
  activePage: 'p1', pages: [{ id: 'p1' }] };
const secondary = { monitor: { id: 'secondary', primary: false, scaleFactor: 1.5 }, viewport: { width: 2400 },
  activePage: 'p2', pages: [{ id: 'p2' }] };
const snapshot = { enabled: true, backend: { availability: 'ready', capabilities: { placement: true } },
  activeMonitor: 'secondary', monitors: [primary, secondary] };

test('scroll targets only the selected monitor; paused, busy, missing and invalid targets are blocked', () => {
  assert.equal(selectedMonitor(snapshot), secondary);
  assert.equal(scrollTarget(snapshot), secondary);
  for (const change of [{ enabled: false }, { activeMonitor: null }, { activeMonitor: 'removed' },
    { backend: { availability: 'unavailable', capabilities: { placement: true } } },
    { backend: { availability: 'ready', capabilities: { placement: false } } },
    { monitors: [{ ...secondary, activePage: 'removed' }] },
    { monitors: [{ ...secondary, monitor: { ...secondary.monitor, scaleFactor: 0 } }] }]) {
    assert.equal(scrollTarget({ ...snapshot, ...change }), undefined);
  }
  assert.equal(scrollTarget(snapshot, true), undefined);
});

test('wheel converts dominant axis, logical pixels, lines and pages to physical pixels without pinch', () => {
  const event = { deltaX: 0, deltaY: 10, deltaMode: 0, ctrlKey: false };
  assert.equal(wheelPixels(event, secondary, 18), 15);
  assert.equal(wheelPixels({ ...event, deltaX: -20 }, secondary, 18), -30);
  assert.equal(wheelPixels({ ...event, deltaX: 4 }, secondary, 18), 15, 'do not double count diagonal gestures');
  assert.equal(wheelPixels({ ...event, deltaMode: 1, deltaY: -2 }, secondary, 18), -54);
  assert.equal(wheelPixels({ ...event, deltaMode: 2, deltaY: 1 }, secondary, 18), 2400, 'page width is already physical');
  assert.equal(wheelPixels({ ...event, deltaY: .2 }, secondary, 18), .2 * 1.5);
  assert.equal(wheelPixels({ ...event, ctrlKey: true }, secondary, 18), 0);
  assert.equal(wheelPixels({ ...event, deltaMode: 3 }, secondary, 18), 0);
  assert.equal(wheelPixels({ ...event, deltaY: Infinity }, secondary, 18), 0);
});

test('wheel bursts batch for 80ms, preserve fractions, bound IPC integers and never overlap requests', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const calls = [];
  let resolve;
  const dispatcher = createScrollDispatcher(() => secondary, (command) => {
    calls.push(command);
    return new Promise((done) => { resolve = done; });
  });
  dispatcher.push(.4);
  t.mock.timers.tick(80);
  assert.equal(calls.length, 0);
  dispatcher.push(.4);
  dispatcher.push(.4);
  t.mock.timers.tick(79);
  assert.equal(calls.length, 0);
  t.mock.timers.tick(1);
  assert.deepEqual(calls, [{ type: 'scroll', monitorId: 'secondary', delta: 1 }]);
  assert.equal(dispatcher.push(100), true, 'in-flight input is retained');
  dispatcher.push(-160);
  t.mock.timers.tick(1000);
  assert.equal(calls.length, 1, 'requests never overlap, even after the batching deadline');
  resolve(); await Promise.resolve();
  assert.equal(calls.at(-1).delta, -59, 'opposite motion reverses the queued direction, retaining the fraction');
  resolve(); await Promise.resolve();
  for (let i = 0; i < 20; i++) dispatcher.push(3);
  t.mock.timers.tick(80);
  assert.deepEqual(calls.at(-1), { type: 'scroll', monitorId: 'secondary', delta: 59 });
  resolve(); await Promise.resolve();
  dispatcher.push(Number.MAX_VALUE);
  t.mock.timers.tick(80);
  assert.equal(calls.at(-1).delta, 2147483647);
  dispatcher.dispose();
  assert.equal(dispatcher.push(10), false);
  resolve(); await Promise.resolve();
});

test('queued motion is cancelled for busy/paused/changed targets, disposal and rejected IPC', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const calls = [];
  let target = secondary;
  const dispatcher = createScrollDispatcher(() => target, async (command) => {
    calls.push(command); throw new Error('native error retained by App');
  });
  for (const next of [undefined, primary, { ...secondary, activePage: 'p3' },
    { ...secondary, monitor: { ...secondary.monitor, scaleFactor: 2 } }]) {
    target = secondary;
    dispatcher.push(30);
    target = next;
    t.mock.timers.tick(80);
    assert.equal(calls.length, 0);
  }
  target = secondary;
  dispatcher.push(30);
  dispatcher.cancel();
  t.mock.timers.tick(80);
  assert.equal(calls.length, 0);
  dispatcher.push(30);
  t.mock.timers.tick(80);
  await Promise.resolve();
  assert.equal(calls.length, 1);
  dispatcher.push(-10);
  t.mock.timers.tick(80);
  await Promise.resolve();
  assert.equal(calls.at(-1).delta, -10, 'rejection unlocks and discards stale motion');
  dispatcher.push(50);
  dispatcher.dispose();
  t.mock.timers.tick(80);
  assert.equal(calls.length, 2);
});


test('completion before the next deadline keeps batching; cancellation does not unlock in-flight IPC', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const calls = [];
  let target = secondary;
  const dispatcher = createScrollDispatcher(() => target, (command) =>
    new Promise((resolve, reject) => calls.push({ command, resolve, reject })));
  dispatcher.push(10);
  t.mock.timers.tick(80);
  dispatcher.push(20);
  t.mock.timers.tick(40);
  calls[0].resolve(); await Promise.resolve();
  assert.equal(calls.length, 1);
  t.mock.timers.tick(39);
  assert.equal(calls.length, 1);
  t.mock.timers.tick(1);
  assert.equal(calls[1].command.delta, 20);
  dispatcher.push(30);
  dispatcher.cancel();
  target = primary;
  dispatcher.push(-40);
  t.mock.timers.tick(80);
  assert.equal(calls.length, 2, 'cancel never permits overlapping IPC');
  calls[1].reject(new Error('old request failed')); await Promise.resolve();
  assert.deepEqual(calls[2].command, { type: 'scroll', monitorId: 'primary', delta: -40 },
    'an obsolete rejection cannot discard new target motion');
  dispatcher.push(50);
  dispatcher.dispose();
  calls[2].resolve(); await Promise.resolve();
  t.mock.timers.tick(1000);
  assert.equal(calls.length, 3, 'dispose prevents completion from draining');
});

test('in-flight errors, cancellation and invalid targets discard queued motion without replay', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  for (const end of ['reject', 'cancel', 'target']) {
    let target = secondary;
    const calls = [];
    const dispatcher = createScrollDispatcher(() => target, (command) =>
      new Promise((resolve, reject) => calls.push({ command, resolve, reject })));
    dispatcher.push(10);
    t.mock.timers.tick(80);
    dispatcher.push(20);
    t.mock.timers.tick(80);
    if (end === 'cancel') dispatcher.cancel();
    if (end === 'target') target = undefined;
    if (end === 'reject') calls[0].reject(new Error('IPC failed'));
    else calls[0].resolve();
    await Promise.resolve();
    t.mock.timers.tick(1000);
    assert.equal(calls.length, 1, end);
    target = secondary;
    dispatcher.push(-5);
    t.mock.timers.tick(80);
    assert.equal(calls[1].command.delta, -5, end + ' releases the lock and clears old motion');
    calls[1].resolve(); await Promise.resolve();
    dispatcher.dispose();
  }
});

test('ScrollControls retains its own busy wheel input but cancels for unrelated busy, target changes and unmount', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  // Exercise the actual component callbacks/effects with only refs and dependency tracking, no DOM.
  const require = createRequire(import.meta.url);
  const refs = [], effects = [];
  let cursor = 0;
  const hooks = {
    useRef(value) { return refs[cursor++] ??= { current: value }; },
    useEffect(run, deps) {
      const index = cursor++;
      const previous = effects[index];
      if (!previous || deps.some((value, i) => !Object.is(value, previous.deps[i]))) {
        effects[index] = { deps, run, cleanup: previous?.cleanup, changed: true };
      }
    },
  };
  const componentSource = readFileSync(new URL('./ScrollControls.tsx', import.meta.url), 'utf8');
  const { outputText } = ts.transpileModule(componentSource, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  });
  const exports = {};
  new Function('require', 'exports', outputText)((id) => id === 'react' ? hooks
    : id === './scroll' ? { createScrollDispatcher, scrollTarget, selectedMonitor, wheelPixels }
      : require(id), exports);
  let wheel;
  const element = {
    addEventListener(name, listener) { assert.equal(name, 'wheel'); wheel = listener; },
    removeEventListener(name, listener) { assert.equal(wheel, listener); wheel = undefined; },
  };
  const originalStyle = Object.getOwnPropertyDescriptor(globalThis, 'getComputedStyle');
  globalThis.getComputedStyle = () => ({ lineHeight: '18px' });
  t.after(() => {
    if (originalStyle) Object.defineProperty(globalThis, 'getComputedStyle', originalStyle);
    else delete globalThis.getComputedStyle;
  });
  const calls = [];
  const props = { snapshot, busy: false, onCommand: (command) =>
    new Promise((resolve, reject) => calls.push({ command, resolve, reject })) };
  const render = () => {
    cursor = 0;
    const tree = exports.ScrollControls(props);
    refs[0].current = element;
    for (const effect of effects) if (effect?.changed) {
      effect.changed = false;
      effect.cleanup?.();
      effect.cleanup = effect.run();
    }
    return tree.props['aria-disabled'];
  };
  const push = (deltaY) => {
    let prevented = false;
    wheel({ deltaX: 0, deltaY, deltaMode: 0, ctrlKey: false,
      preventDefault() { prevented = true; } });
    return prevented;
  };
  const settle = async (call) => {
    call.resolve();
    // The component wrapper and dispatcher each await the native command.
    await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
  };
  assert.equal(render(), false);
  props.busy = true;
  props.busyCommand = 'scroll';
  assert.equal(render(), true, 'another scroll owner is not this dispatcher');
  assert.equal(push(10), false);
  props.busy = false;
  render();
  assert.equal(push(10), true);
  t.mock.timers.tick(80);
  props.busy = true;
  props.busyCommand = 'scroll';
  assert.equal(render(), false, 'own IPC must not disable/cancel the region');
  props.busyCommand = undefined;
  assert.equal(render(), true, 'unknown busy ownership is conservatively blocked');
  assert.equal(push(20), false);
  props.busyCommand = 'scroll';
  assert.equal(render(), false);
  assert.equal(push(-20), true);
  t.mock.timers.tick(80);
  assert.equal(calls.length, 1);
  await settle(calls[0]);
  assert.equal(calls[1].command.delta, -30, 'drain works before React renders busy=false');
  await settle(calls[1]);
  assert.equal(push(10), true);
  props.busyCommand = 'refresh';
  assert.equal(render(), true, 'unrelated busy blocks without an intermediate busy=false render');
  assert.equal(push(20), false);
  t.mock.timers.tick(80);
  assert.equal(calls.length, 2, 'unrelated busy cancels queued input');
  props.busy = false;
  render();
  push(10);
  t.mock.timers.tick(80);
  props.busy = true;
  props.busyCommand = 'scroll';
  render();
  push(20);
  props.snapshot = { ...snapshot, monitors: [{ ...secondary, activePage: 'p3', pages: [{ id: 'p3' }] }] };
  render();
  await settle(calls[2]);
  props.busy = false;
  render();
  t.mock.timers.tick(80);
  assert.equal(calls.length, 3, 'changing page cancels even during own busy');
  push(10);
  t.mock.timers.tick(80);
  props.busy = true;
  render();
  push(20);
  props.snapshot = { ...props.snapshot, enabled: false };
  assert.equal(render(), true, 'pause disables even an owned scroll');
  assert.equal(push(20), false);
  props.snapshot = { ...props.snapshot, enabled: true };
  render();
  push(30);
  for (const effect of effects) effect?.cleanup?.();
  assert.equal(wheel, undefined);
  await settle(calls[3]);
  t.mock.timers.tick(80);
  assert.equal(calls.length, 4, 'unmount never drains');
});
