import { useEffect, useId, useRef, useState } from 'react';
import type { OverviewProps, Page, WindowId } from '../model';
import { useCommand, useSurface } from '../commands/surface';
import { canInteract, moveCommand, overviewScale, sizingTarget, trackDrag, trackResize, widthCommand } from './pointer';
import { WindowPreview, usePreviewFeed, usePreviewSlots } from './previews';
import type { PreviewRequest } from './previews';
import './style.css';

export function Overview({ snapshot, onCommand, onDismiss, busy = false, previewSession = null, syncPreviews }: OverviewProps & {
  previewSession?: number | null; syncPreviews?: PreviewRequest;
}) {
  const id = useId();
  const { root, onKeyDown } = useSurface(onDismiss);
  const { run, pending, error } = useCommand(onCommand);
  const ready = snapshot.enabled && snapshot.backend.availability === 'ready' && snapshot.backend.capabilities.placement;
  const blocked = busy || pending;
  const latest = useRef({ snapshot, blocked, run });
  latest.current = { snapshot, blocked, run };
  const dragging = useRef<{ windowId: WindowId; cancel: () => void } | null>(null);
  const [dropPreview, setDropPreview] = useState<{ windowId: WindowId; pageId: string | null } | null>(null);
  const suppressClick = useRef(false);
  const [preview, setPreview] = useState<{ columnId: string; width: number } | null>(null);
  const resizing = useRef<{ valid: () => boolean; cancel: () => void } | null>(null);
  const previewsAvailable = previewSession !== null && !!syncPreviews;
  const { statuses, publish } = usePreviewFeed(previewSession, syncPreviews);
  usePreviewSlots(root, { available: previewsAvailable, active: !dropPreview && !preview, onSlotsChange: publish });
  const pageAt = (event: PointerEvent) => document.elementFromPoint(event.clientX, event.clientY)
    ?.closest<HTMLElement>('[data-overview-page]')?.dataset.overviewPage ?? null;
  useEffect(() => {
    if (resizing.current && !resizing.current.valid()) resizing.current.cancel();
    if (dragging.current && (!canInteract(snapshot, blocked)
      || !snapshot.windows.some((window) => window.native.id === dragging.current?.windowId)
      || (dropPreview?.pageId && !snapshot.monitors.some((monitor) => monitor.pages.some((page) => page.id === dropPreview.pageId))))) dragging.current.cancel();
  }, [snapshot, blocked, dropPreview?.pageId]);
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
  const card = (windowId: WindowId, page: Page) => {
    const window = windows.get(windowId);
    if (!window) return <div className="overview-missing" key={windowId}>窗口已不可用</div>;
    const { native } = window;
    return <article key={windowId} className="overview-window" data-focused={snapshot.focusedWindow === windowId}
      style={{ flexGrow: Math.max(1, native.rect.height) }}>
      <button className="overview-window-focus" aria-pressed={snapshot.focusedWindow === windowId}
        aria-label={`聚焦 ${native.title || '无标题窗口'} · ${native.appName || '未知应用'}`}
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
            (event) => { suppressClick.current = true; setDropPreview({ windowId, pageId: pageAt(event) }); },
            (event) => {
              dragging.current = null; setDropPreview(null);
              if (handle.hasPointerCapture(pointerId)) handle.releasePointerCapture(pointerId);
              if (event) { const pageId = pageAt(event); if (pageId) move(windowId, pageId); }
            });
          dragging.current = { windowId, cancel };
        }}
        onClick={(event) => {
          if (suppressClick.current && event?.detail !== 0) { event?.preventDefault(); return; }
          if (!blocked && ready) void run({ type: 'focusWindow', windowId }, onDismiss);
        }}>
        <strong>{native.title || '无标题窗口'}</strong><span>{native.appName || '未知应用'}</span>
        <WindowPreview windowId={windowId} available={previewsAvailable} status={statuses[windowId]} />
        {(window.fullscreen || window.floating) && <small>{window.fullscreen ? '布局全屏' : '浮动'}</small>}
      </button>
      <label className="overview-move">移动到页面
        <select aria-label={`移动 ${native.title || '无标题窗口'} 到页面`} value=""
          disabled={blocked || !ready || destinations.length < 2}
          onChange={(event) => {
            if (event.target.value) move(windowId, event.target.value);
          }}>
          <option value="" disabled>选择目标页面…</option>
          {destinations.filter((target) => target.id !== page.id).map((target) => <option key={target.id} value={target.id}>{target.label}</option>)}
        </select>
      </label>
    </article>;
  };
  return <section ref={root} tabIndex={-1} role="dialog" aria-modal="true" aria-labelledby={`${id}-title`}
    className="desktop-overview" onKeyDown={(event) => {
      if (event.key === 'Escape' && (dragging.current || resizing.current)) {
        event.preventDefault(); event.stopPropagation(); dragging.current?.cancel(); resizing.current?.cancel(); return;
      }
      onKeyDown(event);
    }} aria-busy={blocked}>
    <header className="overview-header"><div><h1 id={`${id}-title`}>页面概览</h1><p className="muted">拖动窗口到页面 · 拖动列边缘调宽 · Tab 选择，Enter 进入</p></div>
      <button data-initial-focus onClick={onDismiss}>关闭 · Esc</button></header>
    {!snapshot.enabled && <p role="status">管理已暂停。启用后可进入页面、聚焦和移动窗口。</p>}
    {snapshot.backend.availability !== 'ready' && <p role="status">{snapshot.backend.message || '原生后端不可用'}</p>}
    {error && <p role="alert">{error}</p>}
    {snapshot.errors.map((item, position) => <p role="alert" key={position}>{item.message}</p>)}
    {!snapshot.monitors.length && <p role="status">尚未发现可用显示器。没有可显示的页面。</p>}
    {snapshot.monitors.map(({ monitor, pages, activePage, viewport }) => {
      const scale = overviewScale(monitor.scaleFactor);
      return <section key={monitor.id} className="overview-monitor" aria-label={monitor.name}>
        <h2>{monitor.name}{snapshot.activeMonitor === monitor.id ? ' · 当前显示器' : ''}</h2>
        {!pages.length && <p role="status">此显示器还没有页面。</p>}
        {pages.map((page, position) => <section key={page.id} className="overview-page" aria-label={page.name} data-active={page.id === activePage}
          data-overview-page={page.id}
          data-drop-ready={!!dropPreview && !!moveCommand(snapshot, blocked, dropPreview.windowId, page.id)}
          data-drop-target={dropPreview?.pageId === page.id && !!moveCommand(snapshot, blocked, dropPreview.windowId, page.id)}>
          <header><button aria-current={page.id === activePage ? 'page' : undefined} disabled={blocked || !ready}
            onClick={() => void run({ type: 'switchPage', monitorId: monitor.id, pageId: page.id }, onDismiss)}>
            {String(position + 1).padStart(2, '0')} · {page.name}{page.id === activePage ? ' · 当前页面' : ''}
          </button><span className="muted">{page.columns.reduce((count, column) => count + column.windows.length, 0) + page.floatingWindows.length} 个窗口</span></header>
          <div className="overview-scroll" tabIndex={0} role="region" aria-label={`${page.name} 的横向窗口列表`}>
            <div className="overview-columns" style={{ minHeight: Math.max(180, viewport.height * scale) }}>
              {page.columns.map((column) => {
                const windowId = column.windows.find((id) => sizingTarget(snapshot, id));
                const width = preview?.columnId === column.id ? preview.width : column.width;
                const disabled = blocked || !ready || !windowId;
                return <div key={column.id} className="overview-column" style={{ width: width * scale }}>
                  <label className="overview-width">列宽 · 物理像素
                    <input key={column.width} type="number" min={1} max={viewport.width} step={1}
                      aria-label={`${page.name} 列 ${column.id} 宽度（物理像素）`} defaultValue={column.width} disabled={disabled}
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
                    {preview?.columnId === column.id && <output>{width} px</output>}
                  </label>
                  {column.windows.map((id) => card(id, page))}
                  <button className="overview-resize" aria-label={`拖动调整列 ${column.id} 宽度；也可使用列宽输入框`}
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
                          && target.column.width === column.width && target.monitor.viewport.width === viewport.width && target.scale === scale;
                      };
                      if (!valid()) return;
                      const handle = event.currentTarget;
                      const pointerId = event.pointerId;
                      handle.setPointerCapture(pointerId);
                      const cancel = trackResize(window, event.pointerId, event.clientX, column.width, scale, viewport.width,
                        (width) => setPreview({ columnId: column.id, width }),
                        (width) => {
                          resizing.current = null; setPreview(null);
                          if (handle.hasPointerCapture(pointerId)) handle.releasePointerCapture(pointerId);
                          if (width !== null && valid()) resize(windowId, width);
                        });
                      resizing.current = { valid, cancel };
                    }} />
                </div>;
              })}
              {!page.columns.length && <p className="muted">此页没有平铺窗口。</p>}
            </div>
          </div>
          {page.floatingWindows.length > 0 && <div className="overview-floating"><h3>浮动窗口</h3><div>{page.floatingWindows.map((windowId) => card(windowId, page))}</div></div>}
        </section>)}
        <button disabled={blocked || !ready} onClick={() => void run({ type: 'addPage', monitorId: monitor.id })}>＋ 新增页面</button>
      </section>;
    })}
  </section>;
}
