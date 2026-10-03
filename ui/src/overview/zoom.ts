import { useLayoutEffect, useRef, useState } from 'react';
import type { MonitorState, Rect, Snapshot } from '../model';
import type { PreviewSlot } from './previews';

/** 与 Rust `animation::spring` 相同：临界阻尼弹簧（ω = 6 / 时长）加结尾 Hermite 修正，
 * 按配置时长准确停在终点且速度为 0。返回 [位置, 速度]。 */
export function spring(from: number, to: number, velocity: number, elapsed: number, duration: number): [number, number] {
  if (elapsed <= 0) return [from, velocity];
  if (elapsed >= duration) return [to, 0];
  const omega = 6 / duration;
  const displacement = from - to;
  const coefficient = velocity + omega * displacement;
  const decay = Math.exp(-omega * elapsed);
  const endDecay = Math.exp(-omega * duration);
  const residual = (displacement + coefficient * duration) * endDecay;
  const endVelocity = (velocity - omega * coefficient * duration) * endDecay;
  const t = elapsed / duration;
  return [
    to + (displacement + coefficient * elapsed) * decay
      - residual * (3 * t * t - 2 * t * t * t) - duration * endVelocity * (t * t * t - t * t),
    (velocity - omega * coefficient * elapsed) * decay
      - residual * (6 * t - 6 * t * t) / duration - endVelocity * (3 * t * t - 2 * t),
  ];
}

/** 零初速度的弹簧进度（0→1），与时长无关；夹在 [0, 1]，非有限值视为结束。 */
export function springProgress(t: number) {
  if (!Number.isFinite(t)) return 1;
  return spring(0, 1, 0, Math.min(1, Math.max(0, t)), 1)[0];
}

/** Rust `f64::round` 半数远离 0；`Math.round` 半数朝 +∞。 */
function roundAway(value: number) {
  return Math.sign(value) * Math.round(Math.abs(value));
}

/** t≤0 与 t≥1 取端点原值，不经舍入。 */
export function lerpRect(from: Rect, to: Rect, t: number): Rect {
  if (!(t > 0)) return from;
  if (t >= 1) return to;
  const eased = springProgress(t);
  const n = (start: number, end: number) => roundAway(start + (end - start) * eased);
  return { x: n(from.x, to.x), y: n(from.y, to.y), width: n(from.width, to.width), height: n(from.height, to.height) };
}

/** 桌面物理像素减去概览客户区（work area）原点。 */
export function desktopRect(rect: Rect, origin: Pick<Rect, 'x' | 'y'>): Rect {
  return { x: rect.x - origin.x, y: rect.y - origin.y, width: rect.width, height: rect.height };
}

export function clientArea(size: Pick<Rect, 'width' | 'height'>): Rect {
  return { x: 0, y: 0, width: size.width, height: size.height };
}

export function clipRect(rect: Rect, bounds: Rect): Rect | null {
  const values = [rect.x, rect.y, rect.width, rect.height, bounds.x, bounds.y, bounds.width, bounds.height];
  if (!values.every(Number.isFinite)) return null;
  const x = Math.max(rect.x, bounds.x);
  const y = Math.max(rect.y, bounds.y);
  const right = Math.min(rect.x + rect.width, bounds.x + bounds.width);
  const bottom = Math.min(rect.y + rect.height, bounds.y + bounds.height);
  return right > x && bottom > y ? { x, y, width: right - x, height: bottom - y } : null;
}

export interface ZoomMove { windowId: string; from: Rect; to: Rect }

function finiteRect(rect: Rect | undefined): rect is Rect {
  return !!rect && [rect.x, rect.y, rect.width, rect.height].every(Number.isFinite) && rect.width > 0 && rect.height > 0;
}

/** 活动页窗口在桌面矩形与槽位之间移动；其余窗口打开时停在终点，关闭时不推送。 */
export function zoomPlan(
  slots: readonly PreviewSlot[],
  nativeRects: ReadonlyMap<string, Rect>,
  activeIds: ReadonlySet<string>,
  origin: Pick<Rect, 'x' | 'y'>,
  closing: boolean,
  shown?: ReadonlyMap<string, Rect>,
): { moves: ZoomMove[]; rest: PreviewSlot[] } {
  const moves: ZoomMove[] = [];
  const rest: PreviewSlot[] = [];
  for (const slot of slots) {
    const native = nativeRects.get(slot.windowId);
    const from = closing ? shown?.get(slot.windowId) ?? slot.rect : native && desktopRect(native, origin);
    if (activeIds.has(slot.windowId) && finiteRect(native) && finiteRect(slot.rect) && finiteRect(from)) {
      moves.push({ windowId: slot.windowId, from, to: closing ? desktopRect(native, origin) : slot.rect });
    } else if (!closing) rest.push(slot);
  }
  return { moves, rest };
}

/** 动画矩形保持完整目标，clip 只取与客户区的交集；完全在外则隐藏。 */
export function zoomSlots(moves: readonly ZoomMove[], t: number, client: Rect, rest: readonly PreviewSlot[]): PreviewSlot[] {
  const animated: PreviewSlot[] = [];
  for (const move of moves) {
    const rect = lerpRect(move.from, move.to, t);
    const clip = clipRect(rect, client);
    if (clip && rect.width > 0 && rect.height > 0) animated.push({ windowId: move.windowId, rect, clip });
  }
  return animated.concat(rest);
}

function prefersReducedMotion() {
  return typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
}

function activeIds(monitor: MonitorState | undefined) {
  const page = monitor?.pages.find((item) => item.id === monitor.activePage);
  return new Set(page ? page.columns.flatMap((column) => column.windows).concat(page.floatingWindows) : []);
}

/** 最小化窗口的矩形不是桌面位置（Windows 为 -32000），它们的缩略图直接停在终点。 */
function nativeMap(snapshot: Snapshot) {
  return new Map(snapshot.windows.filter((window) => !window.native.minimized)
    .map((window) => [window.native.id, window.native.rect]));
}

function workArea(monitor: MonitorState | undefined): Rect | null {
  const work = monitor?.monitor.workArea;
  if (!work || work.width <= 0 || work.height <= 0 || ![work.x, work.y, work.width, work.height].every(Number.isFinite)) return null;
  return work;
}

export function useOverviewZoom({
  snapshot, monitor, previewsAvailable, publish, onDismiss,
}: {
  snapshot: Snapshot;
  monitor: MonitorState | undefined;
  previewsAvailable: boolean;
  publish: (slots: PreviewSlot[]) => void;
  onDismiss: () => void;
}) {
  const animate = previewsAvailable && snapshot.overviewAnimationMs > 0 && !prefersReducedMotion();
  const [phase, setPhase] = useState<'open' | 'close' | null>(animate ? 'open' : null);
  const mode = useRef<'open' | 'live' | 'close'>(animate ? 'open' : 'live');
  const opened = useRef(false);
  const slots = useRef<PreviewSlot[]>([]);
  const shown = useRef(new Map<string, Rect>());
  const frozen = useRef<ZoomMove[] | null>(null);
  const started = useRef<number | null>(null);
  const measured = useRef(false);
  const raf = useRef(0);
  const mounted = useRef(true);
  const live = useRef({ snapshot, monitor, publish, onDismiss, previewsAvailable });
  live.current = { snapshot, monitor, publish, onDismiss, previewsAvailable };

  const cancelFrame = () => { if (typeof cancelAnimationFrame === 'function') cancelAnimationFrame(raf.current); };
  const finishOpen = () => {
    cancelFrame();
    mode.current = 'live';
    started.current = null;
    shown.current = new Map();
    if (mounted.current) setPhase(null);
    live.current.publish(slots.current);
  };
  const dismiss = () => {
    cancelFrame();
    const wasAnimating = mode.current !== 'live';
    mode.current = 'live';
    started.current = null;
    if (wasAnimating && mounted.current) setPhase(null);
    live.current.onDismiss();
  };

  const draw = (now: number) => {
    if (!mounted.current) return;
    const state = live.current;
    const work = workArea(state.monitor);
    const duration = state.snapshot.overviewAnimationMs;
    const closing = mode.current === 'close';
    if (!work || !(duration > 0)) {
      if (closing) dismiss();
      else finishOpen();
      return;
    }
    const t = Math.min(1, Math.max(0, (now - (started.current ?? now)) / duration));
    const active = activeIds(state.monitor);
    const natives = nativeMap(state.snapshot);
    let moves: ZoomMove[];
    let rest: PreviewSlot[];
    if (closing) {
      rest = [];
      moves = [];
      for (const move of frozen.current ?? []) {
        if (!active.has(move.windowId)) continue;
        const native = natives.get(move.windowId);
        if (!finiteRect(native)) continue;
        moves.push({ ...move, to: desktopRect(native, work) });
      }
      if (!moves.length) { dismiss(); return; }
    } else {
      ({ moves, rest } = zoomPlan(slots.current, natives, active, work, false));
      if (!moves.length || t >= 1) { finishOpen(); return; }
    }
    const frame = zoomSlots(moves, t, clientArea(work), rest);
    shown.current = new Map(frame.map((slot) => [slot.windowId, slot.rect]));
    state.publish(frame);
    if (t >= 1) { dismiss(); return; }
    raf.current = requestAnimationFrame(draw);
  };

  const arm = () => {
    if (!mounted.current || mode.current !== 'open' || started.current !== null || !measured.current) return;
    const state = live.current;
    const work = workArea(state.monitor);
    const moves = work ? zoomPlan(slots.current, nativeMap(state.snapshot), activeIds(state.monitor), work, false).moves : [];
    if (!moves.length) { finishOpen(); return; }
    started.current = performance.now();
    draw(started.current);
  };

  const onSlots = (next: PreviewSlot[]) => {
    // ponytail: 动画中丢弃空测量。usePreviewSlots 暂停时会推 []，接了就会清掉缩放；真清空要等动画结束。
    if (mode.current !== 'live' && next.length === 0 && slots.current.length > 0) return;
    measured.current = true;
    slots.current = next;
    if (mode.current === 'live') { shown.current = new Map(); live.current.publish(next); }
    else if (mode.current === 'open') arm();
  };

  const requestClose = () => {
    if (!mounted.current || mode.current === 'close') return;
    const state = live.current;
    const work = workArea(state.monitor);
    if (!state.previewsAvailable || !(state.snapshot.overviewAnimationMs > 0) || prefersReducedMotion() || !work) {
      dismiss();
      return;
    }
    const { moves } = zoomPlan(slots.current, nativeMap(state.snapshot), activeIds(state.monitor), work, true, shown.current);
    if (!moves.length) { dismiss(); return; }
    frozen.current = moves;
    mode.current = 'close';
    setPhase('close');
    cancelFrame();
    started.current = performance.now();
    draw(started.current);
  };

  useLayoutEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; cancelFrame(); started.current = null; };
  }, []);
  // 时长可能比这次打开晚一帧到达；空测量先别结束，等占位符进 DOM。
  useLayoutEffect(() => {
    if (!animate || opened.current || mode.current === 'close') return;
    opened.current = true;
    mode.current = 'open';
    setPhase('open');
    if (measured.current && slots.current.length) arm();
  }, [animate]);

  return { phase, onSlots, requestClose };
}
