# Terminal host (wire v1)

## Install and run

Requires an installed **Node >=22.12** and npm. The desktop parent must resolve
an absolute Node executable; Node is not bundled. From the repository:

```sh
npm ci --prefix terminal-host
npm run build --prefix terminal-host --if-present
node /absolute/path/to/terminal-host/src/server.mjs --data-dir /private/application/data/terminals
```

The package-local prepare script applies the exact pinned Orca node-pty 1.1.0
patch, rebuilds the native addon from source with the selected external Node,
and builds `dist/emulator.mjs`, `dist/termination.mjs` and `dist/session.mjs`.
Set `E_DESKTOP_NODE` to the absolute Node executable the desktop will actually
use before preparing resources. Node is not bundled. Windows builds require
the native compiler toolchain; stock prebuilds/debug addons are not fallbacks.

Ship all three bundles, `src/*.mjs`, the deployment scripts and provenance,
production dependencies including the rebuilt node-pty/ConPTY resources, and
all retained licenses. `prepare:terminal-host` checks both source and copied
resources. Before creating private state or binding sockets, `startHost()`
checks its actual process runtime, recorded ABI/architecture/platform, native
hashes, exports, and prohibited fallback candidates. A runtime mismatch is an
explicit startup error; rebuild for the resolved runtime. Permit required
npm install scripts and keep build dependencies until the build has finished.

No runtime path depends on the working directory. Required `--data-dir` is
resolved at startup. Optional `--port` defaults to 7768 (0 selects a free port),
and `--address` defaults to 0.0.0.0. Local WS always binds 127.0.0.1 on a free port.
Pairing candidates are filtered against the actual listener address: IPv4 wildcard
advertises IPv4 only, IPv6 wildcard advertises IPv6 only (no link-local addresses),
and a specific bind advertises only that interface/address, including loopback.
The default is 7768 to avoid Orca's 6768 listener.
Open the remote TCP port in the firewall only for trusted LAN/Tailscale networks.

Stdout contains exactly one record:

```json
{"type":"ready","protocolVersion":1,"localUrl":"ws://127.0.0.1:12345","remotePort":7768,"adminToken":"SECRET"}
```

The parent consumes it privately; do not log it or place the token in a URL.
Diagnostics go to stderr. Write `shutdown\n` to stdin, close stdin, or send
SIGINT/SIGTERM for explicit application shutdown. This kills hosted PTYs.
The host begins root birth/session-identity capture immediately after spawn;
input waits for that initial observation before it can exit the shell. This
observation does not block output/subscription. Close and shutdown refresh owned
descendants before signalling. POSIX uses
bounded SIGHUP → SIGTERM → SIGKILL escalation; Windows uses native taskkill /T /F
and subsequently releases the ConPTY handle. Process identities are retained
across reparenting and retries, with live-tree refresh during verification.
Identity is PID plus creation time, never mutable process-group ID. On POSIX,
the captured terminal session ID finds ordinary members after root reparenting or
job-control group changes. On Windows, persistent PPID plus the captured root's
birth/observed-exit interval finds surviving direct children after root exit;
newer recycled-root identities bound that interval. Live owned descendants extend
the walk. Already captured descendants remain targets even after changing groups
or sessions. Root absence alone is never accepted as cleanup success; absent root
without lifecycle evidence fails and retains the session.

Deliberately detached daemons that leave terminal ownership before observation
are outside this lifecycle contract. This is bounded lifecycle evidence, not a
kernel containment/job subsystem. Known captured live descendants are never
silently dropped merely because their parent/group changed.
Each cleanup attempt has a seven-second deadline, including bounded process-table
commands; Windows node-pty's handle-release helper may add its own five-second
exit delay. Zombie processes are treated as exited (reaping belongs to their parent).
The PTY callback records exit evidence synchronously, outside the global request
queue, so close/shutdown cannot deadlock waiting behind their own request.

CLEANUP_FAILED retains the session and its captured identities for retry. Failed
shutdown keeps the host/listeners alive rather than abandoning processes; retry
terminal.close or send another shutdown signal. Successful shutdown removes only
verified-exited sessions before closing listeners. Cleanup requires ps on POSIX,
and built-in PowerShell/Get-CimInstance/taskkill on Windows; inability to obtain
process ownership/exit evidence is a failure, not assumed success.

Closing a terminal view or disconnecting a socket does **not** kill a PTY.
Disconnected owners lose control, so reconnect then explicitly claim again.

## Authentication and pairing

All contract methods use `{id,method,params}`; IDs are unique for the lifetime
of a connection. Authenticate the local connection with the readiness token.
Admin authority derives only from that token on the loopback endpoint, never
from the supplied clientType. Each active connection needs a distinct clientId.
A mobile device token can authenticate but cannot call admin methods, regardless
of its declared clientType.

`pairing.create` returns the frozen JSON-string descriptor, a 256-bit one-use
code and a five-minute expiry. Pair using remote WSS with `pair`, then `auth`.
The certificate is self-signed, CN=e-terminal; its identity is the descriptor's
**SHA256 hash of DER certificate bytes**, lowercase hex (not the SPKI hash used
by some CertificatePinner APIs). Verify that pin before sending the code or
token. DNS-name validation alone is inappropriate for changing LAN addresses;
a trust-all transport without a checked pin is never acceptable.

`tls.json` persists the certificate/private key; `devices.json` persists names,
IDs and SHA256 hashes of high-entropy tokens, never plaintext tokens. Writes
replace files atomically. POSIX data directory/file modes are 0700/0600.
Windows restricts directory inheritance with icacls to the current user and
SYSTEM; use a dedicated per-user app-data directory, not a shared existing
directory. Changing/removing TLS state changes the fingerprint and requires
fresh out-of-band trust. Revoking a device persists first, then disconnects all
its sockets and releases its control. Revocation does not kill shared PTYs.

Profiles use stable IDs: pwsh and powershell only on Windows; bash and nu on
all platforms. Git Bash is preferred on Windows; System32's legacy WSL bash is
not accepted. Shells load interactive user configuration. The host never evals
command strings. An absolute existing executable override is accepted only for
an unavailable profile. cwd must be an absolute existing directory.

## Terminal lifecycle and snapshot boundary

`terminal.create` returns an unowned session. Subscribe for state/output, then
claim for input/resize. Claim is an explicit takeover; ownership is tied to the
authenticated connection, not a spoofable clientId string. Send preserves bytes
exactly and never inserts Enter. Default input is UTF-8 text. For legacy X10
mouse bytes use `terminal.send {sessionId,data,encoding:'binary'}`: every UTF-16
code unit must be <=255, maximum 65536 bytes, then the host writes
`Buffer.from(data,'latin1')` without UTF-8 re-encoding. Unknown encodings, invalid
byte values (including surrogate code units), and oversized data are rejected
before writing; authentication and ownership requirements are unchanged.

The pinned node-pty 1.1.0 public API accepts `string | Buffer`. Its Windows
`WindowsTerminal._doWrite` passes Buffer directly to the ConPTY input socket;
Unix `CustomWriteStream.write` copies Buffer unchanged and calls `fs.write`.
Windows/Linux/macOS raw input is enabled; other platforms explicitly return
UNSUPPORTED_ENCODING. Tests exercise all 256 byte values and the installed
Windows public write path through its input-stream boundary with high-byte X10
coordinates. This verifies transport byte preservation, not whether every
Windows console application interprets legacy mouse reporting. Unix backend
support was source-verified; Linux/macOS native runtime tests remain unrun.

Exited terminals remain inspectable until
`terminal.close`, which removes the session only after its real PTY exit and descendant cleanup
have been verified. Exit events carry node-pty's actual exitCode, never a fabricated
success code. On POSIX, an exit caused by a signal may have exitCode 0 as reported
by node-pty; the wire does not currently carry a separate signal field.

The host serializes PTY output, subscriptions, ownership and resize on one
queue. A subscribe response is sent inside that queue before subsequent output
events; the first later output has snapshot.seq + 1. Resize emits a complete
snapshot at the current output sequence. Other state-change events may precede
their request response. There is no blind request replay.

For both initial subscribe and terminal.snapshot events, reset the client
terminal, resize it to snapshot.cols/rows, then replay snapshot.ansi without
forwarding any parser query responses. ANSI contains normal scrollback first,
then alternate-screen transition/body if needed, modes/cursor/kitty flags, and
**the incomplete escape tail last**. Do not append reset/kitty sequences after
the tail: they could complete or corrupt a pending escape. kittyKeyboardFlags
is also supplied for input encoders. Clients must use Orca's Unicode provider
with Unicode11 and upstream kitty keyboard parsing enabled.

Orca's actual startup ingress owns its startup/OSC-color query policy; the host
headless emulator answers remaining live queries through actual Session.write.
The Session-internal emulator is query-silent, as are snapshot/seed writes. Windows sessions install Orca's ConPTY DA1 responder before
spawn (ESC[?61;4c); split startup queries are answered once through the same
live-write gate, never from replay. PTY streams are not
stripped. Text, ANSI, modes, scrollback, Unicode widths and kitty flags are
supported; image protocols and OSC hyperlink restoration are not claimed.
Orca's position-only DECSC snapshot trade-off remains: saved pen/charset is not
fully serialized. This is not a durable session-restart service: app exit ends
shells (on Windows also a forced kill, through a kill-on-close job object);
only credentials/certificate persist.

Bounds: 32 sessions (including exited), 64 sockets, 100 saved devices, 16 pending
pairing codes, 256 KiB request payload, 64 KiB input writes, 400 columns × 200 rows,
5000 scrollback rows, 32 outstanding requests/socket, 1024 host-wide, 100000 IDs
per connection. PTY reads pause above 1 MiB queued and resume below 256 KiB.
Socket backlog/individual message above 8 MiB disconnects the slow client;
reconnect and resubscribe. A pathological >8 MiB serialized screen cannot be
delivered and must be reduced (clear scrollback or use smaller dimensions).
Unauthenticated connections expire after 15 seconds and have eight attempts.
Ping/pong reaps dead sockets without touching their shells.

## Mobile display modes

The auth response advertises `capabilities.terminalDisplayMode: true`.
Sessions include observed `displayMode`: `auto` before fitting, `phone` while a
mobile fit is held, or `desktop` after an explicit desktop restore.

A mobile `terminal.subscribe` may include `displayMode: 'auto' | 'desktop'`
and `viewport: {cols, rows}`. Passive subscribers record measurements without
taking control or resizing the PTY. On mobile claim, the current subscriber's
measurement is applied. The current owner can call
`terminal.displayModeSet {sessionId, subscriptionId, displayMode, viewport?}`;
the result contains both `session` and `snapshot`. The subscription ID must
match the current registration. Mode changes emit `terminal.snapshot` with
an additional `displayMode`, including changes that keep the same dimensions.
New mobile viewport updates carry `subscriptionId`; they only record phone
measurements while desktop mode is selected. Phone fit has a minimum 20×8
grid, within the existing 400×200 wire bounds.

The host retains the pre-fit desktop dimensions across replacement subscriptions,
multiple mobile actors, socket disconnects and periods with no subscribers.
Like Orca's default `mobileAutoRestoreFitMs: null`, the last unsubscribe holds
the phone layout indefinitely. Selecting desktop mode or a desktop claim restores
the retained dimensions. Explicit desktop claim/viewport dimensions supersede
that baseline. Invalid or retired-subscription requests cannot mutate it.
Legacy subscribe, input and viewport calls remain accepted; mobile-mode features
require an updated host.

## Upstream reuse and attribution

Read-only source: `D:/project/.agent-work/e-terminal/orca-reference`,
commit `de8bffe24045b396212f4f63de8960ec8380ea07`.
MIT, **Copyright (c) 2026 Lovecast Inc.**
License is preserved in `terminal-host/vendor/orca/LICENSE`.
Every copied/adapted module records source path, commit and adaptation.

The actual 293-module Session/subprocess/headless/writer/driver import closure
is retained under `terminal-host/src/orca/`, with its MIT license and per-file
pinned source/blob/SHA256 provenance. Of these modules, 289 retain exact bytes;
four have documented CJS/Node ESM binding changes only. Function bodies are
unchanged. Vendor-local `.gitattributes` prevents CRLF conversion. See that
subtree's README and provenance test for the two-stage patch application.

`src/session.ts` is the host protocol adapter. It retains explicit authenticated
claim/ownership and stale-subscription fencing, and routes user text, Buffer
input, resize and generated query replies through actual Orca Session and
writer/driver code. Ordinary profiles do not claim an unsupported ready marker.
Host-owned storage and inherited agent/preflight isolation remain boundaries.
Cleanup retains identity-safe owned-tree scans and requires actual exit; it
releases the captured own PTY handle only after matching live targets are gone.

The exact upstream node-pty patch and its license/provenance are under
`terminal-host/orca-patches/`. Windows uses the original upstream
`useConptyDll: true` spawn body and verifies the actual DLL backend in focused
native checks. External xterm versions remain @xterm/headless 6.1.0-beta.302,
@xterm/addon-serialize 0.15.0-beta.300 and @xterm/addon-unicode11 0.10.0-beta.300.
Stock SerializeAddon is still a documented Orca parity exception.

## Checks

```sh
npm test --prefix terminal-host
npm run test:native --prefix terminal-host
npm run smoke --prefix terminal-host
npm audit --prefix terminal-host
```

Tests cover snapshot roundtrip (normal/alt/cursor/Unicode/partial CSI/kitty),
query authority, wrap-pending, local auth, remote pinned TLS and wrong-pin
rejection, pairing single-use, persistence/revocation, exact CLI readiness and
stdin shutdown with a live PTY, parent stdin EOF shutdown, and gated ConPTY
DA1 replies including split queries, raw-byte validation and Windows node-pty
Buffer transport. The real-PTY smoke sends commands through the binary wire
path and rejects non-owner/invalid-byte writes. The real-PTY smoke exercises every available requested shell,
sequenced output, observer rejection, takeover, resize snapshot, unsubscribe,
disconnect survival and close.

Current integrated checks on Windows/Node 26.8.2: host 28 pass/3 Unix skips,
native deployment 6 pass, desktop UI 68 pass; candidate release build and copied
resource startup/shutdown passed. Native PowerShell/Bash Unicode, binary input,
queries, reconnect and cleanup passed. Integrated feature-free Bash complete
and fragmented input each passed 20 rounds, but earlier real failures remain
and are not erased by these green rounds.

**Production input acceptance remains blocked:** Nushell 0.107.0 drops the two
astral characters in `hé汉👩‍💻`, for text and binary UTF8 input. A separate raw
ReadConsoleInputW probe on the same bundled DLL received both complete UTF16
surrogate pairs as down/up events. The Nushell release lockfile's crossterm
0.28.1 pairs surrogate events without filtering general key-up halves; this
matches the observed event stream and upstream issue #1072. This is a narrower
cause candidate, not an instrumented proof inside the installed Nu binary or
a verified production fix. Bracketed paste also failed the local control.
The raw probe emitted real exit 0 but its Node process timed out afterward;
it does not count as a cleanup pass. Full Android/manual IME/hardware keyboard
and Linux/macOS native acceptance remain incomplete. See the validation record.

## Reviewer fixes: cleanup and bind advertisement

Additional Orca reuse (same commit/license): `src/main/pty-process-table-parser.ts`
is copied unchanged, and `collectDescendantRows` is excerpted from
`src/main/pty-descendant-termination.ts` with its return type inferred to avoid
the daemon dependency graph. These are bundled into dist/termination.mjs by the
package build. No shell command strings are evaluated for cleanup.

`test/cleanup.test.mjs` checks address-family/specific-bind advertisements,
cleanup failure evidence, and actual shell plus descendant removal for both close
and shutdown. The Unix regression installs an ignoring-HUP shell and a descendant
that also ignores TERM; it is conditionally skipped on Windows. The Windows real
PTY regression checks actual exit code 1 and verifies both PIDs are gone before
success. No device tests are involved.
