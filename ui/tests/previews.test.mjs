import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { renderToStaticMarkup } from 'react-dom/server';
import { createElement } from 'react';
import ts from 'typescript';

const source = readFileSync(new URL('../src/overview/previews.tsx', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022, jsx: ts.JsxEmit.ReactJSX },
  fileName: 'previews.tsx',
});
const compiled = outputText.replace(/(['"])(react(?:\/jsx-runtime)?)\1/g,
  (_, quote, name) => `${quote}${import.meta.resolve(name)}${quote}`);
const { physicalPreviewSlot, WindowPreview, previewQueue } = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`);

const bounds = { left: -10.2, top: 20.2, right: 100.6, bottom: 80.8 };
test('preview physical rectangles round inward and intersect scrolling clips at fractional DPI', () => {
  assert.deepEqual(physicalPreviewSlot('win-1', bounds, { left: 0, top: 25, right: 90, bottom: 100 }, 1.25), {
    windowId: 'win-1',
    rect: { x: -12, y: 26, width: 137, height: 75 },
    clip: { x: 0, y: 32, width: 112, height: 69 },
  });
  const slot = physicalPreviewSlot('win-1', bounds, { left: -100, top: -100, right: 1000, bottom: 1000 }, 2);
  assert.deepEqual(slot.clip, slot.rect);
});

test('hidden, nonfinite and Win32-overflow preview rectangles never reach the host', () => {
  for (const scale of [0, -1, Infinity, NaN]) assert.equal(physicalPreviewSlot('w', bounds, bounds, scale), null);
  assert.equal(physicalPreviewSlot('w', bounds, { left: 200, top: 0, right: 300, bottom: 100 }, 1), null);
  assert.equal(physicalPreviewSlot('w', { ...bounds, right: Infinity }, bounds, 1), null);
  assert.equal(physicalPreviewSlot('w', { ...bounds, right: 2147483648 }, bounds, 1), null);
  assert.equal(physicalPreviewSlot('w', { left: 0.1, top: 0.1, right: 0.2, bottom: 0.2 }, bounds, 1), null);
});

test('preview placeholders distinguish unsupported, pending, gone and failed without trusting mismatched replies', () => {
  const render = (props) => renderToStaticMarkup(createElement(WindowPreview, { windowId: 'w', available: true, ...props }));
  assert.match(render({ available: false }), /此平台不支持实时预览/);
  assert.match(render({}), /等待实时预览/);
  assert.match(render({ status: { windowId: 'w', state: 'sourceGone', message: 'gone' } }), /窗口已不可用/);
  assert.match(render({ status: { windowId: 'w', state: 'failed', message: 'HRESULT' } }), /实时预览失败：HRESULT/);
  assert.match(render({ status: { windowId: 'w', state: 'unavailable', message: '' } }), /实时预览不可用/);
  assert.match(render({ status: { windowId: 'other', state: 'ready', message: '' } }), /等待实时预览/);
  const ready = render({ status: { windowId: 'w', state: 'ready', message: '' } });
  assert.match(ready, /aria-label="窗口实时预览"/);
  assert.doesNotMatch(ready, /<small>/);
  assert.doesNotMatch(ready, /button|tabindex|onclick/i);
});


test('preview IPC serializes, coalesces geometry, drops stale replies and clears after disposal', async () => {
  const calls = [], received = [];
  const feed = previewQueue(7, (session, slots) => new Promise((resolve, reject) => calls.push({ session, slots, resolve, reject })),
    (statuses) => received.push(statuses));
  const a = [{ windowId: 'a' }], b = [{ windowId: 'b' }], c = [{ windowId: 'c' }];
  feed.push(a); feed.push(b); feed.push(c);
  assert.equal(calls.length, 1);
  calls[0].resolve([{ windowId: 'a', state: 'ready' }]); await Promise.resolve();
  assert.deepEqual(received, []);
  assert.deepEqual(calls[1].slots, c);
  assert.equal(calls[1].session, 7);
  calls[1].reject(new Error('gone')); await Promise.resolve();
  assert.equal(received[0][0].state, 'failed');
  feed.refresh();
  feed.dispose(); feed.push(a); feed.refresh();
  calls[2].resolve([{ windowId: 'c', state: 'ready' }]); await Promise.resolve();
  assert.equal(received.length, 1);
  assert.deepEqual(calls[3].slots, []);
  calls[3].resolve([]); await Promise.resolve();
  feed.dispose(); feed.refresh();
  assert.equal(calls.length, 4);
});

const settle = () => new Promise((resolve) => setImmediate(resolve));

// Exercise layout-effect cleanup/setup without a DOM or native window operations.
let harnessId = 0;
async function previewFeedHarness() {
  const hooksUrl = `data:text/javascript;base64,${Buffer.from(`
    export const effects = [], refs = [], updates = [];
    let cursor = 0;
    export const render = () => { cursor = 0; effects.length = 0; };
    export const useRef = (value) => refs[cursor++] ??= { current: value };
    export const useState = (value) => [value, (next) => updates.push(next)];
    export const useCallback = (callback) => callback;
    export const useLayoutEffect = (setup) => effects.push(setup);
    // Isolate the module's hook storage for each test.
    export const id = ${harnessId++};
  `).toString('base64')}`;
  const hooks = await import(hooksUrl);
  const feedModule = compiled.replace(import.meta.resolve('react'), hooksUrl);
  assert.notEqual(feedModule, compiled, 'replace only the React hook import');
  const { usePreviewFeed } = await import(`data:text/javascript;base64,${Buffer.from(feedModule).toString('base64')}`);
  return { ...hooks, usePreviewFeed };
}

const previewSlot = (windowId, x = 0) => {
  const rect = { x, y: 0, width: 100, height: 100 };
  return { windowId, rect, clip: rect };
};

const deferredPreviews = () => {
  const calls = [], native = new Map();
  const request = (session, slots) => new Promise((resolve, reject) => calls.push({ session, slots, reject,
    finish() {
      native.clear();
      slots.forEach((slot) => native.set(slot.windowId, slot));
      resolve(slots.map(({ windowId }) => ({ windowId, state: 'ready', message: '' })));
    },
  }));
  return { calls, native, request };
};

test('preview feed effect replay waits for old same-session release before creating replacement slots', async (t) => {
  const hooks = await previewFeedHarness();
  const { calls, native, request } = deferredPreviews();
  const feed = hooks.usePreviewFeed(7, request);
  const setup = hooks.effects[0];
  const oldCleanup = setup();
  const first = [previewSlot('a')], latest = [previewSlot('a', 20)];
  feed.publish(first);
  oldCleanup();
  const cleanup = setup();
  t.after(cleanup);
  feed.publish(latest);
  await settle();
  assert.equal(calls.length, 1, 'replacement cannot race the old create/clear');
  calls[0].finish(); await settle();
  assert.deepEqual(calls[1].slots, []);
  assert.equal(calls.length, 2);
  assert.equal(hooks.updates.length, 2, 'old reply did not publish a ready status');
  calls[1].finish(); await settle();
  assert.deepEqual(calls[2].slots, latest);
  calls[2].finish(); await settle();
  assert.deepEqual([...native.values()], latest);
  assert.equal(hooks.updates.at(-1).a.state, 'ready');
  cleanup();
  await settle();
  assert.deepEqual(calls[3].slots, []);
  calls[3].finish(); await settle();
  assert.equal(native.size, 0);
  assert.equal(calls.length, 4);
});

test('preview feed new native session does not wait for an obsolete pending request', async (t) => {
  const hooks = await previewFeedHarness();
  const { calls, request } = deferredPreviews();
  const oldFeed = hooks.usePreviewFeed(7, request);
  const oldCleanup = hooks.effects[0]();
  oldFeed.publish([previewSlot('old')]);
  oldCleanup();
  hooks.render();
  const nextFeed = hooks.usePreviewFeed(8, request);
  const cleanup = hooks.effects[0]();
  t.after(cleanup);
  nextFeed.publish([previewSlot('new')]);
  assert.deepEqual(calls.map(({ session }) => session), [7, 8]);
  calls[1].finish(); await settle();
  assert.equal(hooks.updates.at(-1).new.state, 'ready');
  calls[0].reject(new Error('old source gone')); await settle();
  assert.deepEqual(calls[2].slots, []);
  calls[2].finish(); await settle();
  assert.equal(hooks.updates.at(-1).new.state, 'ready');
  cleanup();
  calls[3].finish(); await settle();
});

test('preview geometry updates and source removal never send an intermediate clear', async () => {
  const { calls, native, request } = deferredPreviews();
  const received = [];
  const feed = previewQueue(7, request, (statuses) => received.push(statuses));
  feed.push([previewSlot('a'), previewSlot('b')]);
  calls[0].finish(); await settle();
  feed.push([previewSlot('a', 5), previewSlot('b')]);
  feed.push([previewSlot('a', 10)]);
  calls[1].finish(); await settle();
  assert.equal(received.length, 1, 'reply with removed source is stale');
  calls[2].finish(); await settle();
  assert.deepEqual([...native.values()], [previewSlot('a', 10)]);
  assert.deepEqual(received.at(-1).map(({ windowId }) => windowId), ['a']);
  assert.ok(calls.every(({ slots }) => slots.length > 0));
  const released = feed.dispose();
  let done = false;
  void released.then(() => { done = true; });
  await settle();
  assert.equal(done, false, 'release promise waits for native clear');
  calls[3].reject(new Error('host already dismissed'));
  await released;
  assert.equal(feed.dispose(), released);
  assert.equal(calls.length, 4);
  assert.equal(received.length, 2, 'failed disposal never restores statuses');
});
