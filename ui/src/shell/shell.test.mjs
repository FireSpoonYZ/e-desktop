import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';
import ts from 'typescript';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

const require = createRequire(import.meta.url);
function load(path) {
  const file = new URL(path, import.meta.url);
  const source = readFileSync(file, 'utf8');
  const { outputText } = ts.transpileModule(source, {
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
const { TopBar, PageRail } = load('./index.tsx');
const { emptySnapshot } = load('../model.ts');
const { ScrollControls } = load('./ScrollControls.tsx');
function nodes(element) {
  if (!element || typeof element !== 'object') return [];
  return [element, ...[element.props?.children].flat(Infinity).flatMap(nodes)];
}
const buttons = (element) => nodes(element).filter(({ type }) => type === 'button');
function readySnapshot() {
  const snapshot = structuredClone(emptySnapshot);
  snapshot.backend.availability = 'ready';
  snapshot.backend.capabilities.enumerate = true;
  snapshot.backend.capabilities.placement = true;
  snapshot.backend.capabilities.minimize = true;
  snapshot.monitors = [{ monitor: { id: 'm1', name: '主屏', primary: true, scaleFactor: 1 }, viewport: { width: 1000 }, activePage: 'p1', pages: [
    { id: 'p1', name: '一', columns: [], floatingWindows: [], viewportX: 0 },
    { id: 'p2', name: '二', columns: [], floatingWindows: [], viewportX: 600 },
  ] }];
  snapshot.activeMonitor = 'm1';
  return snapshot;
}

test('TopBar uses real commands and callbacks, gates enable, but always permits restoration', () => {
  const calls = [];
  const props = { snapshot: structuredClone(emptySnapshot), onCommand: (command) => calls.push(command),
    onOpenOverview: () => calls.push('overview'), onOpenCommands: () => calls.push('commands') };
  assert.equal(buttons(TopBar(props))[0].props.disabled, true);
  props.snapshot = readySnapshot();
  const controls = buttons(TopBar(props));
  assert.equal(controls[0].props.disabled, false);
  controls.forEach(({ props }) => props.onClick());
  assert.deepEqual(calls, [{ type: 'enable' }, 'overview', 'commands', { type: 'refresh' }]);
  props.snapshot.backend.capabilities.placement = false;
  assert.equal(buttons(TopBar(props))[0].props.disabled, true);
  props.snapshot.enabled = true;
  props.snapshot.backend.availability = 'unavailable';
  const restore = buttons(TopBar(props))[0];
  assert.equal(restore.props.disabled, false);
  restore.props.onClick();
  assert.deepEqual(calls.at(-1), { type: 'disable' });
  assert.equal(buttons(TopBar({ ...props, busy: true }))[0].props.disabled, true);
});

test('Shell consumes rejected native commands while retaining the parent error', async () => {
  const snapshot = readySnapshot();
  const onCommand = async () => {
    snapshot.errors.push({ code: 'operationDenied', message: '拒绝还原', windowId: null });
    throw new Error('拒绝还原');
  };
  const props = { snapshot, onCommand, onOpenOverview() {}, onOpenCommands() {} };
  await buttons(TopBar(props))[0].props.onClick();
  await buttons(PageRail(props))[0].props.onClick();
  assert.equal(snapshot.errors.length, 2);
  assert.match(nodes(TopBar(props)).find(({ props }) => props.role === 'alert').props.title, /拒绝还原/);
});

test('TopBar presents backend permission and current error without simulated windows', () => {
  const props = { snapshot: readySnapshot(), onCommand() {}, onOpenOverview() {}, onOpenCommands() {} };
  props.snapshot.backend.availability = 'permissionRequired';
  const status = nodes(TopBar(props)).find(({ props }) => props.role === 'status');
  assert.match(status.props.title, /需要系统权限/);
  props.snapshot.errors.push({ code: 'operationDenied', message: '系统拒绝操作', windowId: null });
  const alert = nodes(TopBar(props)).find(({ props }) => props.role === 'alert');
  assert.match(alert.props.title, /系统拒绝操作/);
});

test('PageRail preserves opaque monitor/page IDs, highlights current page, and handles missing monitors', () => {
  const calls = [];
  const props = { snapshot: readySnapshot(), onCommand: (command) => calls.push(command) };
  const controls = buttons(PageRail(props));
  assert.equal(controls[0].props['aria-current'], 'page');
  assert.equal(controls[1].props['aria-current'], undefined);
  controls[1].props.onClick();
  controls[2].props.onClick();
  assert.deepEqual(calls, [{ type: 'switchPage', monitorId: 'm1', pageId: 'p2' }, { type: 'addPage', monitorId: 'm1' }]);
  assert.ok(buttons(PageRail({ ...props, busy: true })).every(({ props }) => props.disabled));
  const missing = buttons(PageRail({ ...props, monitorId: 'removed' }));
  assert.equal(missing.length, 1);
  assert.equal(missing[0].props.disabled, true);
  missing[0].props.onClick();
  assert.equal(calls.length, 2);
  props.snapshot.backend.availability = 'unavailable';
  assert.ok(buttons(PageRail(props)).every(({ props }) => props.disabled));
});

test('dedicated scroll controls disable for pause, busy and no selected target', () => {
  const snapshot = readySnapshot();
  snapshot.enabled = true;
  const props = { snapshot, onCommand() {} };
  const html = renderToStaticMarkup(createElement(ScrollControls, props));
  assert.match(html, /aria-label="当前显示器横向滚动"/);
  assert.match(html, /向左滚动 160 物理像素/);
  assert.match(html, /向右滚动 160 物理像素/);
  assert.doesNotMatch(html, /disabled=""/);
  for (const blocked of [{ ...props, busy: true }, { ...props, snapshot: { ...snapshot, enabled: false } },
    { ...props, snapshot: { ...snapshot, activeMonitor: 'removed' } }]) {
    const disabled = renderToStaticMarkup(createElement(ScrollControls, blocked));
    assert.equal((disabled.match(/disabled=""/g) ?? []).length, 2);
    assert.match(disabled, /aria-disabled="true"/);
  }
  const missing = TopBar({ ...props, snapshot: { ...snapshot, activeMonitor: 'removed' },
    onOpenOverview() {}, onOpenCommands() {} });
  assert.equal(nodes(missing).find(({ props }) => props.className === 'shell-location').props.title, '无显示器 / 无页面');
});
