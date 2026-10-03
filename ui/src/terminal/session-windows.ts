import type { Session, TerminalClient } from './client';

/** One primary-topbar observer; seen IDs survive reconnects and manual view detach. */
export function observeSessionWindows(
  client: Pick<TerminalClient, 'request' | 'onEvent'>,
  seen: Set<string>,
  open: (sessionId: string) => Promise<void>,
  onError: (error: unknown) => void,
) {
  let active = true;
  let refreshing = false;
  let dirty = false;
  async function refresh() {
    dirty = true;
    if (refreshing) return;
    refreshing = true;
    try {
      while (active && dirty) {
        dirty = false;
        const sessions = await client.request<Session[]>('terminal.list');
        for (const session of sessions) {
          if (!active) return;
          if (seen.has(session.id)) continue;
          // Mark before opening: repeated events must not refocus or reopen detached views.
          seen.add(session.id);
          if (session.status === 'running') {
            try { await open(session.id); } catch (error) { if (active) onError(error); }
          }
        }
      }
    } catch (error) { if (active) onError(error); }
    finally { refreshing = false; }
  }
  const unlisten = client.onEvent(event => {
    if (event.event === 'terminal.listChanged') void refresh();
  });
  void refresh();
  return () => { active = false; unlisten(); };
}
