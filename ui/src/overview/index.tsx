import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react';
import type { CSSProperties } from 'react';
import type { OverviewProps, Page, WindowId } from '../model';
import { useCommand, useSurface } from '../commands/surface';
import { canInteract, dropCommand, moveCommand, overviewScale, pageGeometry, sizingTarget, trackDrag, trackResize, widthCommand } from './pointer';
import { WindowPreview, usePreviewFeed, usePreviewSlots } from './previews';
import type { PreviewRequest } from './previews';
import { useOverviewZoom } from './zoom';
import './style.css';

export function Overview({ snapshot, onCommand, onDismiss, busy = false, previewSession = null, syncPreviews }: OverviewProps & {
  previewSession?: number | null; syncPreviews?: PreviewRequest;
}) {
  const id = useId();
  const closeRef = useRef(onDismiss);
  const { root, onKeyDown } = useSurface(() => closeRef.current());
  const { run, pending, error } = useCommand(onCommand);
  const [monitorId, setMonitorId] = useState(snapshot.activeMonitor);
  const [availableWidth, setAvailableWidth] = useState(Infinity);
  const selected = snapshot.monitors.find(({ monitor }) => monitor.id === monitorId)
    ?? snapshot.monitors.find(({ monitor }) => monitor.id === snapshot.activeMonitor) ?? snapshot.monitors[0];
  const scale = selected ? overviewScale(selected.monitor.scaleFactor, selected.viewport.width, availableWidth) : .5;
  const ready = snapshot.enabled && snapshot.backend.availability === 'ready' && snapshot.backend.capabilities.placement;
  const blocked = busy || pending;
  const latest = useRef({ snapshot, blocked, run, scale });
  latest.current = { snapshot, blocked, run, scale };
  const dragging = useRef<{ windowId: WindowId; cancel: () => void } | null>(null);
  const [dropPreview, setDropPreview] = useState<{ windowId: WindowId; pageId: string | null } | null>(null);
  const suppressClick = useRef(false);
  const [preview, setPreview] = useState<{ columnId: string; width: number } | null>(null);
  const resizing = useRef<{ valid: () => boolean; cancel: () => void } | null>(null);
  const previewsAvailable = previewSession !== null && !!syncPreviews;
  const { statuses, publish } = usePreviewFeed(previewSession, syncPreviews);
  const zoom = useOverviewZoom({ snapshot, monitor: selected, previewsAvailable, publish, onDismiss });
  const requestClose = () => {
    dragging.current?.cancel(); resizing.current?.cancel();
    zoom.requestClose();
  };
  closeRef.current = requestClose;
  usePreviewSlots(root, { available: previewsAvailable, onSlotsChange: zoom.onSlots });
  useLayoutEffect(() => {
    const element = root.current;
    if (!element) return;
    const measure = () => setAvailableWidth(Math.max(1, element.clientWidth - 96));
    const observer = new ResizeObserver(measure);
    observer.observe(element); measure();
    return () => observer.disconnect();
  }, [root]);
  const scrollPositions = selected?.pages.map((page) => `${page.id}:${page.viewportX}`).join('|');
  useLayoutEffect(() => {
    for (const page of selected?.pages ?? []) {
      const element = [...(root.current?.querySelectorAll<HTMLElement>('[data-overview-scroll]') ?? [])]
        .find((element) => element.dataset.overviewScroll === page.id);
      if (element) element.scrollLeft = Math.max(0, page.viewportX) * scale;
    }
  }, [root, selected?.monitor.id, scrollPositions, scale]);
  useLayoutEffect(() => {
    root.current?.querySelector('[data-active="true"]')?.scrollIntoView({ block: 'center' });
  }, [root, selected?.monitor.id]);
  const dropAt = (windowId: WindowId, event: PointerEvent) => {
    const page = document.elementFromPoint(event.clientX, event.clientY)?.closest<HTMLElement>('[data-overview-page]');
    const pageId = page?.dataset.overviewPage;
    const scroll = page?.querySelector<HTMLElement>('[data-overview-scroll]');
    if (!pageId || !scroll) return null;
    const bounds = scroll.getBoundingClientRect();
    const state = latest.current;
    const command = dropCommand(state.snapshot, state.blocked, windowId, pageId,
      event.clientX - bounds.left - scroll.clientLeft, event.clientY - bounds.top - scroll.clientTop,
      scroll.scrollLeft, state.scale);
    return command ? { pageId, command } : null;
  };
  useEffect(() => {
    if (resizing.current && !resizing.current.valid()) resizing.current.cancel();
    if (dragging.current && (!canInteract(snapshot, blocked)
      || !snapshot.windows.some((window) => window.native.id === dragging.current?.windowId)
      || (dropPreview?.pageId && !snapshot.monitors.some((monitor) => monitor.pages.some((page) => page.id === dropPreview.pageId))))) dragging.current.cancel();
  }, [snapshot, blocked, dropPreview?.pageId, scale]);
  useEffect(() => () => { resizing.current?.cancel(); dragging.current?.cancel(); }, []);
  const move = (windowId: WindowId, pageId: string) => {
    const state = latest.current;
    const command = moveCommand(state.snapshot, state.blocked, windowId, pageId);
    if (command) void state.run(command);
  };
  const resize = (windowId: WindowId, width: number) => {
    const state = latest.current;
    const command = widthCommand(state.snapshot, state.blocked, windowId, width);
    if (command) void state.run(command);
  };
  const windows = new Map(snapshot.windows.map((window) => [window.native.id, window]));
  const destinations = snapshot.monitors.flatMap(({ monitor, pages }) => pages.map((page) => ({
    id: page.id, label: `${monitor.name} · ${page.name}`,
  })));
  const card = (windowId: WindowId) => {
    const window = windows.get(windowId);
    if (!window) return <div className="overview-missing" key={windowId}>窗口已不可用</div>;
    const { native } = window;
    return <article key={windowId} className="overview-window" data-focused={snapshot.focusedWindow === windowId}
      style={{ flexGrow: Math.max(1, native.rect.height) }}>
      <button className="overview-window-focus" aria-pressed={snapshot.focusedWindow === windowId}
        aria-label={`聚焦 ${native.title || '无标题窗口'} · ${native.appName || '未知应用'}`}
        title={`${native.title || '无标题窗口'} · ${native.appName || '未知应用'}`}
        disabled={blocked || !ready || !snapshot.backend.capabilities.focus}
        draggable={false} onDragStart={(event) => event.preventDefault()}
        onLostPointerCapture={() => dragging.current?.cancel()}
        onPointerDown={(event) => {
          suppressClick.current = false;
          if (event.button !== 0 || !canInteract(snapshot, blocked)) return;
          resizing.current?.cancel(); dragging.current?.cancel();
          const handle = event.currentTarget, pointerId = event.pointerId;
          handle.setPointerCapture(pointerId);
          const cancel = trackDrag(globalThis.window, pointerId, event.clientX, event.clientY,
            (event) => { suppressClick.current = true; setDropPreview({ windowId, pageId: dropAt(windowId, event)?.pageId ?? null }); },
            (event) => {
              dragging.current = null; setDropPreview(null);
              if (handle.hasPointerCapture(pointerId)) handle.releasePointerCapture(pointerId);
              if (event) { const drop = dropAt(windowId, event); if (drop) void latest.current.run(drop.command); }
            });
          dragging.current = { windowId, cancel };
        }}
        onClick={(event) => {
          if (suppressClick.current && event?.detail !== 0) { event?.preventDefault(); return; }
          if (!blocked && ready) void run({ type: 'focusWindow', windowId }, requestClose);
        }}>
        <WindowPreview windowId={windowId} available={previewsAvailable} status={statuses[windowId]} />
        <span className="overview-caption"><strong>{native.title || '无标题窗口'}</strong><span>{native.appName || '未知应用'}{window.fullscreen ? ' · 全屏' : window.floating ? ' · 浮动' : ''}</span></span>
      </button>
    </article>;
  };
  const moveControl = (windowId: WindowId, page: Page) => {
    const native = windows.get(windowId)?.native;
    if (!native) return null;
    return <label key={windowId} className="overview-move"><span>{native.title || '无标题窗口'}</span>
      <select aria-label={`移动 ${native.title || '无标题窗口'} 到页面`} value=""
        disabled={blocked || !ready || destinations.length < 2}
        onChange={(event) => { if (event.target.value) move(windowId, event.target.value); }}>
        <option value="" disabled>移动到工作区…</option>
        {destinations.filter((target) => target.id !== page.id).map((target) => <option key={target.id} value={target.id}>{target.label}</option>)}
      </select>
    </label>;
  };
  return <section ref={root} tabIndex={-1} role="dialog" aria-modal="true" aria-labelledby={`${id}-title`}
    className="desktop-overview" data-animating={zoom.phase ?? undefined}
    style={zoom.phase ? { '--overview-ms': `${snapshot.animationDurationMs}ms` } as CSSProperties : undefined}
    onKeyDown={(event) => {
      if (event.key === 'Escape' && (dragging.current || resizing.current)) {
        event.preventDefault(); event.stopPropagation(); dragging.current?.cancel(); resizing.current?.cancel(); return;
      }
      onKeyDown(event);
    }} aria-busy={blocked}>
    <header className="overview-header"><h1 id={`${id}-title`}>工作区概览</h1>
      <nav className="overview-monitors" aria-label="选择显示器">
        {snapshot.monitors.map(({ monitor }, index) => <button key={monitor.id} title={monitor.name}
          aria-pressed={monitor.id === selected?.monitor.id} disabled={!!dropPreview || !!preview || zoom.phase !== null}
          onClick={() => setMonitorId(monitor.id)}>显示器 {index + 1}</button>)}
      </nav>
      <button data-initial-focus className="overview-close" onClick={requestClose}>关闭 <kbd>Esc</kbd></button></header>
    <div className="overview-notices">
      {!snapshot.enabled && <p role="status">管理已暂停。启用后可进入工作区、聚焦和移动窗口。</p>}
      {snapshot.backend.availability !== 'ready' && <p role="status">{snapshot.backend.message || '原生后端不可用'}</p>}
      {error && <p role="alert">{error}</p>}
      {snapshot.errors.map((item, position) => <p role="alert" key={position}>{item.message}</p>)}
      {!snapshot.monitors.length && <p role="status">尚未发现可用显示器。没有可显示的页面。</p>}
    </div>
    <div className="overview-workspaces" aria-label={selected?.monitor.name}>
      {selected && <section className="overview-monitor" aria-label={selected.monitor.name} key={selected.monitor.id}
        style={{
          // niri logical sizes (snapshot.gaps, focus ring 4) zoomed like the workspace.
          '--gap': `${snapshot.gaps * scale * selected.monitor.scaleFactor}px`,
          '--ring': `${4 * scale * selected.monitor.scaleFactor}px`,
          '--ws-h': `${selected.viewport.height * scale}px`,
        } as CSSProperties}>
        {!selected.pages.length && <p role="status">此显示器还没有工作区。</p>}
        {selected.pages.map((page, position) => {
          const { monitor, viewport, activePage } = selected;
          const geometry = pageGeometry(page, viewport, scale);
          const dropReady = !!dropPreview && !!dropCommand(snapshot, blocked, dropPreview.windowId, page.id,
            geometry.width / 2, geometry.height / 2, 0, scale);
          return <section key={page.id} className="overview-page" aria-label={page.name} data-active={page.id === activePage}
            style={{ width: geometry.width }} data-overview-page={page.id}
            data-drop-ready={dropReady}
            data-drop-target={dropPreview?.pageId === page.id && dropReady}>
            <header><button aria-current={page.id === activePage ? 'page' : undefined} disabled={blocked || !ready}
              onClick={() => void run({ type: 'switchPage', monitorId: monitor.id, pageId: page.id }, requestClose)}>
              <span className="overview-page-number">{String(position + 1).padStart(2, '0')}</span>{page.name}
            </button></header>
            <div className="overview-scroll" data-overview-scroll={page.id} style={{ height: geometry.height }}
              tabIndex={0} role="region" aria-label={`${page.name} 的横向窗口列表`}>
              <div className="overview-columns" style={{ minWidth: geometry.stripWidth, paddingLeft: `calc(${geometry.leading}px + var(--gap) / 2)` }}>
                {page.columns.map((column) => {
                  const windowId = column.windows.find((id) => sizingTarget(snapshot, id));
                  const width = preview?.columnId === column.id ? preview.width : column.width;
                  const disabled = blocked || !ready || !windowId;
                  return <div key={column.id} className="overview-column" style={{ width: width * scale }}>
                    {column.windows.map(card)}
                    <button className="overview-resize" aria-label={`拖动调整列 ${column.id} 宽度；也可展开调整布局使用列宽输入框`}
                      disabled={disabled} tabIndex={-1}
                      onLostPointerCapture={() => resizing.current?.cancel()}
                      onPointerDown={(event) => {
                        if (event.button !== 0 || disabled || !windowId) return;
                        event.preventDefault(); dragging.current?.cancel(); resizing.current?.cancel();
                        const valid = () => {
                          const state = latest.current;
                          const target = sizingTarget(state.snapshot, windowId);
                          return canInteract(state.snapshot, state.blocked) && !!target && target.column.id === column.id
                            && target.page.id === page.id && target.monitor.monitor.id === monitor.id
                            && target.column.width === column.width && target.monitor.viewport.width === viewport.width && state.scale === scale;
                        };
                        if (!valid()) return;
                        const handle = event.currentTarget, pointerId = event.pointerId;
                        handle.setPointerCapture(pointerId);
                        const cancel = trackResize(window, pointerId, event.clientX, column.width, scale, viewport.width,
                          (width) => setPreview({ columnId: column.id, width }),
                          (width) => {
                            resizing.current = null; setPreview(null);
                            if (handle.hasPointerCapture(pointerId)) handle.releasePointerCapture(pointerId);
                            if (width !== null && valid()) resize(windowId, width);
                          });
                        resizing.current = { valid, cancel };
                      }} />
                    {preview?.columnId === column.id && <output className="overview-resize-value">{width} px</output>}
                  </div>;
                })}
                {!page.columns.length && <p className="overview-empty">此页没有平铺窗口。<span>拖动窗口到这里</span></p>}
              </div>
            </div>
            {page.floatingWindows.length > 0 && <div className="overview-floating"><h3>浮动窗口</h3><div>{page.floatingWindows.map(card)}</div></div>}
            <details className="overview-edit"><summary>调整布局</summary><div className="overview-edit-controls">
              {page.columns.map((column, index) => {
                const windowId = column.windows.find((id) => sizingTarget(snapshot, id));
                return <div className="overview-edit-column" key={column.id}>
                  <label className="overview-width">第 {index + 1} 列 · 物理像素
                    <input key={column.width} type="number" min={1} max={viewport.width} step={1}
                      aria-label={`${page.name} 列 ${column.id} 宽度（物理像素）`} defaultValue={column.width} disabled={blocked || !ready || !windowId}
                      onBlur={(event) => {
                        const input = event.currentTarget;
                        if (windowId && input.validity.valid && input.valueAsNumber !== column.width) resize(windowId, input.valueAsNumber);
                        input.value = String(column.width);
                      }}
                      onKeyDown={(event) => {
                        if (event.key === 'Enter') { event.preventDefault(); event.currentTarget.blur(); }
                        if (event.key === 'Escape') {
                          event.preventDefault(); event.stopPropagation(); event.currentTarget.value = String(column.width); event.currentTarget.blur();
                        }
                      }} />
                  </label>
                  {column.windows.map((windowId) => moveControl(windowId, page))}
                </div>;
              })}
              {page.floatingWindows.map((windowId) => moveControl(windowId, page))}
            </div></details>
          </section>;
        })}
        <button className="overview-add" disabled={blocked || !ready} onClick={() => void run({ type: 'addPage', monitorId: selected.monitor.id })}>＋ 新增工作区</button>
      </section>}
    </div>
    <footer className="overview-help">滚动浏览工作区 <span>·</span> 拖动窗口移动 <span>·</span> 拖动列边缘调宽 <span>·</span> <kbd>Tab</kbd> 选择，<kbd>Enter</kbd> 进入</footer>
  </section>;
}
