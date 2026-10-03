import type { Command, OnCommand, PageRailProps, Snapshot, TopBarProps } from '../model';
import { ScrollControls } from './ScrollControls';
import { selectedMonitor } from './scroll';
import './shell.css';

function currentMonitor(snapshot: Snapshot, monitorId?: string) {
  // An explicitly selected monitor disappearing must not redirect commands elsewhere.
  return monitorId !== undefined
    ? snapshot.monitors.find(({ monitor }) => monitor.id === monitorId)
    : snapshot.monitors.find(({ monitor }) => monitor.id === snapshot.activeMonitor)
      ?? snapshot.monitors.find(({ monitor }) => monitor.primary)
      ?? snapshot.monitors[0];
}

async function runCommand(onCommand: OnCommand, command: Command) {
  try { await onCommand(command); } catch { /* App retains the visible error. */ }
}

function backendNotice(snapshot: Snapshot) {
  switch (snapshot.backend.availability) {
    case 'permissionRequired': return '需要系统权限';
    case 'unsupportedSession': return '当前桌面会话不受支持';
    case 'notImplemented': return '原生后端尚未实现';
    case 'unavailable': return '原生后端不可用';
    case 'ready':
      if (!snapshot.backend.capabilities.enumerate) return '后端无法读取窗口';
      if (!snapshot.backend.capabilities.placement) return '后端不支持窗口平铺';
      if (!snapshot.backend.capabilities.minimize) return '后端不支持窗口最小化与还原';
      if (!snapshot.monitors.length) return '未检测到显示器';
      return snapshot.enabled ? '正在平铺' : '已暂停 · 窗口由系统管理';
  }
}

export function TopBar({ snapshot, onCommand, busy = false, busyCommand, onOpenOverview, onOpenCommands, onOpenTerminals, onQuit, onTogglePin }: TopBarProps) {
  const monitor = selectedMonitor(snapshot);
  const page = monitor?.pages.find(({ id }) => id === monitor.activePage);
  const error = snapshot.errors.at(-1);
  const canEnable = snapshot.backend.availability === 'ready'
    && snapshot.backend.capabilities.enumerate && snapshot.backend.capabilities.placement
    && snapshot.backend.capabilities.minimize && snapshot.monitors.length > 0;
  const notice = backendNotice(snapshot);
  const pinned = !!monitor && snapshot.pinnedBars.includes(monitor.monitor.id);
  const pageEmpty = page && !page.columns.some(({ windows }) => windows.length)
    && !page.floatingWindows.length;

  return <header className="shell-bar" aria-label="桌面窗口控制" aria-busy={busy}>
    <strong className="shell-brand">e-desktop</strong>
    <PageRail snapshot={snapshot} onCommand={onCommand} monitorId={monitor?.monitor.id ?? ''} busy={busy} />
    <span className="shell-location" title={`${monitor?.monitor.name ?? '无显示器'} / ${page?.name ?? '无页面'}`}>
      {monitor?.monitor.name ?? '无显示器'}<span className="shell-muted"> / </span>{page?.name ?? '无页面'}
    </span>
    <button type="button" className="shell-toggle" aria-pressed={snapshot.enabled}
      disabled={busy || (!snapshot.enabled && !canEnable)}
      title={snapshot.enabled ? '停止管理并还原真实窗口' : canEnable ? '开始管理真实窗口' : notice}
      onClick={() => runCommand(onCommand, { type: snapshot.enabled ? 'disable' : 'enable' })}>
      {snapshot.enabled ? '暂停并还原' : '启动平铺'}
    </button>
    <button type="button" onClick={onOpenOverview}>概览</button>
    <button type="button" onClick={onOpenCommands}>命令</button>
    {onOpenTerminals && <button type="button" onClick={onOpenTerminals}>终端</button>}
    <ScrollControls snapshot={snapshot} onCommand={onCommand} busy={busy} busyCommand={busyCommand} />
    <button type="button" disabled={busy} onClick={() => runCommand(onCommand, { type: 'refresh' })}
      title="重新读取后端状态与真实窗口">刷新</button>
    {onTogglePin && <button type="button" className="shell-pin" onClick={onTogglePin} aria-pressed={pinned}
      title={pinned ? '取消固定：此屏控制栏自动收起，鼠标移到屏幕顶端时显示' : '固定：此屏控制栏常驻并占用顶部空间'}>
      {pinned ? '已固定' : '固定'}
    </button>}
    {onQuit && <button type="button" onClick={onQuit} title="还原窗口后退出；还原失败时保留应用供重试">退出</button>}
    <span className={`shell-feedback${error ? ' shell-error' : ''}`} role={error ? 'alert' : 'status'}
      title={error ? `${error.code}: ${error.message}` : `${notice}${snapshot.backend.message ? ` · ${snapshot.backend.message}` : ''}`}>
      {error ? `操作失败：${error.message}` : notice}
      {!error && pageEmpty && snapshot.backend.availability === 'ready' ? ' · 当前页面无窗口' : ''}
    </span>
  </header>;
}

export function PageRail({ snapshot, onCommand, monitorId, busy = false }: PageRailProps) {
  const monitor = currentMonitor(snapshot, monitorId);
  const ready = snapshot.backend.availability === 'ready';
  const blocked = busy || !ready || (snapshot.enabled && !snapshot.backend.capabilities.placement);
  return <nav className="shell-rail" aria-label={`${monitor?.monitor.name ?? '显示器'}页面`} aria-busy={busy}>
    <div className="shell-pages">
      {monitor?.pages.map((page, index) => <button type="button" key={page.id}
        className="shell-page" aria-current={page.id === monitor.activePage ? 'page' : undefined}
        aria-label={`第 ${index + 1} 页：${page.name}`} title={`${index + 1} · ${page.name}`}
        disabled={blocked}
        onClick={() => runCommand(onCommand, { type: 'switchPage', monitorId: monitor.monitor.id, pageId: page.id })}>
        {index + 1}
      </button>)}
      {!monitor?.pages.length && <span className="shell-rail-empty" role="status">
        {!ready ? backendNotice(snapshot) : monitor ? '无页面' : '无显示器'}
      </span>}
    </div>
    <button type="button" className="shell-page" disabled={busy || !ready || !monitor}
      aria-label="新增页面" title="新增页面"
      onClick={() => { if (monitor) return runCommand(onCommand, { type: 'addPage', monitorId: monitor.monitor.id }); }}>+</button>
  </nav>;
}
