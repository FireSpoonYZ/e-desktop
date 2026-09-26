import type { OverviewProps } from '../model';

export function Overview({ onDismiss }: OverviewProps) {
  return <section aria-label="窗口概览" className="empty-state">
    <h1>尚未接入原生窗口</h1>
    <p>当前基线不移动或接管其他应用。</p>
    <button onClick={onDismiss}>关闭概览</button>
  </section>;
}
