import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

// lane: ui-animation — hotkey overlay descriptions and rendering.
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
const { formatKey, describeAction, groupHotkeys } = await import(moduleUrl('../src/hotkeys/describe.ts'));
const { HotkeyOverlay } = await import(moduleUrl('../src/hotkeys/index.tsx'));

const command = (command) => ({ type: 'command', command });

test('normalized keys read like the README', () => {
  assert.equal(formatKey('control+alt+KeyH'), 'Ctrl+Alt+H');
  assert.equal(formatKey('shift+control+alt+Digit3'), 'Ctrl+Alt+Shift+3');
  assert.equal(formatKey('control+alt+Slash'), 'Ctrl+Alt+/');
  assert.equal(formatKey('control+ArrowLeft'), 'Ctrl+←');
  assert.equal(formatKey('super+alt+PageDown'), 'Alt+Win+PageDown');
  assert.equal(formatKey('control+alt+Backspace'), 'Ctrl+Alt+Backspace');
  assert.equal(formatKey('Key'), 'Key');
});

test('known actions get Chinese descriptions in their group', () => {
  assert.deepEqual(describeAction(command({ type: 'focusDirection', direction: 'left' })), ['聚焦', '向左聚焦']);
  assert.deepEqual(describeAction(command({ type: 'moveWindow', direction: 'down' })), ['移动窗口', '向下移动当前窗口']);
  assert.deepEqual(describeAction(command({ type: 'adjustColumnWidth', delta: -50 })), ['尺寸', '减少当前列宽 50 物理像素']);
  assert.deepEqual(describeAction(command({ type: 'adjustWindowHeight', delta: 50 })), ['尺寸', '增加窗口高度 50 物理像素']);
  assert.deepEqual(describeAction(command({ type: 'disable' })), ['应用', '暂停并还原']);
  assert.deepEqual(describeAction({ type: 'relativePage', delta: 1, moveWindow: true }), ['工作区', '把当前窗口移至下一页并跟随']);
  assert.deepEqual(describeAction({ type: 'relativePage', delta: -2 }), ['工作区', '切换到向上第 2 页']);
  assert.deepEqual(describeAction({ type: 'page', number: 4, moveWindow: false }), ['工作区', '进入第 4 页']);
  assert.deepEqual(describeAction({ type: 'scroll', direction: 'right' }), ['视口', '向右滑动一列并聚焦']);
  assert.deepEqual(describeAction({ type: 'hotkeyOverlay' }), ['界面', '显示/关闭快捷键提示']);
});

test('unknown action and command types fall back to the type name and parameters', () => {
  assert.deepEqual(describeAction({ type: 'screenshot', area: 'window', delay: 2 }), ['其他', 'screenshot（area: window，delay: 2）']);
  assert.deepEqual(describeAction({ type: 'toggleTabs' }), ['其他', 'toggleTabs']);
  assert.deepEqual(describeAction(command({ type: 'swapColumns', direction: 'left', nested: { a: 1 } })),
    ['其他', '命令 swapColumns（direction: left，nested: {"a":1}）']);
  assert.deepEqual(describeAction({ type: 'command', command: null }), ['其他', 'command（command: null）']);
});

test('effective bindings are grouped in a fixed order and shared descriptions merge keys', () => {
  const groups = groupHotkeys([
    { key: 'control+alt+Slash', action: { type: 'hotkeyOverlay' } },
    { key: 'control+ArrowLeft', action: { type: 'scroll', direction: 'left' } },
    { key: 'control+alt+ArrowLeft', action: { type: 'scroll', direction: 'left' } },
    { key: 'control+alt+KeyH', action: command({ type: 'focusDirection', direction: 'left' }) },
    { key: 'control+alt+KeyZ', action: { type: 'future', value: 1 } },
  ]);
  assert.deepEqual(groups.map((group) => group.title), ['聚焦', '视口', '界面', '其他']);
  assert.deepEqual(groups[1].rows, [{ keys: ['Ctrl+←', 'Ctrl+Alt+←'], label: '向左滑动一列并聚焦' }]);
  assert.deepEqual(groupHotkeys([]), []);
});

test('overlay renders the groups, its own toggle key and an empty state', () => {
  const html = renderToStaticMarkup(createElement(HotkeyOverlay, { onDismiss() {}, hotkeys: [
    { key: 'control+alt+Slash', action: { type: 'hotkeyOverlay' } },
    { key: 'control+alt+KeyO', action: { type: 'overview' } },
  ] }));
  assert.match(html, /role="dialog"/);
  assert.match(html, /<kbd>Ctrl\+Alt\+\/<\/kbd> 或点击关闭/);
  assert.match(html, /<h2>界面<\/h2>/);
  assert.match(html, /打开概览/);
  assert.match(renderToStaticMarkup(createElement(HotkeyOverlay, { onDismiss() {}, hotkeys: null })), /当前没有生效的全局快捷键/);
});
