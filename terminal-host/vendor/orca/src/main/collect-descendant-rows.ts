// Orca: src/main/pty-descendant-termination.ts
// de8bffe24045b396212f4f63de8960ec8380ea07 | MIT Copyright (c) 2026 Lovecast Inc.
// Adaptation: collectDescendantRows excerpt only; inferred return type avoids daemon graph.
import type { ProcessTableRow } from './pty-process-table-parser'
export function collectDescendantRows(
  rootPid: number,
  table: ProcessTableRow[],
  capturedAtMs = Date.now()
) {
  const childrenByPpid = new Map<number, ProcessTableRow[]>()
  let rootRow: ProcessTableRow | null = null
  let duplicateRoot = false
  for (const row of table) {
    if (row.pid === rootPid) {
      // A non-atomic process-table read can contain both an old and a recycled
      // root row. There is no safe identity to retain in that case.
      duplicateRoot = rootRow !== null
      rootRow ??= row
      continue
    }
    const siblings = childrenByPpid.get(row.ppid)
    if (siblings) {
      siblings.push(row)
    } else {
      childrenByPpid.set(row.ppid, [row])
    }
  }
  // Why: a ppid walk is only meaningful while the root is alive in this snapshot.
  // An absent root has already exited — its real descendants reparent to pid 1 and
  // become unreachable by ppid, so any rows still pointing at the vacated PID are a
  // PID-reuse coincidence. Sweeping them could signal an unrelated process, so bail.
  if (!rootRow || duplicateRoot) {
    return { rootPgid: null, descendants: [], capturedAtMs }
  }
  const descendants: ProcessTableRow[] = []
  const queue = [rootPid]
  const visited = new Set(queue)
  for (let nextIndex = 0; nextIndex < queue.length; nextIndex += 1) {
    const pid = queue[nextIndex]
    for (const child of childrenByPpid.get(pid) ?? []) {
      // Why: ps is not an atomic snapshot. PID reuse can produce duplicate or
      // cyclic-looking rows, which must not hang the Electron main thread.
      if (visited.has(child.pid)) {
        continue
      }
      visited.add(child.pid)
      descendants.push(child)
      queue.push(child.pid)
    }
  }
  return {
    root: { pid: rootRow.pid, startedAt: rootRow.startedAt },
    rootPgid: rootRow.pgid,
    descendants,
    capturedAtMs,
    reDerivedPids: new Set(descendants.map((row) => row.pid))
  }
}
