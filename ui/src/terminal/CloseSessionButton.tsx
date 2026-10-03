import type { Session } from './client';

export function CloseSessionButton({ session, disabled, onClose }: {
  session: Session | null;
  disabled: boolean;
  onClose: () => void;
}) {
  const running = session?.status === 'running';
  return <button className="terminal-danger" disabled={disabled || !session} onClick={() => {
    if (!session || disabled) return;
    if (!running || window.confirm('结束此 shell 及其进程？未保存的工作将丢失。关闭窗口本身不会结束 shell。')) onClose();
  }}>{running ? '结束 shell' : '移除会话'}</button>;
}
