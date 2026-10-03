import { useEffect, useId, useRef } from 'react';
import { formatKey, groupHotkeys } from './describe';
import type { Hotkey } from './describe';
import './style.css';

/** lane: ui-animation — niri hotkey overlay. Esc, a click, losing focus or the key again closes it. */
export function HotkeyOverlay({ hotkeys, onDismiss }: { hotkeys: Hotkey[] | null; onDismiss: () => void }) {
  const id = useId();
  const root = useRef<HTMLElement>(null);
  const dismiss = useRef(onDismiss);
  dismiss.current = onDismiss;
  useEffect(() => {
    root.current?.focus();
    // Clicking another window blurs the overlay; it closes without taking focus back.
    const blur = () => dismiss.current();
    window.addEventListener('blur', blur);
    return () => window.removeEventListener('blur', blur);
  }, []);
  const groups = groupHotkeys(hotkeys ?? []);
  const toggle = hotkeys?.find(({ action }) => action.type === 'hotkeyOverlay');
  return <section ref={root} tabIndex={-1} role="dialog" aria-modal="true" aria-labelledby={`${id}-title`}
    className="hotkey-overlay" onClick={() => onDismiss()}
    onKeyDown={(event) => { if (event.key === 'Escape') { event.preventDefault(); onDismiss(); } }}>
    <header><h1 id={`${id}-title`}>快捷键</h1>
      <p className="muted">按 <kbd>Esc</kbd>{toggle && <>、<kbd>{formatKey(toggle.key)}</kbd></>} 或点击关闭</p></header>
    {groups.length ? <div className="hotkey-groups">{groups.map((group) => <section key={group.title}>
      <h2>{group.title}</h2>
      <dl>{group.rows.map((row) => <div key={row.label} className="hotkey-row">
        <dt>{row.keys.map((key) => <kbd key={key}>{key}</kbd>)}</dt><dd>{row.label}</dd>
      </div>)}</dl>
    </section>)}</div> : <p role="status">当前没有生效的全局快捷键。</p>}
  </section>;
}
