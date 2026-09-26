import type { Command, Snapshot } from '../model';

export interface Action { id: string; label: string; detail: string; command: Command; disabled: boolean }

export function commandActions(snapshot: Snapshot, query: string): Action[] {
  const { capabilities, availability } = snapshot.backend;
  const managed = snapshot.enabled && availability === 'ready' && capabilities.placement;
  const focused = managed && snapshot.focusedWindow !== null;
  const actions: Action[] = snapshot.windows.map(({ native }) => ({
    id: `window:${native.id}`, label: native.title || '无标题窗口', detail: native.appName || '未知应用',
    command: { type: 'focusWindow', windowId: native.id }, disabled: !managed || !capabilities.focus,
  }));
  const add = (label: string, command: Command, enabled: boolean, detail = '布局命令') =>
    actions.push({ id: JSON.stringify(command), label, detail, command, disabled: !enabled });
  add('刷新窗口 Refresh', { type: 'refresh' }, capabilities.enumerate);
  add(snapshot.enabled ? '暂停管理 Disable' : '启用管理 Enable', { type: snapshot.enabled ? 'disable' : 'enable' },
    snapshot.enabled || (availability === 'ready' && capabilities.placement));
  for (const [direction, label] of [['left', '左'], ['right', '右'], ['up', '上'], ['down', '下']] as const) {
    add(`向${label}聚焦 Focus ${direction}`, { type: 'focusDirection', direction }, focused && capabilities.focus);
    add(`向${label}移动窗口 Move ${direction}`, { type: 'moveWindow', direction }, focused);
  }
  add('切换窗口宽度 Width', { type: 'cycleWidth' }, focused);
  add('居中当前窗口 Center', { type: 'centerFocused' }, focused);
  add('切换浮动 Floating', { type: 'toggleFloating' }, focused);
  add('切换布局全屏 Fullscreen', { type: 'toggleFullscreen' }, focused);
  if (snapshot.focusedWindow) add('关闭当前窗口 Close', { type: 'closeWindow', windowId: snapshot.focusedWindow }, focused && capabilities.close);
  for (const { monitor, pages } of snapshot.monitors) {
    add(`新增页面 · ${monitor.name}`, { type: 'addPage', monitorId: monitor.id }, managed);
    for (const page of pages) {
      add(`进入 ${page.name}`, { type: 'switchPage', monitorId: monitor.id, pageId: page.id }, managed, monitor.name);
      if (snapshot.focusedWindow) add(`移动当前窗口到 ${page.name}`, {
        type: 'moveWindowToPage', windowId: snapshot.focusedWindow, pageId: page.id,
      }, focused, monitor.name);
    }
  }
  const words = query.normalize('NFKC').toLocaleLowerCase().trim().split(/\s+/).filter(Boolean);
  return actions.filter(({ label, detail }) => {
    const text = `${label} ${detail}`.normalize('NFKC').toLocaleLowerCase();
    return words.every((word) => text.includes(word));
  });
}

export function paletteKey(key: string, composing: boolean, keyCode: number): 'next' | 'previous' | 'execute' | 'dismiss' | null {
  if (composing || keyCode === 229) return null;
  return ({ ArrowDown: 'next', ArrowUp: 'previous', Enter: 'execute', Escape: 'dismiss' } as const)[key as 'Enter'] ?? null;
}

export function nextSelection(current: number, step: number, count: number): number {
  return count ? (current + step + count) % count : 0;
}
