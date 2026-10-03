// Orca: src/main/pty-descendant-termination.ts
// de8bffe24045b396212f4f63de8960ec8380ea07 | MIT Copyright (c) 2026 Lovecast Inc.
// Adaptation: exact hasUnambiguousStartTime excerpt; no function changes.
export function hasUnambiguousStartTime(startedAt: string, capturedAtMs: number): boolean {
  const startedAtMs = Date.parse(startedAt)
  if (!Number.isFinite(startedAtMs)) {
    return false
  }
  // ps lstart is second-resolution. A process born in the capture second can
  // be replaced by a different process with the same displayed timestamp.
  return startedAtMs < Math.floor(capturedAtMs / 1_000) * 1_000
}
