import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
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
  assert.equal(dispatcher.push(100), false, 'busy input is dropped, not queued');
  t.mock.timers.tick(1000);
  assert.equal(calls.length, 1);
  resolve(); await Promise.resolve();
  for (let i = 0; i < 20; i++) dispatcher.push(3);
  t.mock.timers.tick(80);
  assert.deepEqual(calls.at(-1), { type: 'scroll', monitorId: 'secondary', delta: 60 });
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
