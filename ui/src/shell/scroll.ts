import type { MonitorState, OnCommand, Snapshot } from '../model';

/** Never substitute the primary display for a missing or unselected target. */
export function selectedMonitor(snapshot: Snapshot): MonitorState | undefined {
  return snapshot.monitors.find(({ monitor }) => monitor.id === snapshot.activeMonitor);
}

export function scrollTarget(snapshot: Snapshot, busy = false): MonitorState | undefined {
  const monitor = selectedMonitor(snapshot);
  return !busy && snapshot.enabled && snapshot.backend.availability === 'ready'
    && snapshot.backend.capabilities.placement
    && monitor?.pages.some(({ id }) => id === monitor.activePage)
    && Number.isFinite(monitor.monitor.scaleFactor) && monitor.monitor.scaleFactor > 0
    && Number.isFinite(monitor.viewport.width) && monitor.viewport.width > 0 ? monitor : undefined;
}

/** Pixel/line deltas are logical pixels; a page is the physical viewport width. */
export function wheelPixels(event: Pick<WheelEvent, 'deltaX' | 'deltaY' | 'deltaMode' | 'ctrlKey'>,
  monitor: MonitorState, lineHeight: number): number {
  if (event.ctrlKey) return 0; // Trackpad pinch is delivered as ctrl-wheel.
  const delta = Math.abs(event.deltaX) > Math.abs(event.deltaY) ? event.deltaX : event.deltaY;
  const unit = event.deltaMode === 0 ? monitor.monitor.scaleFactor
    : event.deltaMode === 1 ? lineHeight * monitor.monitor.scaleFactor
      : event.deltaMode === 2 ? monitor.viewport.width : 0;
  const pixels = delta * unit;
  return Number.isFinite(pixels) ? pixels : 0;
}

// Batch bursts, retain subpixel motion, and allow only one native request at a time.
export function createScrollDispatcher(getTarget: () => MonitorState | undefined, onCommand: OnCommand) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let targetKey = '';
  let delta = 0;
  let pending = false;
  let disposed = false;
  let revision = 0;
  const key = (target: MonitorState) => JSON.stringify([
    target.monitor.id, target.activePage, target.monitor.scaleFactor, target.viewport.width,
  ]);
  const cancel = () => {
    revision++;
    clearTimeout(timer);
    timer = undefined;
    delta = 0;
    targetKey = '';
  };
  const flush = async () => {
    timer = undefined;
    if (disposed || pending) return;
    const target = getTarget();
    if (!target || key(target) !== targetKey) { cancel(); return; }
    const pixels = Math.trunc(delta);
    delta -= pixels;
    if (!pixels) return;
    pending = true;
    const requestRevision = revision;
    try { await onCommand({ type: 'scroll', monitorId: target.monitor.id, delta: pixels }); }
    catch {
      if (revision === requestRevision) cancel(); // App retains the visible IPC error.
    } finally {
      pending = false;
      if (!disposed && timer === undefined && Math.trunc(delta)) void flush();
    }
  };
  return {
    push(pixels: number) {
      const target = getTarget();
      if (disposed || !target || !Number.isFinite(pixels) || !pixels) return false;
      if (key(target) !== targetKey) { cancel(); targetKey = key(target); }
      delta = Math.max(-2147483648, Math.min(2147483647, delta + pixels));
      timer ??= setTimeout(() => { void flush(); }, 80);
      return true;
    },
    cancel,
    dispose() { disposed = true; cancel(); },
  };
}
