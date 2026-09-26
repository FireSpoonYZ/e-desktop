import { useLayoutEffect, useRef } from 'react';
import type { RefObject } from 'react';
import type { Rect } from '../model';

export interface PreviewSlot {
  windowId: string;
  /** Physical pixels relative to the destination client area. */
  rect: Rect;
  clip: Rect;
}
export interface PreviewStatus {
  windowId: string;
  state: 'ready' | 'hidden' | 'unavailable' | 'sourceGone' | 'failed';
  message: string;
}
type Bounds = Pick<DOMRectReadOnly, 'left' | 'top' | 'right' | 'bottom'>;

/** Round inward: fractional CSS pixels must never paint into adjacent controls. */
export function physicalPreviewSlot(windowId: string, bounds: Bounds, visible: Bounds, scale: number): PreviewSlot | null {
  if (!Number.isFinite(scale) || scale <= 0) return null;
  const physical = (box: Bounds): Rect | null => {
    const x = Math.ceil(box.left * scale), y = Math.ceil(box.top * scale);
    const right = Math.floor(box.right * scale), bottom = Math.floor(box.bottom * scale);
    if (![x, y, right, bottom].every((v) => Number.isSafeInteger(v) && v >= -2147483648 && v <= 2147483647)
      || right <= x || bottom <= y || right - x > 2147483647 || bottom - y > 2147483647) return null;
    return { x, y, width: right - x, height: bottom - y };
  };
  const rect = physical(bounds);
  const clip = physical({ left: Math.max(bounds.left, visible.left), top: Math.max(bounds.top, visible.top),
    right: Math.min(bounds.right, visible.right), bottom: Math.min(bounds.bottom, visible.bottom) });
  return rect && clip ? { windowId, rect, clip } : null;
}

function measure(element: HTMLElement): PreviewSlot | null {
  // Pinch zoom/panning moves the visual viewport independently of the native client.
  const viewport = window.visualViewport;
  if (viewport && (viewport.scale !== 1 || viewport.offsetLeft !== 0 || viewport.offsetTop !== 0)) return null;
  const bounds = element.getBoundingClientRect();
  const visible = { left: 0, top: 0, right: document.documentElement.clientWidth, bottom: document.documentElement.clientHeight };
  for (let node: HTMLElement | null = element; node; node = node.parentElement) {
    const style = getComputedStyle(node);
    // Native DWM rectangles cannot follow CSS transforms, masks, or hidden subtrees.
    if (style.visibility !== 'visible' || style.display === 'none' || Number(style.opacity) !== 1
      || style.transform !== 'none' || style.clipPath !== 'none' || style.maskImage !== 'none') return null;
    if (node === element) continue;
    const box = node.getBoundingClientRect();
    if (style.overflowX !== 'visible') {
      visible.left = Math.max(visible.left, box.left + node.clientLeft);
      visible.right = Math.min(visible.right, box.left + node.clientLeft + node.clientWidth);
    }
    if (style.overflowY !== 'visible') {
      visible.top = Math.max(visible.top, box.top + node.clientTop);
      visible.bottom = Math.min(visible.bottom, box.top + node.clientTop + node.clientHeight);
    }
  }
  return physicalPreviewSlot(element.dataset.previewWindowId!, bounds, visible, window.devicePixelRatio);
}

/**
 * Observe placeholders inside the existing Overview root; no IPC and no source HWNDs.
 * The webview must fill the destination HWND client area. Keep interactive/overlapping
 * content outside placeholders; suspend via `active` before overlays or animations.
 * Host must serialize callbacks, drop stale replies and clear on native dismiss/destroy.
 */
export function usePreviewSlots(
  root: RefObject<HTMLElement | null>,
  { available, active = true, onSlotsChange }: {
    available: boolean;
    active?: boolean;
    onSlotsChange: (slots: PreviewSlot[]) => void;
  },
): void {
  const callback = useRef(onSlotsChange);
  useLayoutEffect(() => { callback.current = onSlotsChange; });
  useLayoutEffect(() => {
    const scope = root.current;
    if (!scope || !available || !active) {
      callback.current([]);
      return;
    }
    let previous = '';
    const publish = () => {
      const slots = document.visibilityState === 'hidden' ? [] : [...scope.querySelectorAll<HTMLElement>('[data-preview-window-id]')]
        .map(measure).filter((slot): slot is PreviewSlot => slot !== null);
      const signature = JSON.stringify(slots);
      if (signature !== previous) {
        previous = signature;
        callback.current(slots);
      }
    };
    const resize = new ResizeObserver(publish);
    const observe = () => {
      resize.disconnect();
      const nodes = new Set<HTMLElement>([scope]);
      for (const element of scope.querySelectorAll<HTMLElement>('[data-preview-window-id]')) {
        for (let node: HTMLElement | null = element; node; node = node.parentElement) nodes.add(node);
      }
      nodes.forEach((node) => resize.observe(node));
      publish();
    };
    const mutation = new MutationObserver(observe);
    mutation.observe(scope, { subtree: true, childList: true, attributes: true,
      attributeFilter: ['class', 'style', 'hidden', 'data-preview-window-id'] });
    for (let node = scope.parentElement; node; node = node.parentElement) {
      mutation.observe(node, { attributes: true, attributeFilter: ['class', 'style', 'hidden'] });
    }
    // Scroll does not resize a placeholder, so ResizeObserver alone is insufficient.
    document.addEventListener('scroll', publish, true);
    document.addEventListener('visibilitychange', publish);
    window.addEventListener('resize', publish);
    window.visualViewport?.addEventListener('resize', publish);
    window.visualViewport?.addEventListener('scroll', publish);
    let resolution = matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
    const rescale = () => {
      resolution.removeEventListener('change', rescale);
      resolution = matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
      resolution.addEventListener('change', rescale);
      publish();
    };
    resolution.addEventListener('change', rescale);
    observe();
    return () => {
      resize.disconnect();
      mutation.disconnect();
      resolution.removeEventListener('change', rescale);
      document.removeEventListener('scroll', publish, true);
      document.removeEventListener('visibilitychange', publish);
      window.removeEventListener('resize', publish);
      window.visualViewport?.removeEventListener('resize', publish);
      window.visualViewport?.removeEventListener('scroll', publish);
      callback.current([]);
    };
  }, [root, available, active]);
}

/** A dedicated, non-interactive rectangle; captions/buttons belong outside it. */
export function WindowPreview({ windowId, available, status, height = 100 }: {
  windowId: string;
  available: boolean;
  status?: PreviewStatus;
  height?: number;
}) {
  const state = available && status?.windowId === windowId ? status.state : undefined;
  const text = !available ? '此平台不支持实时预览'
    : state === 'ready' ? '窗口实时预览'
    : state === 'sourceGone' ? '窗口已不可用'
    : state === 'failed' ? '实时预览失败'
    : state === 'unavailable' ? '实时预览不可用'
    : state === 'hidden' ? '预览区域不可见' : '等待实时预览';
  return <span data-preview-window-id={windowId} data-preview-state={state ?? 'unavailable'}
    role="img" aria-label={text} title={state && status?.message ? `${text}：${status.message}` : text}
    style={{ display: 'block', width: '100%', height, overflow: 'hidden', pointerEvents: 'none' }}>
    {state !== 'ready' && <small>{text}</small>}
  </span>;
}
