// lane: layout-options. Palette entries and page-rail labels for named pages.
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';
import ts from 'typescript';

const require = createRequire(import.meta.url);
function load(path) {
  const file = new URL(path, import.meta.url);
  const { outputText } = ts.transpileModule(readFileSync(file, 'utf8'), {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  });
  const exports = {};
  new Function('require', 'exports', outputText)((id) => {
    if (id.endsWith('.css')) return {};
    if (!id.startsWith('.')) return require(id);
    const tsx = new URL(`${id}.tsx`, file);
    return load(existsSync(tsx) ? tsx : new URL(`${id}.ts`, file));
  }, exports);
  return exports;
}
const { commandActions } = load('./actions.ts');
const { PageRail } = load('../shell/index.tsx');
const { emptySnapshot } = load('../model.ts');

const rect = { x: 0, y: 0, width: 1000, height: 800 };
const snapshot = {
  ...emptySnapshot, enabled: true,
  backend: { ...emptySnapshot.backend, availability: 'ready', capabilities: { ...emptySnapshot.backend.capabilities, placement: true, focus: true, enumerate: true } },
  focusedWindow: 'w1', activeMonitor: 'm1', namedPages: ['p2'],
  windows: [{ native: { id: 'w1', title: 'w1', appName: 'Writer', rect }, floating: false, fullscreen: false }],
  monitors: [{ monitor: { id: 'm1', name: 'Main', scaleFactor: 1 }, viewport: rect, activePage: 'p1', pages: [
    { id: 'p1', name: 'Desktop 1', viewportX: 0, columns: [{ id: 'c1', width: 600, windows: ['w1'] }], floatingWindows: [] },
    { id: 'p2', name: '聊天', viewportX: 0, columns: [], floatingWindows: [] },
  ] }],
};

test('layout option actions carry their commands and follow the sizing gate', () => {
  const types = ['cycleWidthBack', 'cycleWindowHeight', 'maximizeColumn'];
  const sizes = (state) => commandActions(state, '').filter(({ command }) => types.includes(command.type));
  assert.deepEqual(sizes(snapshot).map(({ command }) => command.type), types);
  assert.ok(sizes(snapshot).every(({ disabled }) => !disabled));
  const floating = { ...snapshot, windows: snapshot.windows.map((w) => ({ ...w, floating: true })) };
  assert.ok(sizes(floating).every(({ disabled }) => disabled));
  assert.equal(commandActions(snapshot, 'maximize')[0].command.type, 'maximizeColumn');
});

test('page naming takes the name from the query prefix and named pages are searchable', () => {
  assert.deepEqual(commandActions(snapshot, '命名 工作 区')[0].command, { type: 'setPageName', name: '工作 区' });
  assert.deepEqual(commandActions(snapshot, 'name Chat')[0].command, { type: 'setPageName', name: 'Chat' });
  assert.ok(!commandActions(snapshot, 'chat').some(({ command }) => command.type === 'setPageName'));
  assert.deepEqual(commandActions(snapshot, 'unset page')[0].command, { type: 'unsetPageName' });
  assert.ok(commandActions({ ...snapshot, enabled: false }, 'name Chat')[0].disabled);
  assert.deepEqual(commandActions(snapshot, '进入 聊天')[0].command, { type: 'switchPage', monitorId: 'm1', pageId: 'p2' });
});

test('the page rail shows names for named pages and numbers for the rest', () => {
  const rail = PageRail({ snapshot, onCommand() {} });
  const pages = rail.props.children[0].props.children[0];
  assert.deepEqual(pages.map((button) => button.props.children), [1, '聊天']);
  const unnamed = PageRail({ snapshot: { ...snapshot, namedPages: undefined }, onCommand() {} });
  assert.deepEqual(unnamed.props.children[0].props.children[0].map((button) => button.props.children), [1, 2]);
});
