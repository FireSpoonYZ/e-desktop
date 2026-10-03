/** lane: ui-animation — hotkey overlay content: effective bindings grouped with Chinese labels. */

/** One effective binding as the backend registered it (normalized key, e.g. `control+alt+KeyH`). */
export interface Hotkey { key: string; action: { type: string; [field: string]: unknown } }
export interface HotkeyRow { keys: string[]; label: string }
export interface HotkeyGroup { title: string; rows: HotkeyRow[] }

type Fields = { type: string; [field: string]: unknown };

const MODIFIERS: Record<string, [number, string]> = {
  control: [0, 'Ctrl'], ctrl: [0, 'Ctrl'], alt: [1, 'Alt'], option: [1, 'Alt'], shift: [2, 'Shift'],
  super: [3, 'Win'], meta: [3, 'Win'], command: [3, 'Win'], cmd: [3, 'Win'],
};
const KEYS: Record<string, string> = {
  ArrowLeft: '←', ArrowRight: '→', ArrowUp: '↑', ArrowDown: '↓', Slash: '/', Backslash: '\\', Semicolon: ';',
  Quote: "'", Comma: ',', Period: '.', Minus: '-', Equal: '=', BracketLeft: '[', BracketRight: ']', Backquote: '`',
};
const DIRECTIONS: Record<string, string> = { left: '左', right: '右', up: '上', down: '下' };
/** Display order; unknown actions land in 其他. */
const GROUPS = ['聚焦', '移动窗口', '视口', '尺寸', '窗口', '工作区', '界面', '应用', '其他'];

/** `shift+control+alt+KeyH` → `Ctrl+Alt+Shift+H`. */
export function formatKey(key: string) {
  const parts = key.split('+').map((part) => part.trim()).filter(Boolean);
  const code = parts.pop() ?? '';
  const modifiers = parts.map((part) => MODIFIERS[part.toLowerCase()] ?? [9, part] as [number, string])
    .sort((a, b) => a[0] - b[0]).map(([, name]) => name);
  return [...modifiers, KEYS[code] ?? code.replace(/^(Key|Digit)(?=.)/, '')].join('+');
}

const text = (value: unknown) => typeof value === 'string' ? value : JSON.stringify(value);
const direction = (value: unknown) => DIRECTIONS[String(value)] ?? text(value);

/** Unknown action or command types (other lanes add them): show the type and its parameters. */
function fallback(prefix: string, value: Fields) {
  const params = Object.entries(value).filter(([name]) => name !== 'type')
    .map(([name, field]) => `${name}: ${text(field)}`).join('，');
  return `${prefix}${value.type}${params ? `（${params}）` : ''}`;
}

const pixels = (delta: unknown, more: string, less: string) =>
  Number(delta) < 0 ? `${less} ${-Number(delta)} 物理像素` : `${more} ${text(delta)} 物理像素`;

function describeCommand(command: Fields): [string, string] {
  switch (command.type) {
    case 'focusDirection': return ['聚焦', `向${direction(command.direction)}聚焦`];
    case 'focusWindow': return ['聚焦', `聚焦窗口 ${text(command.windowId)}`];
    case 'moveWindow': return ['移动窗口', `向${direction(command.direction)}移动当前窗口`];
    case 'moveWindowToPage': return ['工作区', `把窗口移到页面 ${text(command.pageId)}`];
    case 'switchPage': return ['工作区', `进入页面 ${text(command.pageId)}`];
    case 'addPage': return ['工作区', '新增/进入空页面'];
    case 'scroll': return ['视口', `滚动 ${text(command.delta)} 物理像素`];
    case 'slideColumn': return ['视口', `向${direction(command.direction)}滑动一列`];
    case 'centerFocused': return ['视口', '居中当前窗口'];
    case 'cycleWidth': return ['尺寸', '循环预设列宽'];
    case 'setColumnWidth': return ['尺寸', `当前列宽设为 ${text(command.width)} 物理像素`];
    case 'setWindowColumnWidth': return ['尺寸', `列宽设为 ${text(command.width)} 物理像素`];
    case 'adjustColumnWidth': return ['尺寸', pixels(command.delta, '增加当前列宽', '减少当前列宽')];
    case 'adjustWindowHeight': return ['尺寸', pixels(command.delta, '增加窗口高度', '减少窗口高度')];
    case 'resetWindowHeights': return ['尺寸', '当前列恢复等高'];
    case 'toggleFloating': return ['窗口', '切换浮动'];
    case 'toggleFullscreen': return ['窗口', '切换布局全屏'];
    case 'closeWindow': return ['窗口', '关闭窗口'];
    case 'refresh': return ['应用', '刷新窗口'];
    case 'enable': return ['应用', '启用平铺'];
    case 'disable': return ['应用', '暂停并还原'];
    // Commands added alongside the overlay.
    case 'consumeOrExpelWindow': return ['移动窗口', `窗口并入或移出${direction(command.direction)}侧列`];
    case 'consumeWindowIntoColumn': return ['移动窗口', '右侧列首个窗口并入当前列'];
    case 'expelWindowFromColumn': return ['移动窗口', '当前窗口移出为右侧新列'];
    case 'moveColumn': return ['移动窗口', `当前列向${direction(command.direction)}移`];
    case 'moveColumnToFirst': return ['移动窗口', '当前列移到最前'];
    case 'moveColumnToLast': return ['移动窗口', '当前列移到最后'];
    case 'swapWindow': return ['移动窗口', `与${direction(command.direction)}侧列交换窗口`];
    case 'focusColumnFirst': return ['聚焦', '聚焦第一列'];
    case 'focusColumnLast': return ['聚焦', '聚焦最后一列'];
    case 'focusWindowOrPage': return ['聚焦', `向${direction(command.direction)}聚焦窗口或页面`];
    case 'focusColumnOrMonitor': return ['聚焦', `向${direction(command.direction)}聚焦列或显示器`];
    case 'focusMonitor': return ['聚焦', `聚焦${direction(command.direction)}侧显示器`];
    case 'focusWindowPrevious': return ['聚焦', '回到上一个窗口'];
    case 'moveColumnToMonitor': return ['工作区', `当前列移到${direction(command.direction)}侧显示器`];
    case 'moveWindowToMonitor': return ['工作区', `当前窗口移到${direction(command.direction)}侧显示器`];
    case 'movePageToMonitor': return ['工作区', `当前页面移到${direction(command.direction)}侧显示器`];
    case 'focusPagePrevious': return ['工作区', '回到上一个页面'];
    case 'cycleWidthBack': return ['尺寸', '反向循环预设列宽'];
    case 'cycleWindowHeight': return ['尺寸', '循环预设窗口高度'];
    case 'maximizeColumn': return ['尺寸', '切换列最大化'];
    case 'setPageName': return ['工作区', `当前页面命名为 ${text(command.name)}`];
    case 'unsetPageName': return ['工作区', '取消页面命名'];
    case 'toggleColumnTabbedDisplay': return ['窗口', '切换标签列'];
    case 'snapViewport': return ['视口', '视图对齐到最近的列'];
    case 'screenshot': return ['应用', '框选截图'];
    case 'screenshotScreen': return ['应用', '截取当前显示器'];
    case 'screenshotWindow': return ['应用', '截取当前窗口'];
    default: return ['其他', fallback('命令 ', command)];
  }
}

function pages(delta: number) {
  if (delta === -1) return '上一页';
  if (delta === 1) return '下一页';
  return `${delta < 0 ? '向上' : '向下'}第 ${Math.abs(delta)} 页`;
}

/** [group, label] for one shortcut action. */
export function describeAction(action: Fields): [string, string] {
  switch (action.type) {
    case 'command': {
      const command = action.command;
      return command && typeof command === 'object' && typeof (command as Fields).type === 'string'
        ? describeCommand(command as Fields) : ['其他', fallback('', action)];
    }
    case 'overview': return ['界面', '打开概览'];
    case 'commands': return ['界面', '打开命令面板'];
    case 'hotkeyOverlay': return ['界面', '显示/关闭快捷键提示'];
    case 'quit': return ['应用', '还原后退出'];
    case 'page': return ['工作区', action.moveWindow ? `把当前窗口移至第 ${text(action.number)} 页` : `进入第 ${text(action.number)} 页`];
    case 'relativePage': {
      const target = pages(Number(action.delta));
      return ['工作区', action.moveWindow ? `把当前窗口移至${target}并跟随` : `切换到${target}`];
    }
    case 'scroll': return ['视口', `向${direction(action.direction)}滑动一列并聚焦`];
    default: return ['其他', fallback('', action)];
  }
}

/** Groups in a fixed order; keys bound to the same description share one row. */
export function groupHotkeys(hotkeys: readonly Hotkey[]): HotkeyGroup[] {
  const groups = new Map<string, HotkeyRow[]>();
  for (const { key, action } of hotkeys) {
    const [title, label] = describeAction(action);
    const rows = groups.get(title) ?? [];
    groups.set(title, rows);
    const row = rows.find((item) => item.label === label);
    if (row) row.keys.push(formatKey(key));
    else rows.push({ keys: [formatKey(key)], label });
  }
  return GROUPS.filter((title) => groups.has(title)).map((title) => ({ title, rows: groups.get(title)! }));
}
