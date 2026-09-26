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
const { physicalPreviewSlot, WindowPreview } = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`);

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
