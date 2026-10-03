import { useCallback, useEffect, useRef, useState } from 'react';
import { desktopAvailable, dismissSurface, execute, getSnapshot, onSnapshot, onSurfaceOpened, openSurface, quit, setBarPinned, syncPreviews } from './bridge';
import type { Surface } from './bridge';
import { emptySnapshot } from './model';
import type { Command, OnCommand } from './model';
import { TopBar } from './shell';
import { Overview } from './overview';
import { CommandPalette } from './commands';
import { TerminalManager, TerminalView } from './terminal/Terminal';
import { openTerminal, observeTerminalConnection } from './terminal/client';
import { observeSessionWindows } from './terminal/session-windows';

const query = new URLSearchParams(location.search);
const surface = query.get('surface') ?? 'topbar';
const monitorIndex = Number(query.get('monitorIndex') ?? 0);
const errorMessage = (cause: unknown) => typeof cause === 'object' && cause !== null && 'message' in cause
  ? String(cause.message) : String(cause);

export default function App() {
  if (surface === 'terminals') return <TerminalManager />;
  if (surface === 'terminal') return <TerminalView sessionId={query.get('sessionId') ?? ''} />;
  return <DesktopApp />;
}

function DesktopApp() {
  const [snapshot, setSnapshot] = useState(emptySnapshot);
  const [terminalError, setTerminalError] = useState<string | null>(null);
  const [localError, setLocalError] = useState<string | null>(null);
  const [busyCommand, setBusyCommand] = useState<Command['type'] | null>(null);
  const busy = busyCommand !== null;
  const [opening, setOpening] = useState(0);
  const [overlayMonitor, setOverlayMonitor] = useState<string | null>(null);
  const [previewSession, setPreviewSession] = useState<number | null>(null);
  const executing = useRef(false);
  const seenTerminalSessions = useRef(new Set<string>());

  useEffect(() => {
    // control_label(0) is the single persistent primary topbar, not each monitor's bar.
    if (surface !== 'topbar' || monitorIndex !== 0 || !desktopAvailable()) return;
    let disposed = false;
    let stopObserving = () => {};
    const report = (cause: unknown) => { if (!disposed) setTerminalError(errorMessage(cause)); };
    const stopConnection = observeTerminalConnection(current => {
      stopObserving(); setTerminalError(null);
      stopObserving = observeSessionWindows(current, seenTerminalSessions.current, openTerminal, report);
    }, cause => {
      stopObserving(); report(cause);
    });
    return () => {
      disposed = true; stopObserving(); stopConnection();
    };
  }, []);

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
    setBusyCommand(command.type);
    try {
      // The snapshot event is authoritative after startup; no local layout reducer.
      await execute(command);
      setLocalError(null);
    } catch (cause) {
      setLocalError(errorMessage(cause));
      throw cause;
    } finally {
      executing.current = false;
      setBusyCommand(null);
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

  const displaySnapshot = localError || terminalError ? {
    ...snapshot,
    errors: [...snapshot.errors, ...[localError, terminalError].filter((value): value is string => !!value)
      .map(message => ({ code: 'backendUnavailable' as const, message, windowId: null }))],
  } : snapshot;
  const monitor = snapshot.monitors[monitorIndex];
  const selectedMonitor = surface === 'topbar' ? monitor?.monitor.id : overlayMonitor;
  const localSnapshot = { ...displaySnapshot, activeMonitor: selectedMonitor ?? null };
  const props = { snapshot: localSnapshot, onCommand, busy, busyCommand: busyCommand ?? undefined };

  return <main className={`desktop-surface desktop-surface--${surface}`}>
    {surface === 'overview' ? <Overview key={opening} {...props} onDismiss={onDismiss} previewSession={previewSession} syncPreviews={syncPreviews} />
      : surface === 'commands' ? <CommandPalette key={opening} {...props} onDismiss={onDismiss} />
      : <TopBar {...props} onOpenTerminals={() => { void openTerminal().catch(cause => setLocalError(errorMessage(cause))); }} onOpenOverview={() => show('overview')} onOpenCommands={() => show('commands')}
        onQuit={() => { void quit().catch((cause: unknown) => setLocalError(errorMessage(cause))); }}
        onTogglePin={monitor && snapshot.barsAutohide ? () => {
          void setBarPinned(monitor.monitor.id, !snapshot.pinnedBars.includes(monitor.monitor.id))
            .catch((cause: unknown) => setLocalError(errorMessage(cause)));
        } : undefined} />}
  </main>;
}
