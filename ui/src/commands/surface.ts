import { useEffect, useRef, useState } from 'react';
import type { KeyboardEvent } from 'react';
import type { Command, OnCommand } from '../model';

export function useSurface(onDismiss: () => void) {
  const root = useRef<HTMLElement>(null);
  useEffect(() => {
    const previous = document.activeElement;
    (root.current?.querySelector<HTMLElement>('[data-initial-focus]') ?? root.current)?.focus();
    return () => { if (previous instanceof HTMLElement && previous.isConnected) previous.focus(); };
  }, []);
  // Disabling a focused busy control can blur it to body, outside this key handler.
  useEffect(() => {
    const dialog = root.current;
    if (dialog?.isConnected && document.activeElement === document.body
      && document.visibilityState === 'visible' && document.hasFocus()) dialog.focus();
  });
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return;
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); onDismiss(); }
    if (event.key !== 'Tab') return;
    const controls = [...(root.current?.querySelectorAll<HTMLElement>('button:not(:disabled):not([tabindex="-1"]), input:not(:disabled), select:not(:disabled), summary, [tabindex="0"]') ?? [])]
      .filter((element) => element.getClientRects().length > 0
        && (element.tagName === 'SUMMARY' || !element.closest('details:not([open])')));
    const first = controls[0], last = controls.at(-1);
    if (!first) { event.preventDefault(); root.current?.focus(); }
    else if (event.shiftKey && (document.activeElement === first || document.activeElement === root.current)) {
      event.preventDefault(); last?.focus();
    } else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
  };
  return { root, onKeyDown };
}

export function useCommand(onCommand: OnCommand) {
  const locked = useRef(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (command: Command, after?: () => void) => {
    if (locked.current) return;
    locked.current = true;
    setPending(true);
    setError(null);
    try { await onCommand(command); after?.(); }
    catch (cause) {
      setError(typeof cause === 'object' && cause !== null && 'message' in cause ? String(cause.message) : String(cause));
    } finally { locked.current = false; setPending(false); }
  };
  return { run, pending, error };
}
