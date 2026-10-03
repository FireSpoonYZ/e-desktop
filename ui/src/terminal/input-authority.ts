import type { Terminal } from '@xterm/xterm';
import { extractOnlyTerminalQueryReplies } from './orca/terminal-query-reply';

interface InputCore {
  triggerDataEvent(data: string, wasUserInput?: boolean): void;
}

/**
 * e-desktop adapter for pinned @xterm/xterm 6.1.0-beta.303 (Orca ESM patch).
 * Public onData loses input provenance: modified F3 and CPR have identical bytes.
 * Keep Orca's classifier unchanged, but never classify known real input as a reply.
 */
export function attachTerminalInputAuthority(term: Terminal, send: (data: string) => void) {
  const core = (term as unknown as { _core?: { coreService?: InputCore } })._core?.coreService;
  if (!core || typeof core.triggerDataEvent !== 'function') {
    throw new Error('Unsupported xterm input core. Reinstall the pinned Orca-patched xterm dependency.');
  }
  const original = core.triggerDataEvent;
  let userInput = false;
  const wrapped = function(this: InputCore, data: string, wasUserInput = false) {
    const previous = userInput;
    userInput = wasUserInput;
    try { original.call(this, data, wasUserInput); }
    finally { userInput = previous; }
  };
  core.triggerDataEvent = wrapped;
  const input = term.onData(data => {
    if (userInput || !extractOnlyTerminalQueryReplies(data)) send(data);
  });
  return () => {
    input.dispose();
    if (core.triggerDataEvent === wrapped) core.triggerDataEvent = original;
  };
}
