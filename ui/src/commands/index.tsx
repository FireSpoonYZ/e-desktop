import type { CommandPaletteProps } from '../model';

export function CommandPalette({ onDismiss }: CommandPaletteProps) {
  return <section aria-label="命令" className="empty-state">
    <h1>命令面板尚未实现</h1>
    <button onClick={onDismiss}>关闭命令面板</button>
  </section>;
}
