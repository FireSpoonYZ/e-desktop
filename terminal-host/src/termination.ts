import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { join } from 'node:path'
import { collectDescendantRows } from '../vendor/orca/src/main/collect-descendant-rows'
import { parseProcessTable } from '../vendor/orca/src/main/pty-process-table-parser'
import { hasUnambiguousStartTime } from '../vendor/orca/src/main/has-unambiguous-start-time'

const exec = promisify(execFile)
const delay = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))
export async function processTable(timeout = 2000) {
  // Boundary before the scan, never its completion time (Orca identity rule).
  const capturedAtMs = Date.now()
  const options = { timeout, maxBuffer: 32 * 1024 * 1024, windowsHide: true }
  if (process.platform === 'win32') {
    const { stdout } = await exec(join(process.env.SystemRoot ?? 'C:\\Windows', 'System32/WindowsPowerShell/v1.0/powershell.exe'),
      ['-NoProfile', '-NonInteractive', '-Command',
        'Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,@{n="Started";e={$_.CreationDate.ToUniversalTime().ToString("o")}} | ConvertTo-Json -Compress'], options)
    return JSON.parse(stdout).map((p: any) => ({
      pid: p.ProcessId, ppid: p.ParentProcessId, pgid: 0, startedAt: p.Started, capturedAtMs
    }))
  }
  const { stdout } = await exec('ps', ['-axo', 'pid=,ppid=,pgid=,sess=,stat=,lstart='], {
    ...options, env: { ...process.env, LANG: 'C', LC_ALL: 'C' }
  })
  // Zombies have already exited; only their parent's wait/reaping remains.
  const sessions = new Map<number, string>()
  const liveLines = stdout.split('\n').flatMap(line => {
    const match = line.match(/^(\s*(\d+)\s+\d+\s+\d+\s+)(\S+)\s+(\S+)\s+(.+)$/)
    if (!match || match[4].startsWith('Z')) return []
    sessions.set(Number(match[2]), match[3])
    return [match[1] + match[5]]
  })
  return parseProcessTable(liveLines.join('\n')).map(row => ({ ...row, sid: sessions.get(row.pid), capturedAtMs }))
}

// A process group is mutable (job control/setpgid), not process identity.
export function sameProcess(a: any, b: any) {
  return a.pid === b.pid && a.startedAt === b.startedAt
}

export function canSignalProcess(target: any, platform = process.platform) {
  return platform === 'win32' || hasUnambiguousStartTime(target.startedAt, target.capturedAtMs)
}

export function signalPosixTargets(targets: any[], signal: NodeJS.Signals, sendSignal = process.kill) {
  // Validate the entire batch before sending even the first signal. Matching
  // pid/lstart alone is NOT authorization for a capture-second identity.
  if (targets.some(target => !canSignalProcess(target, 'linux'))) {
    throw Object.assign(new Error('Ambiguous POSIX process identity; cleanup retained'), { code: 'CLEANUP_FAILED' })
  }
  for (const target of targets) {
    try { sendSignal(target.pid, signal) }
    catch (error: any) { if (error.code !== 'ESRCH') throw error }
  }
}

export function observeOwnership(s: any, table: any[], platform = process.platform) {
  const captured = (row: any) => ({ ...row, capturedAtMs: row.capturedAtMs ?? Date.now() })
  if (!s.rootIdentity) {
    const root = table.find(p => p.pid === s.pty.pid)
    if (!root || root.ppid !== process.pid || s.exitResult) throw new Error('Cannot prove PTY root ownership')
    s.rootIdentity = captured(root)
    s.cleanupTargets = [s.rootIdentity]
  }
  const root = s.rootIdentity
  const remember = (row: any) => {
    const index = s.cleanupTargets.findIndex((known: any) => sameProcess(known, row))
    if (index === -1) s.cleanupTargets.push(captured(row))
    else if (!canSignalProcess(s.cleanupTargets[index], platform) && row.pid !== root.pid) {
      // Only callers with fresh ancestry/session proof may renew an ambiguous
      // descendant; detached same-pid/lstart matches never reach this function.
      s.cleanupTargets[index] = captured(row)
    }
  }
  const rootNow = table.find(p => p.pid === root.pid)
  const born = Date.parse(root.startedAt)
  // Windows retains PPID after the parent dies. Bound that association to the
  // captured root's lifetime, and exclude births belonging to a recycled PID.
  const recycledAt = rootNow && !sameProcess(rootNow, root) ? Date.parse(rootNow.startedAt) : Infinity
  const ended = s.rootExitedAt ?? Date.now()
  for (const row of table) {
    const childBorn = Date.parse(row.startedAt)
    if (platform === 'win32') {
      if (row.ppid === root.pid && childBorn >= born && childBorn <= ended && childBorn < recycledAt) remember(row)
    } else if (canSignalProcess(root, platform) && root.sid !== undefined && row.sid === root.sid && childBorn >= born && childBorn < recycledAt) {
      // Ordinary reparenting/job control preserves the terminal session ID.
      remember(row)
    }
  }
  // Windows PPIDs can also outlive an intermediate parent. Do not reintroduce
  // an older, unrelated child through the ordinary live-parent tree walk.
  const byPid = new Map(table.map(row => [row.pid, row]))
  const ancestry = table.filter(row => {
    const parent = byPid.get(row.ppid)
    return !parent || Date.parse(row.startedAt) >= Date.parse(parent.startedAt)
  })
  for (const parent of table.filter(row => s.cleanupTargets.some((p: any) =>
    sameProcess(p, row) && (canSignalProcess(p, platform) ||
      (p.pid === root.pid && !s.exitResult && row.ppid === process.pid))))) {
    for (const child of collectDescendantRows(parent.pid, ancestry).descendants) remember(child)
  }
}

export async function captureOwnership(s: any) {
  observeOwnership(s, await processTable())
  if (process.platform !== 'win32' && !canSignalProcess(s.rootIdentity)) {
    // A newly spawned shell normally begins in its birth second. Wait only to
    // the next boundary, and renew root identity only while its native PTY is
    // still live. Never renew a root merely from matching coarse pid/lstart.
    const boundary = Date.parse(s.rootIdentity.startedAt) + 1000
    const wait = boundary - Date.now() + 1
    if (!Number.isFinite(wait) || wait > 1001 || s.exitResult) throw new Error('Ambiguous root identity')
    await delay(Math.max(0, wait))
    const table = await processTable()
    const root = table.find((row: any) => sameProcess(row, s.rootIdentity))
    if (s.exitResult || !root || root.ppid !== process.pid || !canSignalProcess(root)) {
      throw new Error('Cannot renew live root identity')
    }
    s.rootIdentity = root
    s.cleanupTargets = s.cleanupTargets.map((row: any) => row.pid === root.pid ? root : row)
    observeOwnership(s, table)
  }
}

// Ownership snapshots survive a failed attempt; never discard a still-live tree.
export async function terminateSession(s: any) {
  try {
    const overallDeadline = Date.now() + 7000
    await s.ownershipPromise
    const timeLeft = () => {
      const left = overallDeadline - Date.now()
      if (left <= 0) throw new Error('Cleanup deadline exceeded')
      return Math.min(left, 2000)
    }
    let table = await processTable(timeLeft())
    observeOwnership(s, table)
    const matching = () => s.cleanupTargets.filter((expected: any) =>
      table.some((p: any) => sameProcess(p, expected)))
    if (process.platform !== 'win32' && matching().some((row: any) => !canSignalProcess(row))) {
      // One bounded pre-signal recapture. observeOwnership renews descendants
      // only via fresh live-parent ancestry or an established terminal SID.
      await delay(Math.min(timeLeft(), 1001 - Date.now() % 1000))
      table = await processTable(timeLeft())
      observeOwnership(s, table)
    }
    for (const [signal, grace] of [['SIGHUP', 500], ['SIGTERM', 1000], ['SIGKILL', 2000]] as const) {
      // POSIX children before root; Windows /T starts at the live root.
      // Retain identities even after descendants are reparented.
      const live = matching()
      const windowsRoot = live.find((p: any) => p.pid === s.pty.pid)
      const targets = process.platform === 'win32' && windowsRoot ? [windowsRoot] : live.reverse()
      if (process.platform !== 'win32') signalPosixTargets(targets, signal)
      for (const target of process.platform === 'win32' ? targets : []) {
        // Native /T reaches descendants not present in the initial snapshot.
        await exec(join(process.env.SystemRoot ?? 'C:\\Windows', 'System32/taskkill.exe'),
          ['/PID', String(target.pid), '/T', '/F'], { timeout: timeLeft(), windowsHide: true })
          .catch(() => {}) // Verification below, not taskkill's racing exit status, decides success.
      }
      const deadline = Math.min(Date.now() + grace, overallDeadline)
      do {
        table = await processTable(timeLeft())
        observeOwnership(s, table)
        if (!matching().length) {
          // Fresh identity proof says the owned tree is gone. On the DLL path
          // public exit needs ConPTY/conout closed, so release this own handle
          // before waiting for exit. Tree-empty alone is NOT physical exit.
          if (process.platform === 'win32' && !s.ptyReleased) {
            s.pty.kill(); s.ptyReleased = true
          }
          if (s.exitResult) return
        }
        await delay(50)
      } while (Date.now() < deadline)
    }
    throw new Error('PTY or descendant exit could not be verified')
  } catch (error) {
    throw Object.assign(new Error('Terminal cleanup failed; session retained for retry', { cause: error }), { code: 'CLEANUP_FAILED' })
  }
}
