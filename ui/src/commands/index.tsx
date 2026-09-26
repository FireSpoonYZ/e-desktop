import { useEffect, useId, useRef, useState } from 'react';
import type { CommandPaletteProps } from '../model';
import { commandActions, nextSelection, paletteKey } from './actions';
import { useCommand, useSurface } from './surface';
import './style.css';

export function CommandPalette({ snapshot, onCommand, onDismiss, busy = false }: CommandPaletteProps) {
  const [query, setQuery] = useState('');
  const [selection, setSelection] = useState(0);
  const composing = useRef(false);
  const id = useId();
  const { root, onKeyDown } = useSurface(onDismiss);
  const { run, pending, error } = useCommand(onCommand);
  const actions = commandActions(snapshot, query);
  const index = Math.min(selection, Math.max(0, actions.length - 1));
  const blocked = busy || pending;
  useEffect(() => {
    document.getElementById(`${id}-option-${index}`)?.scrollIntoView({ block: 'nearest' });
  }, [id, index, query]);
  const execute = (position: number) => {
    const action = actions[position];
    if (action && !action.disabled && !blocked) void run(action.command, onDismiss);
  };
  return <section ref={root} tabIndex={-1} role="dialog" aria-modal="true" aria-labelledby={`${id}-title`}
    className="command-palette" onKeyDown={onKeyDown} aria-busy={blocked}>
    <header><h1 id={`${id}-title`}>窗口与命令</h1><button onClick={onDismiss} aria-label="关闭命令面板">关闭 · Esc</button></header>
    <label htmlFor={`${id}-input`} className="muted">搜索标题、应用或布局命令</label>
    <input id={`${id}-input`} data-initial-focus role="combobox" aria-autocomplete="list" aria-expanded="true"
      aria-controls={`${id}-results`} aria-activedescendant={actions.length ? `${id}-option-${index}` : undefined}
      autoComplete="off" value={query} placeholder="搜索窗口 / 宽度 / 浮动…"
      onChange={(event) => { setQuery(event.target.value); setSelection(0); }}
      onCompositionStart={() => { composing.current = true; }} onCompositionEnd={() => { composing.current = false; }}
      onKeyDown={(event) => {
        const key = paletteKey(event.key, composing.current || event.nativeEvent.isComposing, event.nativeEvent.keyCode);
        if (!key) {
          if (composing.current || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) event.stopPropagation();
          return;
        }
        event.preventDefault(); event.stopPropagation();
        if (key === 'dismiss') onDismiss();
        else if (key === 'execute') execute(index);
        else setSelection(nextSelection(index, key === 'next' ? 1 : -1, actions.length));
      }} />
    <p className="muted command-help">↑ ↓ 选择 · Enter 执行 · Esc 关闭{!snapshot.enabled && ' · 管理已暂停'}</p>
    {snapshot.backend.availability !== 'ready' && <p role="status">{snapshot.backend.message || '原生后端不可用'}</p>}
    <ul id={`${id}-results`} role="listbox" aria-label="窗口和布局命令" className="command-results">
      {actions.map((action, position) => <li key={action.id} id={`${id}-option-${position}`} role="option"
        aria-selected={position === index} aria-disabled={action.disabled || blocked}
        onMouseDown={(event) => event.preventDefault()} onClick={() => { setSelection(position); execute(position); }}>
        <span>{action.label}</span><small>{action.detail}{action.disabled ? ' · 当前不可用' : ''}</small>
      </li>)}
    </ul>
    {!actions.length && <p role="status">没有匹配的窗口或命令。</p>}
    {!snapshot.windows.length && !query && <p className="muted">当前没有可搜索的原生窗口。</p>}
    {(error || snapshot.errors.length > 0) && <div className="command-errors">
      {error && <p role="alert">{error}</p>}
      {snapshot.errors.map((item, position) => <p role="alert" key={position}>{item.message}</p>)}
    </div>}
  </section>;
}
