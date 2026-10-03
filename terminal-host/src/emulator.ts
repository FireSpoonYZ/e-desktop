// Public snapshot compatibility adapter over the pinned Orca emulator.
// The Session's internal emulator remains non-authoritative; this host view
// answers only live writes, with replies routed back through Session.write.
import { HeadlessEmulator as OrcaHeadlessEmulator } from './orca/src/main/daemon/headless-emulator'

export class HeadlessEmulator {
  private readonly core: OrcaHeadlessEmulator
  constructor(cols: number, rows: number, onQueryReply?: (reply: string) => void) {
    this.core = new OrcaHeadlessEmulator({ cols, rows, scrollback: 5000, onQueryReply })
  }
  // Existing host consumers/tests inspect the headless terminal model.
  get terminal(): any { return (this.core as any).terminal }
  get serializer(): any { return (this.core as any).serializer }
  installConptyPrimaryDeviceAttributesOverride() {
    this.core.installConptyPrimaryDeviceAttributesOverride()
  }
  write(data: string, options: { forwardQueryReplies?: boolean } = {}) {
    return this.core.write(data, options)
  }
  resize(cols: number, rows: number) { this.core.resize(cols, rows) }
  getSnapshot(seq: number) {
    const snapshot = this.core.getSnapshot({ scrollbackRows: 5000 })
    const kittyKeyboardFlags = snapshot.modes.kittyKeyboardFlags
    return {
      ansi: snapshot.scrollbackAnsi + snapshot.rehydrateSequences + snapshot.snapshotAnsi
        + '\x1b[=' + kittyKeyboardFlags + ';1u' + (snapshot.pendingEscapeTailAnsi ?? ''),
      cols: snapshot.cols, rows: snapshot.rows, seq, kittyKeyboardFlags
    }
  }
  dispose() {
    this.core.disableQueryReplyForwarding()
    this.core.dispose()
  }
}
