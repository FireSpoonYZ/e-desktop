import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';
import { renderToStaticMarkup } from 'react-dom/server';
import { createElement } from 'react';

// Compile the owned TS seams in memory; no browser, IPC or native windows.
const modules = new Map();
function moduleUrl(relative) {
  const file = new URL(relative, import.meta.url);
  if (modules.has(file.href)) return modules.get(file.href);
  let source = readFileSync(file, 'utf8').replace(/import '\.\/style\.css';/g, '');
  let { outputText } = ts.transpileModule(source, { compilerOptions: {
    target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022, jsx: ts.JsxEmit.ReactJSX,
  } });
  outputText = outputText.replace(/from ['"]([^'"]+)['"]/g, (_, specifier) => {
    const url = specifier.startsWith('.')
      ? moduleUrl(new URL(`${specifier}.ts`, file).href)
      : import.meta.resolve(specifier);
    return `from '${url}'`;
  });
  const url = `data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`;
  modules.set(file.href, url);
  return url;
}
const { commandActions, paletteKey, nextSelection } = await import(moduleUrl('./actions.ts'));
const { emptySnapshot } = await import(moduleUrl('../model.ts'));
const { Overview } = await import(moduleUrl('../overview/index.tsx'));
const { CommandPalette } = await import(moduleUrl('./index.tsx'));
const rect = { x: 0, y: 0, width: 1000, height: 800 };
const snapshot = {
  ...emptySnapshot, enabled: true,
  backend: { ...emptySnapshot.backend, availability: 'ready', capabilities: { ...emptySnapshot.backend.capabilities, placement: true, focus: true, enumerate: true } },
  focusedWindow: 'w1', activeMonitor: 'm1',
  windows: ['w1', 'w2'].map((id) => ({ native: { id, title: id === 'w1' ? '中文文档' : 'Editor', appName: 'Writer', rect }, floating: false, fullscreen: false })),
  monitors: [{ monitor: { id: 'm1', name: 'Main', scaleFactor: 1 }, viewport: rect, activePage: 'p1', pages: [
    { id: 'p1', name: '第一页', viewportX: 0, columns: [{ id: 'c1', width: 600, windows: ['w1'] }, { id: 'c2', width: 300, windows: ['w2'] }], floatingWindows: [] },
    { id: 'p2', name: '第二页', viewportX: 0, columns: [], floatingWindows: [] },
  ] }],
};

test('search matches Chinese, mixed case and all tokens; results carry exact public commands', () => {
  const found = commandActions(snapshot, ' 中文 WRITER ');
  assert.equal(found.length, 1);
  assert.deepEqual(found[0].command, { type: 'focusWindow', windowId: 'w1' });
  assert.equal(found[0].disabled, false);
  assert.equal(commandActions(snapshot, 'ｆｕｌｌｓｃｒｅｅｎ')[0].command.type, 'toggleFullscreen');
  assert.deepEqual(commandActions(snapshot, '移动当前窗口到 第二页')[0].command, { type: 'moveWindowToPage', windowId: 'w1', pageId: 'p2' });
  assert.equal(commandActions(snapshot, 'not-a-real-window').length, 0);
  assert.ok(commandActions({ ...snapshot, enabled: false }, '中文')[0].disabled);
  assert.ok(commandActions(emptySnapshot, 'width')[0].disabled);
});

test('keyboard selection wraps, empty results are safe, IME cannot execute or dismiss', () => {
  assert.equal(nextSelection(0, -1, 3), 2);
  assert.equal(nextSelection(2, 1, 3), 0);
  assert.equal(nextSelection(0, 1, 0), 0);
  for (const key of ['Enter', 'Escape', 'ArrowDown', 'ArrowUp']) {
    assert.equal(paletteKey(key, true, 13), null);
    assert.equal(paletteKey(key, false, 229), null);
  }
  assert.equal(paletteKey('Enter', false, 13), 'execute');
  assert.equal(paletteKey('Escape', false, 27), 'dismiss');
  assert.equal(paletteKey('ArrowDown', false, 40), 'next');
  assert.equal(paletteKey('ArrowUp', false, 38), 'previous');
  assert.equal(paletteKey('a', false, 65), null);
});

test('overview renders ordered proportional columns, focused state and real destination controls', () => {
  const html = renderToStaticMarkup(createElement(Overview, { snapshot, onCommand() {}, onDismiss() {} }));
  assert.ok(html.indexOf('width:144px') < html.indexOf('width:72px'));
  assert.match(html, /data-focused="true"/);
  assert.match(html, /aria-pressed="true"/);
  assert.match(html, /<option value="p2">Main · 第二页<\/option>/);
  assert.match(html, /预览不可用/);
  assert.match(html, /此页没有平铺窗口/);
  assert.match(html, /aria-current="page"/);
});

function capture(Component, props) {
  let tree;
  function Capture() { tree = Component(props); return tree; }
  renderToStaticMarkup(createElement(Capture));
  const elements = [];
  function visit(node) {
    if (Array.isArray(node)) { node.forEach(visit); return; }
    if (!node || typeof node !== 'object' || !node.props) return;
    elements.push(node);
    visit(node.props.children);
  }
  visit(tree);
  return elements;
}

test('overview native select dispatches a move; focus dismisses only after command resolution', async () => {
  const commands = [];
  let resolve, dismissed = 0;
  const elements = capture(Overview, { snapshot, onCommand(command) {
    commands.push(command);
    return new Promise((done) => { resolve = done; });
  }, onDismiss() { dismissed++; } });
  const select = elements.find((element) => element.type === 'select');
  select.props.onChange({ target: { value: 'p2' } });
  assert.deepEqual(commands, [{ type: 'moveWindowToPage', windowId: 'w1', pageId: 'p2' }]);
  assert.equal(dismissed, 0);
  resolve(); await Promise.resolve();
  const focus = elements.find((element) => element.props.className === 'overview-window-focus');
  focus.props.onClick(); focus.props.onClick();
  assert.equal(commands.length, 2, 'duplicate pending dispatch is blocked');
  assert.deepEqual(commands[1], { type: 'focusWindow', windowId: 'w1' });
  assert.equal(dismissed, 0);
  resolve(); await Promise.resolve();
  assert.equal(dismissed, 1);
});

test('palette composition blocks actual Enter handler; rejected command does not dismiss', async () => {
  const commands = [];
  let dismissed = 0;
  const elements = capture(CommandPalette, { snapshot, onCommand(command) {
    commands.push(command); return Promise.reject(new Error('Native focus denied'));
  }, onDismiss() { dismissed++; } });
  const input = elements.find((element) => element.type === 'input');
  const event = { key: 'Enter', nativeEvent: { isComposing: false, keyCode: 13 }, preventDefault() {}, stopPropagation() {} };
  input.props.onCompositionStart();
  input.props.onKeyDown(event);
  assert.equal(commands.length, 0);
  input.props.onCompositionEnd();
  input.props.onKeyDown(event);
  await Promise.resolve();
  assert.deepEqual(commands, [{ type: 'focusWindow', windowId: 'w1' }]);
  assert.equal(dismissed, 0);
});

test('empty surfaces do not fabricate native windows and expose accessible search', () => {
  const props = { snapshot: emptySnapshot, onCommand() {}, onDismiss() {} };
  assert.match(renderToStaticMarkup(createElement(Overview, props)), /尚未发现可用显示器/);
  const html = renderToStaticMarkup(createElement(CommandPalette, props));
  assert.match(html, /role="combobox"/);
  assert.match(html, /当前没有可搜索的原生窗口/);
  assert.match(html, /当前不可用/);
});
