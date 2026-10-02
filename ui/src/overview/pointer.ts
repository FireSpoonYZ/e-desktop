import type { Command, Page, Rect, Snapshot, WindowId } from '../model';

/** Niri's half-size overview, reduced further when viewing a larger display. */
export const overviewScale = (scaleFactor: number, viewportWidth = 1, availableWidth = Infinity) =>
  Math.min(.5 / Math.max(1, scaleFactor), availableWidth / Math.max(1, viewportWidth));

/** Keep column widths and scroll offsets in the same physical-to-CSS scale. */
export function pageGeometry(page: Page, viewport: Rect, scale: number) {
  const leading = Math.max(0, -page.viewportX);
  return {
    width: viewport.width * scale,
    height: viewport.height * scale,
    leading: leading * scale,
    stripWidth: Math.max(viewport.width + Math.max(0, page.viewportX),
      leading + page.columns.reduce((total, column) => total + column.width, 0)) * scale,
  };
}
export const canInteract = (snapshot: Snapshot, busy: boolean) => !busy && snapshot.enabled
  && snapshot.backend.availability === 'ready' && snapshot.backend.capabilities.placement;

export function moveCommand(snapshot: Snapshot, busy: boolean, windowId: WindowId, pageId: string): Command | null {
  if (!canInteract(snapshot, busy) || !snapshot.windows.some((window) => window.native.id === windowId)) return null;
  const pages = snapshot.monitors.flatMap((monitor) => monitor.pages);
  const source = pages.find((page) => page.floatingWindows.includes(windowId)
    || page.columns.some((column) => column.windows.includes(windowId)));
  return source && source.id !== pageId && pages.some((page) => page.id === pageId)
    ? { type: 'moveWindowToPage', windowId, pageId } : null;
}

/** x/y are CSS pixels inside the target page's visible workspace, not the desktop.
 * viewportX keeps manual overview scrolling in the same column hit geometry; supplying
 * it disables desktop edge/top hot zones so the visible edge still hits its column. */
export function dropCommand(snapshot: Snapshot, busy: boolean, windowId: WindowId, pageId: string,
  x: number, y: number, scrollLeft: number, scale: number): Command | null {
  if (!canInteract(snapshot, busy)) return null;
  const window = snapshot.windows.find((window) => window.native.id === windowId);
  if (!window) return null;
  if (window.floating) return moveCommand(snapshot, busy, windowId, pageId);
  const pages = snapshot.monitors.flatMap((monitor) => monitor.pages);
  if (!pages.some((page) => page.columns.some((column) => column.windows.includes(windowId)))) return null;
  const monitor = snapshot.monitors.find((monitor) => monitor.pages.some((page) => page.id === pageId));
  const page = monitor?.pages.find((page) => page.id === pageId);
  if (!monitor || !page || ![x, y, scrollLeft, scale].every(Number.isFinite) || scale <= 0 || scrollLeft < 0) return null;
  const { viewport } = monitor;
  if (x < 0 || y < 0 || x >= viewport.width * scale || y >= viewport.height * scale) return null;
  const physicalX = viewport.x + Math.min(viewport.width - 1, Math.round(x / scale));
  const physicalY = viewport.y + Math.min(viewport.height - 1, Math.round(y / scale));
  const viewportX = Math.round(scrollLeft / scale - Math.max(0, -page.viewportX));
  if (![physicalX, physicalY, viewportX].every((value) => Number.isInteger(value)
    && value >= -2147483648 && value <= 2147483647)) return null;
  return { type: 'dropWindow', windowId, pageId, x: physicalX, y: physicalY, viewportX };
}

export function sizingTarget(snapshot: Snapshot, windowId: WindowId) {
  const window = snapshot.windows.find((window) => window.native.id === windowId);
  if (!window || window.floating || window.fullscreen) return null;
  for (const monitor of snapshot.monitors) for (const page of monitor.pages) {
    const column = page.columns.find((column) => column.windows.includes(windowId));
    if (column) return { monitor, page, column, scale: overviewScale(monitor.monitor.scaleFactor) };
  }
  return null;
}

export function widthCommand(snapshot: Snapshot, busy: boolean, windowId: WindowId, width: number): Command | null {
  const target = sizingTarget(snapshot, windowId);
  return canInteract(snapshot, busy) && target && Number.isInteger(width) && width > 0 && width <= 0xffffffff
    ? { type: 'setWindowColumnWidth', windowId, width: Math.min(width, target.monitor.viewport.width) } : null;
}

export function physicalWidth(width: number, delta: number, scale: number, maximum: number) {
  return Math.max(1, Math.min(maximum, Math.round(width + delta / scale)));
}

/** One transient gesture; the owner cancels on unmount/stale target. */
function trackPointer(host: Window, pointerId: number, preview: (event: PointerEvent) => void,
  finish: (event: PointerEvent | null) => void) {
  let active = true;
  const end = (value: PointerEvent | null) => {
    if (!active) return;
    active = false;
    host.removeEventListener('pointermove', move);
    host.removeEventListener('pointerup', up);
    host.removeEventListener('pointercancel', cancelPointer);
    host.removeEventListener('keydown', key, true);
    host.removeEventListener('blur', cancel);
    finish(value);
  };
  const move = (event: PointerEvent) => {
    if (event.pointerId !== pointerId) return;
    preview(event);
  };
  const up = (event: PointerEvent) => {
    if (event.pointerId !== pointerId) return;
    move(event);
    end(event);
  };
  const cancel = () => end(null);
  const cancelPointer = (event: PointerEvent) => { if (event.pointerId === pointerId) cancel(); };
  const key = (event: KeyboardEvent) => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); cancel(); }
  };
  host.addEventListener('pointermove', move);
  host.addEventListener('pointerup', up);
  host.addEventListener('pointercancel', cancelPointer);
  host.addEventListener('keydown', key, true);
  host.addEventListener('blur', cancel);
  return cancel;
}

export function trackResize(host: Window, pointerId: number, startX: number, width: number, scale: number,
  maximum: number, preview: (width: number) => void, finish: (width: number | null) => void) {
  let current = width;
  return trackPointer(host, pointerId, (event) => {
    current = physicalWidth(width, event.clientX - startX, scale, maximum);
    preview(current);
  }, (event) => finish(event && current !== width ? current : null));
}

export function trackDrag(host: Window, pointerId: number, startX: number, startY: number,
  preview: (event: PointerEvent) => void, finish: (event: PointerEvent | null) => void) {
  let moved = false;
  return trackPointer(host, pointerId, (event) => {
    moved ||= Math.hypot(event.clientX - startX, event.clientY - startY) >= 5;
    if (moved) preview(event);
  }, (event) => finish(moved ? event : null));
}
