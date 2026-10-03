// Orca: src/main/daemon/terminal-snapshot-ansi-buffers.ts
// de8bffe24045b396212f4f63de8960ec8380ea07 | MIT Copyright (c) 2026 Lovecast Inc.
// Adaptation: type-only imports narrowed to terminal-modes; otherwise unchanged.
import type { TerminalModes } from './terminal-modes'

export function splitTerminalSnapshotAnsi(
  snapshotAnsi: string,
  modes: TerminalModes
): { snapshotAnsi: string; scrollbackAnsi: string } {
  if (!modes.alternateScreen) {
    return { snapshotAnsi, scrollbackAnsi: '' }
  }
  const alternateScreenMarker = '\x1b[?1049h'
  const start = snapshotAnsi.lastIndexOf(alternateScreenMarker)
  if (start === -1) {
    return { snapshotAnsi, scrollbackAnsi: '' }
  }
  // Why: rehydrateSequences owns the alt-screen transition. Keeping the
  // normal buffer separate lets an already-alt renderer rebuild it safely.
  return {
    scrollbackAnsi: snapshotAnsi.slice(0, start),
    snapshotAnsi: snapshotAnsi.slice(start + alternateScreenMarker.length)
  }
}
