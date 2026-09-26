import { useState } from 'react';
import { execute } from './bridge';
import { emptySnapshot } from './model';
import type { OnCommand } from './model';
import { TopBar, PageRail } from './shell';
import { Overview } from './overview';
import { CommandPalette } from './commands';

export default function App() {
  const [snapshot, setSnapshot] = useState(emptySnapshot);
  const [error, setError] = useState<string | null>(null);
  const onCommand: OnCommand = async (command) => {
    try {
      setSnapshot(await execute(command));
      setError(null);
    } catch (cause) {
      setError(typeof cause === 'object' && cause !== null && 'message' in cause
        ? String(cause.message) : String(cause));
    }
  };
  // Native show/hide and subscriptions belong to the integration owner.
  const onDismiss = () => setError('基线尚未接入原生窗口显示/隐藏。');
  const props = { snapshot, onCommand };
  const surface = new URLSearchParams(location.search).get('surface');
  return <>
    {surface === 'pagerail' ? <PageRail {...props} />
      : surface === 'overview' ? <Overview {...props} onDismiss={onDismiss} />
      : surface === 'commands' ? <CommandPalette {...props} onDismiss={onDismiss} />
      : <TopBar {...props} onOpenOverview={onDismiss} onOpenCommands={onDismiss} />}
    {error && <p role="alert">{error}</p>}
  </>;
}
