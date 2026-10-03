import { parseTerminalKittyKeyboardFlags } from './orca/terminal-kitty-keyboard-flags';
import type { Snapshot } from './client';

interface Renderer {
  reset(): void;
  resize(cols: number, rows: number): void;
  write(data: string, callback: () => void): void;
}

/** Sequence validation and one bounded queue for BOTH reset/snapshot and live writes. */
export function createTerminalStream(term: Renderer, onReady: () => void, onError: (error: Error) => void) {
  let sequence = -1;
  let pendingSnapshots = 0;
  let queuedBytes = 0;
  let active = true;
  let initialized = false;
  let tail = Promise.resolve();
  function fail(message: string) {
    if (active) { active = false; onError(new Error(message)); }
  }
  function enqueue(bytes: number, operation: (done: () => void) => void) {
    queuedBytes += bytes;
    if (queuedBytes > 8 * 1024 * 1024) { fail('Terminal renderer overloaded. Reconnect to restore snapshot.'); return; }
    tail = tail.then(() => {
      if (!active) return;
      return new Promise<void>(resolve => operation(() => { queuedBytes -= bytes; resolve(); }));
    }).catch(error => fail(String(error)));
  }
  return {
    get ready() { return active && initialized && pendingSnapshots === 0; },
    snapshot(value: Snapshot) {
      if (!active) return;
      if (typeof value.ansi !== 'string' || !Number.isSafeInteger(value.seq) || value.seq < sequence
        || !Number.isInteger(value.cols) || value.cols < 2 || value.cols > 1000
        || !Number.isInteger(value.rows) || value.rows < 1 || value.rows > 1000) {
        fail('Invalid terminal snapshot. Reconnect to restore snapshot.'); return;
      }
      sequence = value.seq;
      pendingSnapshots++;
      const flags = parseTerminalKittyKeyboardFlags(value.kittyKeyboardFlags);
      // Seed BEFORE ANSI: its trailing bytes may be an unfinished escape sequence.
      const prefix = flags === undefined ? '' : `\x1b[=${flags}u`;
      enqueue(value.ansi.length, done => {
        term.reset(); term.resize(value.cols, value.rows);
        term.write(prefix + value.ansi, () => {
          initialized = true; pendingSnapshots--; done();
          if (active && pendingSnapshots === 0) onReady();
        });
      });
    },
    output(data: string, seq: number) {
      if (!active) return;
      if (Number.isSafeInteger(seq) && seq <= sequence) return;
      if (!Number.isSafeInteger(seq) || seq !== sequence + 1 || sequence < 0) {
        fail('Terminal output gap. Reconnect to restore snapshot.'); return;
      }
      sequence = seq;
      enqueue(data.length, done => term.write(data, done));
    },
    dispose() { active = false; },
  };
}
