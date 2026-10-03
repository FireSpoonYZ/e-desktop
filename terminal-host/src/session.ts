import { mkdirSync } from 'node:fs'
import { resolve } from 'node:path'
import { Session } from './orca/src/main/daemon/session'
import { createPtySubprocess } from './orca/src/main/daemon/pty-subprocess'
import { RuntimeTerminalWriter } from './orca/src/main/runtime/runtime-terminal-writer'
import { RuntimeTerminalDriverController } from './orca/src/main/runtime/runtime-terminal-driver-controller'
import { shellPathSupportsPtyStartupBarrier } from './orca/src/main/daemon/shell-ready'
import { resolvePtyOwnerBackend } from './orca/src/shared/pty-owner-backend'

type SubprocessHandle = Awaited<ReturnType<typeof createPtySubprocess>>
type Owner = { clientId: string; clientType: 'desktop' | 'mobile' }
type HostSessionOptions = {
  sessionId: string
  cols: number
  rows: number
  subprocess: SubprocessHandle
  onData(data: string): void
  onExit(result: { exitCode: number }): void
  /** Internal contract: true only with a launch configuration that emits the marker. */
  shellReadySupported?: boolean
}

// These are Orca-owned launch overlays, not the user's normal shell/profile env.
// Delete through the upstream option, including its overlay restoration policy.
export const inheritedAgentEnvToDelete = [
  'ORCA_CODEX_LAUNCH_PREFLIGHT', 'ORCA_CODEX_LAUNCH_PREFLIGHT_CMD_QUOTE',
  'ORCA_CODEX_HOME', 'ORCA_OPENCODE_CONFIG_DIR', 'ORCA_MIMOCODE_HOME',
  'ORCA_OMP_STATUS_EXTENSION', 'ORCA_AGENT_TEAMS_SHIM_DIR', 'ORCA_REMOTE_CLI_BIN_DIR',
  'ORCA_AGENT_HOOK_ENDPOINT', 'ORCA_AGENT_HOOK_ENV', 'ORCA_AGENT_TEAMS_TEAM_ID',
  'ORCA_SHELL_FEATURES', 'ORCA_SHELL_READY_ROOT'
]

// Upstream wrapper/preflight helpers read process.env rather than opts.env.
// Scope that legitimate binding to a serialized launch; never write user Orca data.
let launchQueue = Promise.resolve()
export async function createHostSession(options: {
  sessionId: string; executable: string; cwd: string; cols: number; rows: number
  dataDir: string; terminalShellArgs: string[]
  onData(data: string): void; onExit(result: { exitCode: number }): void
}): Promise<HostSession> {
  const launch = launchQueue.then(async () => {
    const storage = resolve(options.dataDir, 'orca-runtime')
    mkdirSync(storage, { recursive: true, mode: 0o700 })
    const previous = process.env.ORCA_USER_DATA_PATH
    process.env.ORCA_USER_DATA_PATH = storage
    try {
      const subprocess = await createPtySubprocess({
        sessionId: options.sessionId, shellOverride: options.executable,
        cwd: options.cwd, cols: options.cols, rows: options.rows,
        terminalShellArgs: options.terminalShellArgs,
        env: { ORCA_USER_DATA_PATH: storage },
        envToDelete: inheritedAgentEnvToDelete
      })
      return new HostSession({ ...options, subprocess, shellReadySupported: false })
    } finally {
      if (previous === undefined) delete process.env.ORCA_USER_DATA_PATH
      else process.env.ORCA_USER_DATA_PATH = previous
    }
  })
  launchQueue = launch.then(() => undefined, () => undefined)
  return launch
}

/** Host's one-owner protocol mapped to Orca's Session and write callbacks.
 * No daemon RPC tower or implicit mobile takeover; claims/releases remain server-owned. */
export class HostSession {
  private readonly session: Session
  private readonly writer: RuntimeTerminalWriter
  private readonly driver: RuntimeTerminalDriverController
  private owner: Owner | null = null
  private readonly subprocess: SubprocessHandle
  private readonly token: symbol

  constructor(options: HostSessionOptions) {
    this.subprocess = options.subprocess
    this.session = new Session({
      sessionId: options.sessionId, cols: options.cols, rows: options.rows, scrollback: 5000,
      subprocess: options.subprocess,
      ownerBackend: resolvePtyOwnerBackend({
        platform: process.platform, shellPath: options.subprocess.shellPath
      }),
      shellReadySupported: (options.shellReadySupported ?? false)
        && (options.subprocess.shellPath === undefined
          || shellPathSupportsPtyStartupBarrier(options.subprocess.shellPath)),
      onExit: exitCode => options.onExit({ exitCode })
    })
    this.token = this.session.attachClient({
      onData: data => {
        if (data) options.onData(data)
        // This host consumes live output directly. Drain Orca's record queue;
        // cold snapshots remain the host's existing public snapshot protocol.
        this.session.takePendingOutput(false)
      },
      onExit: () => {} // The Session onExit above is the single physical-exit sink.
    })
    // A subprocess may have buffered data before Session installed its listeners.
    const pending = this.session.takePendingOutput(false)
    for (const record of pending?.records ?? []) {
      if (record.kind === 'output' && record.data) options.onData(record.data)
    }
    this.driver = new RuntimeTerminalDriverController({
      notifyChanged: () => {}, // Host control events remain emitted by claim/release.
      canClaimMobileFloor: (_id, clientId) =>
        this.owner?.clientType === 'mobile' && this.owner.clientId === clientId,
      commitMobileFloor: async (_id, clientId, _previous, isCurrent) => {
        if (isCurrent()) this.requireOwner(clientId)
      }
    })
    this.writer = new RuntimeTerminalWriter((_id, data) => {
      if (!this.session.isAlive) return false
      this.session.write(data)
      return true // In-process acceptance, not a ConPTY consumer acknowledgement.
    })
  }

  get pid() { return this.session.pid }
  get shellState() { return this.session.shellState }
  setOwner(owner: Owner | null) {
    this.owner = owner
    this.driver.set(this.session.sessionId, owner
      ? owner.clientType === 'mobile' ? { kind: 'mobile', clientId: owner.clientId } : { kind: 'desktop' }
      : { kind: 'idle' })
  }
  private requireOwner(clientId: string) {
    if (this.owner?.clientId !== clientId) {
      throw Object.assign(new Error('Claim terminal control first'), { code: 'NOT_OWNER' })
    }
  }
  async send(data: string | Buffer, clientId: string) {
    let claim: ReturnType<RuntimeTerminalDriverController['beginMobileInputFloor']> = null
    try {
      await this.writer.writeAction(this.session.sessionId,
        typeof data === 'string' ? { text: data } : {}, data as string, {
          inputKind: 'driving',
          reserveWrite: () => {
            this.requireOwner(clientId)
            if (this.owner?.clientType === 'mobile') {
              claim = this.driver.beginMobileInputFloor(this.session.sessionId, clientId)
              if (!claim) throw new Error('mobile_input_floor_unavailable')
            }
          },
          afterWrite: async () => {
            await claim?.commit()
            claim = null
          }
        })
    } catch (error) {
      claim?.rollback()
      throw error
    }
  }
  // Raw Buffer input uses writeAction's unchunked payload and Session's real
  // startup queue. Its classifier rejects numeric Buffer[0], and the queue
  // retains the object unmodified through subprocess.write -> node-pty.write.
  // Text and generated query replies remain strings (upstream reply policy).
  write(reply: string) { this.session.write(reply) }
  resize(cols: number, rows: number) { this.session.resize(cols, rows) }
  pause() { this.session.pauseProducer('stream') }
  resume() { this.session.resumeProducer('stream') }
  // ONLY termination.ts calls this after fresh owned-tree identity proof.
  // This is the captured own native handle operation, never a PID lookup/kill.
  kill() { this.subprocess.kill() }
  dispose() {
    if (this.session.isAlive) throw new Error('Cannot dispose a physically live Session')
    this.session.detachClient(this.token)
    this.session.dispose()
    this.driver.clear(this.session.sessionId)
    this.owner = null
  }
}
