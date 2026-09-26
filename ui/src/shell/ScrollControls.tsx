import { useEffect, useRef } from 'react';
import type { ControlProps } from '../model';
import { createScrollDispatcher, scrollTarget, selectedMonitor, wheelPixels } from './scroll';

export function ScrollControls({ snapshot, onCommand, busy = false }: ControlProps) {
  const region = useRef<HTMLDivElement>(null);
  const current = useRef({ snapshot, onCommand, busy });
  current.current = { snapshot, onCommand, busy };
  const dispatcher = useRef<ReturnType<typeof createScrollDispatcher> | null>(null);
  const monitor = selectedMonitor(snapshot);
  const page = monitor?.pages.find(({ id }) => id === monitor.activePage);
  const disabled = !scrollTarget(snapshot, busy);

  useEffect(() => {
    const element = region.current;
    if (!element) return;
    const target = () => scrollTarget(current.current.snapshot, current.current.busy);
    const scroll = createScrollDispatcher(target, (command) => current.current.onCommand(command));
    dispatcher.current = scroll;
    const onWheel = (event: WheelEvent) => {
      const monitor = target();
      if (!monitor || event.ctrlKey) return;
      const lineHeight = Number.parseFloat(getComputedStyle(element).lineHeight) || 18;
      if (scroll.push(wheelPixels(event, monitor, lineHeight))) event.preventDefault();
    };
    // Only this dedicated region consumes wheel input; React wheel listeners are passive.
    element.addEventListener('wheel', onWheel, { passive: false });
    return () => {
      element.removeEventListener('wheel', onWheel);
      scroll.dispose();
      dispatcher.current = null;
    };
  }, []);
  useEffect(() => { dispatcher.current?.cancel(); }, [disabled, monitor?.monitor.id,
    monitor?.activePage, monitor?.monitor.scaleFactor, monitor?.viewport.width]);

  return <div ref={region} className="shell-scroll" role="group" aria-label="当前显示器横向滚动"
    aria-disabled={disabled} title="在此滚轮或触控板横向滚动；箭头每次 160 物理像素">
    <button type="button" disabled={disabled} aria-label="向左滚动 160 物理像素"
      onClick={() => dispatcher.current?.push(-160)}>←</button>
    <span className="shell-scroll-position">横向 {page ? `${page.viewportX} px` : '无目标'}</span>
    <button type="button" disabled={disabled} aria-label="向右滚动 160 物理像素"
      onClick={() => dispatcher.current?.push(160)}>→</button>
  </div>;
}
