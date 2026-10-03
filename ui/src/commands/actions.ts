import type { Command, Snapshot } from '../model';
import { scrollTarget } from '../shell/scroll';

export interface Action { id: string; label: string; detail: string; command: Command; disabled: boolean }

export function commandActions(snapshot: Snapshot, query: string): Action[] {
  const { capabilities, availability } = snapshot.backend;
  const managed = snapshot.enabled && availability === 'ready' && capabilities.placement;
  const focused = managed && snapshot.focusedWindow !== null;
  const window = snapshot.windows.find(({ native }) => native.id === snapshot.focusedWindow);
  const sizable = focused && !!window && !window.floating && !window.fullscreen
    && snapshot.monitors.some(({ pages, activePage }) => pages.some((page) => page.id === activePage
      && page.columns.some(({ windows }) => windows.includes(window.native.id))));
  const target = scrollTarget(snapshot);
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
  add('切换窗口宽度 Width', { type: 'cycleWidth' }, sizable);
  add('加宽当前列 Width +50', { type: 'adjustColumnWidth', delta: 50 }, sizable, '当前聚焦列 · +50 物理像素');
  add('减宽当前列 Width -50', { type: 'adjustColumnWidth', delta: -50 }, sizable, '当前聚焦列 · -50 物理像素');
  add('增高当前窗口 Height +50', { type: 'adjustWindowHeight', delta: 50 }, sizable, '当前聚焦窗口 · +50 物理像素');
  add('减高当前窗口 Height -50', { type: 'adjustWindowHeight', delta: -50 }, sizable, '当前聚焦窗口 · -50 物理像素');
  add('恢复当前列等高 Reset heights', { type: 'resetWindowHeights' }, sizable, '当前聚焦列 · 恢复等高分配');
  for (const [delta, label] of [[-160, '左'], [160, '右']] as const) {
    add(`向${label}滚动 Scroll`, { type: 'scroll', monitorId: snapshot.activeMonitor ?? '', delta }, !!target,
      `${target?.monitor.name ?? '无可用显示器'} · ${delta} 物理像素`);
  }
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
  // lane: layout-actions
  const tiled = focused && !!window && !window.floating;
  const focusable = managed && capabilities.focus;
  for (const [direction, label] of [['left', '左'], ['right', '右']] as const) {
    add(`窗口并入或移出${label}侧列 Consume or expel ${direction}`, { type: 'consumeOrExpelWindow', direction }, tiled);
    add(`当前列${label}移 Move column ${direction}`, { type: 'moveColumn', direction }, tiled);
    add(`与${label}侧列交换窗口 Swap window ${direction}`, { type: 'swapWindow', direction }, tiled);
    add(`向${label}聚焦列或显示器 Focus column or monitor ${direction}`, { type: 'focusColumnOrMonitor', direction }, focusable);
  }
  add('右侧列首个窗口并入当前列 Consume into column', { type: 'consumeWindowIntoColumn' }, tiled);
  add('当前窗口移出为右侧新列 Expel from column', { type: 'expelWindowFromColumn' }, tiled);
  add('当前列移到最前 Move column to first', { type: 'moveColumnToFirst' }, tiled);
  add('当前列移到最后 Move column to last', { type: 'moveColumnToLast' }, tiled);
  add('聚焦第一列 Focus first column', { type: 'focusColumnFirst' }, focusable);
  add('聚焦最后一列 Focus last column', { type: 'focusColumnLast' }, focusable);
  for (const [direction, label] of [['up', '上'], ['down', '下']] as const) {
    add(`向${label}聚焦窗口或页面 Focus window or page ${direction}`, { type: 'focusWindowOrPage', direction }, managed);
  }
  for (const [direction, label] of [['left', '左侧'], ['right', '右侧'], ['up', '上方'], ['down', '下方']] as const) {
    add(`聚焦${label}显示器 Focus monitor ${direction}`, { type: 'focusMonitor', direction }, managed);
    add(`当前列移到${label}显示器 Move column to monitor ${direction}`, { type: 'moveColumnToMonitor', direction }, focused);
    add(`当前窗口移到${label}显示器 Move window to monitor ${direction}`, { type: 'moveWindowToMonitor', direction }, focused);
    add(`当前页面移到${label}显示器 Move page to monitor ${direction}`, { type: 'movePageToMonitor', direction }, managed);
  }
  add('聚焦上一个窗口 Previous window', { type: 'focusWindowPrevious' }, focusable);
  add('回到上一个页面 Previous page', { type: 'focusPagePrevious' }, managed);
  // lane: layout-options
  add('反向切换列宽 Width back', { type: 'cycleWidthBack' }, sizable);
  add('切换窗口高度 Height presets', { type: 'cycleWindowHeight' }, sizable, '当前聚焦窗口 · 循环预设高度');
  add('切换列最大化 Maximize column', { type: 'maximizeColumn' }, sizable, '当前聚焦列 · 视口全宽 / 恢复原宽度');
  const pageName = /^\s*(?:命名|name)\s+(.+)$/iu.exec(query)?.[1].trim();
  if (pageName) add(`命名当前页面为 ${pageName} Name page`, { type: 'setPageName', name: pageName }, managed, '当前显示器的活动页面');
  add('取消页面命名 Unset page name', { type: 'unsetPageName' }, managed, '当前显示器的活动页面');
  // lane: tabbed
  add('切换标签列 Tabbed column', { type: 'toggleColumnTabbedDisplay' }, focused && !!window && !window.floating,
    '当前聚焦列 · 标签显示 / 纵向堆叠');
  // lane: input-gestures
  add('视图对齐到最近的列 Snap to column', { type: 'snapViewport', monitorId: target?.monitor.id ?? '', delta: 0 }, !!target,
    `${target?.monitor.name ?? '无可用显示器'} · 对齐列边缘并聚焦该列`);
  // lane: rules-spawn-screenshot
  const screenshots = snapshot.backend.kind === 'windows';
  add('截图 Screenshot', { type: 'screenshot' }, screenshots, '冻结画面后框选区域 · 保存 PNG 并复制');
  add('截取当前显示器 Screenshot monitor', { type: 'screenshotScreen' }, screenshots && snapshot.activeMonitor !== null,
    '活动显示器 · 保存 PNG 并复制');
  add('截取当前窗口 Screenshot window', { type: 'screenshotWindow' }, screenshots && snapshot.focusedWindow !== null,
    '焦点窗口 · 保存 PNG 并复制');
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
