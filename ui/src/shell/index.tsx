import type { PageRailProps, TopBarProps } from '../model';

export function TopBar({ snapshot, onCommand, busy }: TopBarProps) {
  return <header className="baseline-bar">
    <strong>e-desktop</strong>
    <span>{snapshot.enabled ? '正在平铺' : '已暂停'}</span>
    <span className="muted">{snapshot.backend.message}</span>
    <button disabled={busy || snapshot.backend.availability !== 'ready'}
      onClick={() => void onCommand({ type: snapshot.enabled ? 'disable' : 'enable' })}>
      {snapshot.enabled ? '暂停并还原' : '启动平铺'}
    </button>
  </header>;
}

export function PageRail(_props: PageRailProps) {
  return <nav aria-label="页面"><span className="muted">无页面</span></nav>;
}
