import { useId } from 'react';
import type { OverviewProps, Page, WindowId } from '../model';
import { useCommand, useSurface } from '../commands/surface';
import './style.css';

export function Overview({ snapshot, onCommand, onDismiss, busy = false }: OverviewProps) {
  const id = useId();
  const { root, onKeyDown } = useSurface(onDismiss);
  const { run, pending, error } = useCommand(onCommand);
  const ready = snapshot.enabled && snapshot.backend.availability === 'ready' && snapshot.backend.capabilities.placement;
  const blocked = busy || pending;
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
        onClick={() => void run({ type: 'focusWindow', windowId }, onDismiss)}>
        <strong>{native.title || '无标题窗口'}</strong><span>{native.appName || '未知应用'}</span>
        <small>预览不可用{window.fullscreen ? ' · 布局全屏' : ''}{window.floating ? ' · 浮动' : ''}</small>
      </button>
      <label className="overview-move">移动到页面
        <select aria-label={`移动 ${native.title || '无标题窗口'} 到页面`} value=""
          disabled={blocked || !ready || destinations.length < 2}
          onChange={(event) => {
            if (event.target.value) void run({ type: 'moveWindowToPage', windowId, pageId: event.target.value });
          }}>
          <option value="" disabled>选择目标页面…</option>
          {destinations.filter((target) => target.id !== page.id).map((target) => <option key={target.id} value={target.id}>{target.label}</option>)}
        </select>
      </label>
    </article>;
  };
  return <section ref={root} tabIndex={-1} role="dialog" aria-modal="true" aria-labelledby={`${id}-title`}
    className="desktop-overview" onKeyDown={onKeyDown} aria-busy={blocked}>
    <header className="overview-header"><div><h1 id={`${id}-title`}>页面概览</h1><p className="muted">纵向切换页面 · 横向浏览窗口 · Tab 选择，Enter 进入</p></div>
      <button data-initial-focus onClick={onDismiss}>关闭 · Esc</button></header>
    {!snapshot.enabled && <p role="status">管理已暂停。启用后可进入页面、聚焦和移动窗口。</p>}
    {snapshot.backend.availability !== 'ready' && <p role="status">{snapshot.backend.message || '原生后端不可用'}</p>}
    {error && <p role="alert">{error}</p>}
    {snapshot.errors.map((item, position) => <p role="alert" key={position}>{item.message}</p>)}
    {!snapshot.monitors.length && <p role="status">尚未发现可用显示器。没有可显示的页面。</p>}
    {snapshot.monitors.map(({ monitor, pages, activePage, viewport }) => {
      const scale = .24 / Math.max(1, monitor.scaleFactor);
      return <section key={monitor.id} className="overview-monitor" aria-label={monitor.name}>
        <h2>{monitor.name}{snapshot.activeMonitor === monitor.id ? ' · 当前显示器' : ''}</h2>
        {!pages.length && <p role="status">此显示器还没有页面。</p>}
        {pages.map((page, position) => <section key={page.id} className="overview-page" aria-label={page.name} data-active={page.id === activePage}>
          <header><button aria-current={page.id === activePage ? 'page' : undefined} disabled={blocked || !ready}
            onClick={() => void run({ type: 'switchPage', monitorId: monitor.id, pageId: page.id }, onDismiss)}>
            {String(position + 1).padStart(2, '0')} · {page.name}{page.id === activePage ? ' · 当前页面' : ''}
          </button><span className="muted">{page.columns.reduce((count, column) => count + column.windows.length, 0) + page.floatingWindows.length} 个窗口</span></header>
          <div className="overview-scroll" tabIndex={0} role="region" aria-label={`${page.name} 的横向窗口列表`}>
            <div className="overview-columns" style={{ minHeight: Math.max(180, viewport.height * scale) }}>
              {page.columns.map((column) => <div key={column.id} className="overview-column" style={{ width: column.width * scale }}>
                {column.windows.map((windowId) => card(windowId, page))}
              </div>)}
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
