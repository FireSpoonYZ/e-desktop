# Hosted desktop terminals

The top bar **终端** button opens shell creation, session management, pairing
and paired-device revocation. Choose an available profile, optionally enter a
working directory, and create a session. An unavailable profile accepts an
absolute executable override; the host validates it.

Each session opens one normal decorated Tauri window, eligible for existing
tiling. Topbar, overview, command palette and terminal manager stay excluded
from own-process enumeration. Only exact Rust-registered terminal titles can
bypass the PID filter; terminal OSC output never sets these native titles.

## Control and lifetime

- **接管输入** explicitly takes control; other desktop/phone views become read-only.
- **释放控制** relinquishes input and viewport ownership.
- **分离窗口**, the native close button, and closing the manager detach views only.
- **结束 shell** explicitly ends the PTY after confirmation.
- Reopen a session from the manager to recover its authoritative snapshot.
- Disconnects and timeouts reject pending requests, never replay input or mutations.
  Reconnect explicitly; a timed-out operation may already have happened.
- Exiting e-desktop restores managed windows but leaves the background terminal
  service and shells running, including on app crash or force-kill.
- The service runs from a relocated copy under Tauri's local application data
  directory in `terminal-host/<build id>/`, so rebuilding the repository does
  not replace files held open by the running host.
- Service data (certificate, paired devices, etc.) is under Tauri's application
  data directory in `terminals/`. A restarted app reads the private
  `terminals/host.json` record and authenticates through loopback `/health`
  to reuse the host; running-session windows reopen automatically.
- With no running shells and no connected clients, the host exits after 60 seconds
  (also if nobody adopts it after startup). Exited sessions do not keep it alive.
  Close all sessions and disconnect views to stop it, or explicitly end the host's
  Node process. Stdin and app exit are not shutdown requests.
- A host from an older build keeps running while it has live sessions. Once it
  has none, the app can request authenticated HTTP shutdown and start the new
  build. A stuck verified host gets process-tree termination after twelve seconds.
  On Windows, the host's own kill-on-close job reaps its shells when the host dies;
  no app-owned job ties it to e-desktop. Sessions do not survive host death/reboot.

The view uses patched xterm, native composition/paste/selection behavior,
scrollback, explicit control, and font-size controls. Only the owner publishes
viewport changes; observers retain the host's dimensions rather than resize
the PTY. Snapshot and output writes share one bounded ordered queue.
Kitty keyboard negotiation is enabled and snapshot flags are restored.
No image addon is advertised; unsupported image graphics are not claimed to
render, and ANSI output is not stripped.

Parser-generated replies never travel to the PTY from this view. Orca's
classifier is reused with a pinned-xterm input-provenance adapter, preserving
actual modified F3 even though its bytes match CPR. Legacy X10 binary mouse
uses terminal.send encoding:'binary'; the host must either support raw bytes
on its platform or explicitly reject them. SGR mouse and keyboard use text.

## Development and release

Requirements: Node.js 22+ installed, npm, Git (applies the exact Orca patch),
and the project's normal Tauri/Rust/platform build tools. For Vite development
use the project's existing Node minimum (22.12+). Node is NOT bundled.

After integrating the separately owned terminal-host directory:

```sh
npm ci
npm run tauri -- dev
npm run tauri -- build
```

Root postinstall applies the pinned upstream ESM xterm patch, idempotently.
Tauri beforeDevCommand / beforeBuildCommand run
`npm run prepare:terminal-host`: install host dependencies with npm ci (or
npm install without a lock), run host build --if-present, and copy host source,
generated assets and node_modules, including platform-native node-pty files,
into ignored `src-tauri/terminal-host/` resource staging.

The host must be built/staged on the target platform and architecture.
Do not distribute a bare EXE without its terminal-host resources. The existing
bundle.active=false policy is retained: enable the desired Tauri installer
target separately if producing an installer. Resource mapping is configured
for release builds independently of installer creation.

Rust uses a compile-time absolute source directory in debug, and
Tauri resource_dir()/terminal-host/ in release, then copies that tree to the
versioned local-data runtime before launching it detached. A stat-based build ID
tracks relative paths, sizes and modification times, excluding test, .git and .pi.
Neither lookup depends on launch CWD. Rust discovers an absolute installed Node
executable from PATH, or accepts an absolute `E_DESKTOP_NODE` override, checks Node >=22 and gives
an actionable error if unavailable. Restart the app after installing Node
so it receives the updated PATH. No admin token appears in a URL or log;
the readiness endpoint reaches local views only through Tauri IPC.

Pairing descriptors deliberately contain a single-use, five-minute code and
certificate fingerprint. Share only with your own phone, on LAN/Tailscale.
The remote default port is 7768; firewall/network configuration remains explicit.
The local admin endpoint is loopback-only WS with authenticated HTTP
health/shutdown; CSP permits only that WS origin in addition to the existing IPC origins.

## Reuse and validation

Orca de8bffe24045b396212f4f63de8960ec8380ea07, MIT Copyright (c) 2026 Lovecast Inc.
See ui/src/terminal/orca/ADAPTATIONS.md, LICENSE and XTERM-LICENSE for exact
source paths, unchanged helpers and extracted ESM patch provenance.

Focused checks:
```sh
node --test ui/tests/terminal.test.mjs
npm run build
cargo check -p e-desktop --locked
cargo test -p e-desktop --locked terminal::tests
cargo test --workspace --no-default-features --locked
```

Automated checks do not replace GUI/device validation. Windows integration has
also exercised actual host startup, physical Android WSS pairing, shared
PowerShell Unicode input, a real Pi settings TUI and editable phone presets.
Release-resource launch, tiling and lifecycle results are recorded separately
in the validation notes. Linux/macOS runtime behavior and physical hardware
keyboards remain target-platform acceptance items.
