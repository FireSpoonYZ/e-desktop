// Orca: src/main/daemon/terminal-modes.ts
// de8bffe24045b396212f4f63de8960ec8380ea07 | MIT Copyright (c) 2026 Lovecast Inc.
// Adaptation: type-only imports narrowed to terminal-modes; otherwise unchanged.
export type TerminalModes = {
  bracketedPaste: boolean
  mouseTracking: boolean
  mouseTrackingMode?: 'none' | 'x10' | 'vt200' | 'drag' | 'any'
  sgrMouseMode?: boolean
  sgrMousePixelsMode?: boolean
  applicationCursor: boolean
  alternateScreen: boolean
  /** Kitty keyboard protocol flags used only to reseed a warm daemon emulator. */
  kittyKeyboardFlags?: number
}
