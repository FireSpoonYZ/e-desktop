export type MonitorId = string;
export type PageId = string;
export type ColumnId = string;
export type WindowId = string;

/** All geometry is physical screen pixels, not CSS pixels. */
export interface Rect { x: number; y: number; width: number; height: number }
export interface Monitor {
  id: MonitorId;
  name: string;
  bounds: Rect;
  workArea: Rect;
  scaleFactor: number;
  primary: boolean;
}
export interface NativeWindow {
  id: WindowId;
  title: string;
  appName: string;
  processId: number;
  monitorId: MonitorId;
  rect: Rect;
  minimized: boolean;
  minimizedByManager: boolean;
  resizable: boolean;
}
export interface SystemSnapshot {
  monitors: Monitor[];
  windows: NativeWindow[];
  focusedWindow: WindowId | null;
}
export interface WindowState { native: NativeWindow; floating: boolean; fullscreen: boolean }
/** lane: tabbed. niri column display: stacked rows or one shown tab. */
export type ColumnDisplay = 'normal' | 'tabbed';
export interface Column {
  id: ColumnId; width: number; windows: WindowId[];
  /** lane: tabbed. Absent means normal. */
  display?: ColumnDisplay;
  /** lane: tabbed. The window a tabbed column shows. */
  activeTab?: WindowId;
}
export interface Page {
  id: PageId;
  name: string;
  columns: Column[];
  floatingWindows: WindowId[];
  viewportX: number;
}
export interface MonitorState {
  monitor: Monitor;
  pages: Page[];
  activePage: PageId;
  viewport: Rect;
}
export type BackendKind = 'windows' | 'x11' | 'macOs' | 'unsupported';
export type BackendAvailability = 'ready' | 'notImplemented' | 'permissionRequired' | 'unsupportedSession' | 'unavailable';
export interface Capabilities {
  enumerate: boolean;
  placement: boolean;
  focus: boolean;
  close: boolean;
  minimize: boolean;
  clipping: boolean;
  globalShortcuts: boolean;
  focusFollowsPointer: boolean;
}
export interface BackendStatus {
  kind: BackendKind;
  availability: BackendAvailability;
  capabilities: Capabilities;
  message: string;
}
export interface Snapshot {
  enabled: boolean;
  backend: BackendStatus;
  monitors: MonitorState[];
  windows: WindowState[];
  focusedWindow: WindowId | null;
  activeMonitor: MonitorId | null;
  errors: AppError[];
  /** Monitors whose bar stays visible and reserves space; the others auto-hide. */
  pinnedBars: MonitorId[];
  barsAutohide: boolean;
  /** 平铺间距，逻辑像素。 */
  gaps: number;
  /** 布局动画基准时长（毫秒）；0 禁用。概览开合用 overviewAnimationMs。 */
  animationDurationMs: number;
  /** Monitors paused while a foreign window covers the full display. */
  suspendedMonitors: MonitorId[];
  // lane: layout-options
  /** 带名称的页面（名称在 Page.name 中）；为空时也保留。 */
  namedPages?: PageId[];
  /** lane: ui-animation — 概览开合动画时长（毫秒，已套用 animations.overviewOpenClose）；0 禁用。 */
  overviewAnimationMs: number;
}
export type Direction = 'left' | 'right' | 'up' | 'down';
export type Command =
  | { type: 'refresh' }
  | { type: 'enable' }
  | { type: 'disable' }
  | { type: 'focusWindow'; windowId: WindowId }
  | { type: 'focusDirection'; direction: Direction }
  | { type: 'switchPage'; monitorId: MonitorId; pageId: PageId }
  | { type: 'addPage'; monitorId: MonitorId }
  | { type: 'moveWindowToPage'; windowId: WindowId; pageId: PageId }
  /** viewportX overrides hit geometry for a page-targeted overview drop only. */
  | { type: 'dropWindow'; windowId: WindowId; x: number; y: number; pageId?: PageId; viewportX?: number }
  | { type: 'moveWindow'; direction: Direction }
  | { type: 'cycleWidth' }
  | { type: 'setColumnWidth'; width: number }
  | { type: 'setWindowColumnWidth'; windowId: WindowId; width: number }
  | { type: 'adjustColumnWidth'; delta: number }
  | { type: 'adjustWindowHeight'; delta: number }
  | { type: 'resetWindowHeights' }
  | { type: 'centerFocused' }
  | { type: 'scroll'; monitorId: MonitorId; delta: number }
  | { type: 'toggleFloating' }
  | { type: 'toggleFullscreen' }
  | { type: 'closeWindow'; windowId: WindowId }
  // lane: layout-actions
  | { type: 'consumeOrExpelWindow'; direction: Direction }
  | { type: 'consumeWindowIntoColumn' }
  | { type: 'expelWindowFromColumn' }
  | { type: 'moveColumn'; direction: Direction }
  | { type: 'moveColumnToFirst' }
  | { type: 'moveColumnToLast' }
  | { type: 'swapWindow'; direction: Direction }
  | { type: 'focusColumnFirst' }
  | { type: 'focusColumnLast' }
  | { type: 'focusWindowOrPage'; direction: Direction }
  | { type: 'focusColumnOrMonitor'; direction: Direction }
  | { type: 'focusMonitor'; direction: Direction }
  | { type: 'moveColumnToMonitor'; direction: Direction }
  | { type: 'moveWindowToMonitor'; direction: Direction }
  | { type: 'movePageToMonitor'; direction: Direction }
  | { type: 'focusWindowPrevious' }
  | { type: 'focusPagePrevious' }
  // lane: layout-options
  | { type: 'cycleWidthBack' }
  | { type: 'cycleWindowHeight' }
  | { type: 'maximizeColumn' }
  | { type: 'setPageName'; name: string }
  | { type: 'unsetPageName' }
  /** lane: tabbed */
  | { type: 'toggleColumnTabbedDisplay' }
  // lane: input-gestures
  | { type: 'dragViewport'; monitorId: MonitorId; delta: number }
  | { type: 'snapViewport'; monitorId: MonitorId; delta: number };
export type ErrorCode = 'notImplemented' | 'unsupportedSession' | 'backendUnavailable' | 'permissionRequired' | 'windowGone' | 'operationDenied' | 'invalidCommand';
export interface AppError { code: ErrorCode; message: string; windowId: WindowId | null }
export type NativeAction =
  | { type: 'placement'; windowId: WindowId; rect: Rect; clip: Rect | null; minimized: boolean }
  | { type: 'focus'; windowId: WindowId }
  | { type: 'close'; windowId: WindowId }
  | { type: 'restore'; windowId: WindowId };
export interface Transition { snapshot: Snapshot; actions: NativeAction[] }

export type OnCommand = (command: Command) => void | Promise<void>;
export interface ControlProps { snapshot: Snapshot; onCommand: OnCommand; busy?: boolean; busyCommand?: Command['type'] }
export interface TopBarProps extends ControlProps {
  onOpenOverview: () => void;
  onOpenCommands: () => void;
  onOpenTerminals?: () => void;
  onQuit?: () => void;
  onTogglePin?: () => void;
}
export interface PageRailProps extends ControlProps { monitorId?: MonitorId }
export interface OverviewProps extends ControlProps { onDismiss: () => void }
export interface CommandPaletteProps extends ControlProps { onDismiss: () => void }

/** Honest initial state: no invented windows, permissions, or native capabilities. */
export const emptySnapshot: Snapshot = {
  enabled: false,
  backend: {
    kind: 'unsupported',
    availability: 'notImplemented',
    capabilities: {
      enumerate: false, placement: false, focus: false, close: false,
      minimize: false, clipping: false, globalShortcuts: false, focusFollowsPointer: false,
    },
    message: 'Native backend is not implemented in this baseline.',
  },
  monitors: [], windows: [], focusedWindow: null, activeMonitor: null, errors: [],
  pinnedBars: [], barsAutohide: false, gaps: 0, animationDurationMs: 0,
  suspendedMonitors: [],
  overviewAnimationMs: 0, // lane: ui-animation
};
