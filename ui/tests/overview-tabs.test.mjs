// lane: tabbed
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
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
    ? moduleUrl(new URL(`${specifier}${existsSync(new URL(`${specifier}.ts`, file)) ? '.ts' : '.tsx'}`, file).href) : import.meta.resolve(specifier)}'`);
  const url = `data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`;
  modules.set(file.href, url);
  return url;
}
const { shownTab } = await import(moduleUrl('../src/overview/tabs.ts'));
const { Overview } = await import(moduleUrl('../src/overview/index.tsx'));
const { commandActions } = await import(moduleUrl('../src/commands/actions.ts'));
const { emptySnapshot } = await import(moduleUrl('../src/model.ts'));

const rect = { x: 0, y: 0, width: 1000, height: 800 };
function snapshot(display = 'tabbed') {
  const window = (id, title) => ({ native: { id, title, appName: 'App', rect }, floating: false, fullscreen: false });
  return {
    ...emptySnapshot, enabled: true, focusedWindow: 'b', activeMonitor: 'm',
    backend: { ...emptySnapshot.backend, availability: 'ready', capabilities: { ...emptySnapshot.backend.capabilities, placement: true, focus: true } },
    windows: [window('a', 'Alpha'), window('b', 'Beta'), window('c', 'Gamma')],
    monitors: [{ monitor: { id: 'm', name: 'Monitor', scaleFactor: 1 }, viewport: rect, activePage: 'p', pages: [
      { id: 'p', name: 'Page', columns: [
        { id: 't', width: 500, windows: ['a', 'b'], display, ...(display === 'tabbed' ? { activeTab: 'b' } : {}) },
        { id: 'n', width: 500, windows: ['c'], display: 'normal' },
      ], floatingWindows: [], viewportX: 0 },
    ] }],
  };
}
function capture(props) {
  let tree;
  function Capture() { tree = Overview(props); return tree; }
  renderToStaticMarkup(createElement(Capture));
  const elements = [];
  (function visit(node) {
    if (Array.isArray(node)) return node.forEach(visit);
    if (!node?.props) return;
    elements.push(node); visit(node.props.children);
  })(tree);
  return elements;
}

test('shownTab mirrors the engine: only multi-window tabbed columns show one tab', () => {
  assert.equal(shownTab({ id: 'c', width: 1, windows: ['a', 'b'], display: 'tabbed', activeTab: 'b' }), 'b');
  assert.equal(shownTab({ id: 'c', width: 1, windows: ['a', 'b'], display: 'tabbed', activeTab: 'gone' }), 'a');
  assert.equal(shownTab({ id: 'c', width: 1, windows: ['a'], display: 'tabbed', activeTab: 'a' }), null);
  assert.equal(shownTab({ id: 'c', width: 1, windows: ['a', 'b'] }), null);
  assert.equal(shownTab({ id: 'c', width: 1, windows: ['a', 'b'], display: 'normal' }), null);
});

test('overview shows a tabbed column as its active card under a tab strip that switches tabs', () => {
  const calls = [];
  const elements = capture({ snapshot: snapshot(), onCommand: (command) => calls.push(command), onDismiss() {} });
  const cards = elements.filter((node) => node.props.className === 'overview-window');
  assert.deepEqual(cards.map((node) => node.key), ['b', 'c']);
  const tabs = elements.filter((node) => node.props.role === 'tab');
  assert.deepEqual(tabs.map((node) => [node.props.children, node.props['aria-selected']]), [['Alpha', false], ['Beta', true]]);
  assert.equal(tabs[0].props.title, '标签 1/2 · Alpha');
  tabs[0].props.onClick();
  assert.deepEqual(calls, [{ type: 'focusWindow', windowId: 'a' }]);
  const stacked = capture({ snapshot: snapshot('normal'), onCommand() {}, onDismiss() {} });
  assert.deepEqual(stacked.filter((node) => node.props.className === 'overview-window').map((node) => node.key), ['a', 'b', 'c']);
  assert.equal(stacked.filter((node) => node.props.role === 'tab').length, 0);
});

test('command palette toggles the focused tiled column display', () => {
  const [action] = commandActions(snapshot(), 'tabbed column');
  assert.deepEqual(action.command, { type: 'toggleColumnTabbedDisplay' });
  assert.equal(action.disabled, false);
  const floating = snapshot();
  floating.windows[1].floating = true;
  assert.ok(commandActions(floating, 'tabbed column')[0].disabled);
  assert.ok(commandActions({ ...snapshot(), enabled: false }, '标签列')[0].disabled);
});
