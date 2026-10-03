# Orca Session/input runtime closure

Pinned upstream: https://github.com/stablyai/orca.git at
`de8bffe24045b396212f4f63de8960ec8380ea07`.
MIT, Copyright (c) 2026 Lovecast Inc.; retain `LICENSE`.

`provenance.json` records the original Git blob, SHA256 and byte count for
every copied file. This is the measured **293-module runtime import closure**
of Session, createPtySubprocess, HeadlessEmulator, RuntimeTerminalWriter and
RuntimeTerminalDriverController, not an app/RPC/renderer dump. Normal esbuild
tree shaking removes unused output; import resolution still needs those files.
Type-only app contracts are not a full TypeScript/application checkout.
No assertion that all imported functions executed or all Orca features are enabled.

Four deliberate Node ESM binding adaptations are listed with post-adaptation
hashes: default imports for the three external CJS xterm packages in the emulator,
and `createRequire(import.meta.url)` in three Windows modules. Function bodies
are unchanged. The provenance test reverses only these binding edits and checks
the exact pinned bytes. The host-owned .gitattributes uses * -text only inside
this vendored subtree to prevent Windows Git CRLF conversion; it is not upstream
source. Upstream launch, environment, native/subprocess handle,
startup ingress, query reply delivery, Session.write/resize and writer/driver
bodies have not been rewritten.

## Host boundaries

- `../session.ts` maps the existing one-owner wire protocol to Session. Claim,
  release, disconnect, replacement and subscription fences stay in server.mjs.
  Mobile write reservations only admit the already-claimed host owner; they do
  not add Orca's implicit subscription takeover or a new RPC architecture.
- Every user send goes through the actual writer and Session. Raw Buffer payloads
  use the writer's unchunked action branch and survive Session's startup queue;
  generated query replies stay strings. Acceptance is JS admission, not proof
  that ConPTY consumed bytes.
- Current profiles request no startup marker feature, so shellReadySupported is
  false (the upstream default). Tests explicitly exercise a marker-capable fake.
  Winning-shell capability remains required if an internal marker launch is
  ever supplied. Git Bash bash.exe does not magically acquire a ready marker.
- Upstream helpers that read process.env.ORCA_USER_DATA_PATH are scoped to
  serialized creation under the host's private dataDir/orca-runtime. The prior
  environment binding is restored. Inherited Orca agent overlays/preflight are
  removed through actual envToDelete; normal user profiles remain in force.
- Session's internal emulator remains non-authoritative. Upstream startup
  ingress retains its OSC-color ownership/filtering; the host's public emulator
  answers remaining live queries through Session.write. Hydration does not answer.
  `../emulator.ts` preserves the existing {ansi,cols,rows,seq,kittyKeyboardFlags}
  snapshot API over the actual upstream HeadlessEmulator.
- Cleanup is the existing host identity-proven owned-tree algorithm, NOT
  Session.forceKill/root-only termination. The explicitly approved loop-order
  exception in termination.ts releases the captured own subprocess handle only
  after a fresh scan proves all matching identities gone; success still requires
  a real public/Session exit. Errors/deadlines retain the session for retry.

## Applying the incoming-only source patch on Windows

A single git apply does not load a newly added attributes file early enough for
its other hunks. In a Git integration worktree, apply the vendor-local attribute
first, then the remaining delta (do not change global core.autocrlf):

```sh
git apply --include=terminal-host/src/orca/.gitattributes <session.delta.patch>
git apply --exclude=terminal-host/src/orca/.gitattributes <session.delta.patch>
git check-attr text -- terminal-host/src/orca/src/main/daemon/session.ts
```

Expected attribute: text: unset. This two-step application was tested with
core.autocrlf=true; all 294 source/license SHA256 values matched the manifest,
including the four documented adapted hashes. Provenance tests then reverse
only those binding edits and check original pinned bytes. Applying outside a
Git repository does not enforce attributes and is not the byte-preserving
integration workflow.

## Integration build/dependency handoff

Add `src/session.ts` to the existing esbuild entries (package/build files are
owned by the native/integration lane):

```sh
esbuild src/emulator.ts src/termination.ts src/session.ts --bundle --platform=node --format=esm --packages=external --outdir=dist --out-extension:.js=.mjs
```

server.mjs now imports dist/session.mjs; prepare/package that generated asset.
Keep the native lane's npm native prepare and source/runtime/resource deployment
gate. Integration must execute that gate with the actual host Node before
server/facade startup, not assume Rust PATH/ABI stays fixed after packaging.
Do not add a stock/debug or legacy fallback. Upstream spawn explicitly passes
useConptyDll:true; on supported Windows node-pty's original build-number
predicate actually selects ConPTY. Focused tests verify both actual private
backend flags and the bundled conpty.dll loaded in the Node process; production
logic is not polluted with these test-only private-field checks.

Required external JS packages are the existing node-pty and pinned xterm
headless/serialize/unicode11 packages. The host's ws/selfsigned dependencies are
unchanged. Optional upstream loaders for @vscode/windows-process-tree and
@orca/windows-registry keep their real feature-detection/CIM or registry
fallback paths; they are not installed or claimed as packaged here.

This source port requires the separate native lane's exact node-pty 1.1.0
patch, node-addon-api 7.1.1, packaged-runtime ABI rebuild and ConPTY resources.
A diagnostic ABI147 addon is used only in isolated local tests. This is not a
packaged native deployment claim. Run source/protocol tests after building all
three entries, then standalone native evidence:

```sh
node --test test/*.test.mjs
node test/session-native.mjs shells <new-results.json>
node test/session-native.mjs complete <new-results.json>
node test/session-native.mjs fragmented <new-results.json>
```

The native collector stops a case on first input failure, retains output and
independently checks cleanup. It adds no startup sleep, forced wrapper/preflight,
input replay/retry, or suppressed resize. Feature-free Bash first-character loss
and Nushell astral-character input remain unresolved; later green runs do not
erase earlier failures or establish a production cure. Stock SerializeAddon is
still not Orca's separately patched renderer/serialization deployment.

## Bounded Nushell negative control

nu-input-control.mjs records UTF8 bytes/UTF16 code units/scalars at wire intent,
actual Session.write (current only), native PTY and input-socket invocation.
The shell itself prints its parsed literal's UTF8 hex, so lost codepoints are
not inferred from headless rendering.

Under the same Node/Nu/80x24/input and patched addon:
- Current DLL and byte-identical incoming host with only a test-boundary DLL
  option both dropped the two astral characters, in text and binary input.
- Literal incoming host's kernel backend retained astral characters but dropped
  the ZWJ. It is also full-string red, but NOT the identical astral defect.
- JS boundary payloads remained correct; all captured own shells naturally
  exited0 and cleanup passed.

No facade/writer/binary body regression is demonstrated. The required DLL
backend transition exposes a different native/shell-consumer input failure;
its internal cause is unresolved and production input acceptance remains BLOCK.
Do not describe this as merely an unchanged incoming astral bug, do not fall
back to the legacy backend, and do not add a handwritten surrogate encoder.
Socket traces prove invocation bytes, not async completion/native consumption.

The control accepts current/baseline/baseline-dll, a host-root path and a new
result path. Baseline paths must be external copies of incoming source, never
the original repository. baseline-dll changes only the test spawn boundary;
it is not a production fallback or a modified upstream/source body.
