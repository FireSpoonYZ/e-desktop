import { useCallback, useEffect, useRef, useState } from 'react';
import { desktopAvailable, dismissSurface, execute, getSnapshot, onSnapshot, onSurfaceOpened, openSurface, quit, syncPreviews } from './bridge';
import type { Surface } from './bridge';
import { emptySnapshot } from './model';
import type { OnCommand } from './model';
import { TopBar } from './shell';
import { Overview } from './overview';
import { CommandPalette } from './commands';

const query = new URLSearchParams(location.search);
const surface = query.get('surface') ?? 'topbar';
const monitorIndex = Number(query.get('monitorIndex') ?? 0);
const errorMessage = (cause: unknown) => typeof cause === 'object' && cause !== null && 'message' in cause
  ? String(cause.message) : String(cause);

export default function App() {
  const [snapshot, setSnapshot] = useState(emptySnapshot);
  const [localError, setLocalError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [opening, setOpening] = useState(0);
  const [overlayMonitor, setOverlayMonitor] = useState<string | null>(null);
  const [previewSession, setPreviewSession] = useState<number | null>(null);
  const executing = useRef(false);

  useEffect(() => {
    if (!desktopAvailable()) {
      setLocalError('这是桌面应用界面。请使用 npm run tauri -- dev 启动，浏览器不会模拟系统窗口。');
      return;
    }
    let disposed = false;
    let receivedEvent = false;
    const cleanups: Array<() => void> = [];
    const start = async () => {
      const unlisten = await onSnapshot((next) => {
        receivedEvent = true;
        if (!disposed) setSnapshot(next);
      });
      if (disposed) { unlisten(); return; }
      cleanups.push(unlisten);
      const reopen = await onSurfaceOpened(({ monitorId, previewSession }) => {
        if (!disposed) { setOpening((value) => value + 1); setOverlayMonitor(monitorId); setPreviewSession(previewSession); setLocalError(null); }
      });
      if (disposed) { reopen(); return; }
      cleanups.push(reopen);
      const initial = await getSnapshot();
      if (!disposed && !receivedEvent) setSnapshot(initial);
    };
    void start().catch((cause: unknown) => { if (!disposed) setLocalError(errorMessage(cause)); });
    return () => { disposed = true; cleanups.forEach((cleanup) => cleanup()); };
  }, []);

  const onCommand: OnCommand = useCallback(async (command) => {
    if (executing.current) throw new Error('上一项操作尚未完成。');
    executing.current = true;
    setBusy(true);
    try {
      // The snapshot event is authoritative after startup; no local layout reducer.
      await execute(command);
      setLocalError(null);
    } catch (cause) {
      setLocalError(errorMessage(cause));
      throw cause;
    } finally {
      executing.current = false;
      setBusy(false);
    }
  }, []);

  const show = useCallback((target: Surface) => {
    void openSurface(target, snapshot.monitors[monitorIndex]?.monitor.id)
      .catch((cause: unknown) => setLocalError(errorMessage(cause)));
  }, [snapshot.monitors]);
  const onDismiss = useCallback(() => {
    if (surface === 'overview' || surface === 'commands') {
      setPreviewSession(null);
      void dismissSurface(surface).catch((cause: unknown) => setLocalError(errorMessage(cause)));
    }
  }, []);

  const displaySnapshot = localError ? {
    ...snapshot,
    errors: [...snapshot.errors, { code: 'backendUnavailable' as const, message: localError, windowId: null }],
  } : snapshot;
  const monitor = snapshot.monitors[monitorIndex];
  const selectedMonitor = surface === 'topbar' ? monitor?.monitor.id : overlayMonitor;
  const localSnapshot = { ...displaySnapshot, activeMonitor: selectedMonitor ?? null };
  const props = { snapshot: localSnapshot, onCommand, busy };

  return <main className={`desktop-surface desktop-surface--${surface}`}>
    {surface === 'overview' ? <Overview key={opening} {...props} onDismiss={onDismiss} previewSession={previewSession} syncPreviews={syncPreviews} />
      : surface === 'commands' ? <CommandPalette key={opening} {...props} onDismiss={onDismiss} />
      : <TopBar {...props} onOpenOverview={() => show('overview')} onOpenCommands={() => show('commands')}
        onQuit={() => { void quit().catch((cause: unknown) => setLocalError(errorMessage(cause))); }} />}
  </main>;
}
