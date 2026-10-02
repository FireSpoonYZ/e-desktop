# Shared DWM visual feasibility probe

Standalone, read-only Windows capability inventory. **Visual creation/transform is
unsupported on every build in this revision.** This is the approved capability-probe
fallback, not a working shared-thumbnail demo.

No production integration, DLL loading, private function calls, window/device
creation, capture, global hooks, injection, cloaking, DWM replacement, or system
modification. The nested `[workspace]` leaves the application workspace untouched.

## Research and ABI gate

Primary source: [ADeltaX: DWM thumbnails but with IDCompositionVisual](https://blog.adeltax.com/dwm-thumbnails-but-with-idcompositionvisual/).

Linked [sample gist](https://gist.github.com/ADeltaX/aea6aac248604d0cb7d423a61b06e247),
inspected at revision `39c19cae1121bf031537f6b9820446c766601488`.
The probe is independently written; no demo source is copied.

The article explicitly says: "Since symbols are stripped for these functions I
had to guess the name and the type". It describes a transition at "20xxx (or maybe
a bit earlier)", but its sample also says to use the older update signature for
"19043 or older". Neither source establishes an exact tested build/revision and
architecture allowlist for the signatures. In particular, neither verifies the
current host's 26200.9550 ABI.

These are **source-reported dwmapi.dll ordinal candidates**, not verified function
identities or callable Rust bindings:

| Ordinal | Article's label | Article's signature sketch |
| --- | --- | --- |
| 114 | DwmpQueryThumbnailType | HRESULT, thumbnail handle, type output |
| 147 | DwmpCreateSharedThumbnailVisual | HRESULT, destination/source HWND, DWORD flags, thumbnail-properties pointer, device pointer, visual output, thumbnail-handle output |
| 162 | DwmpQueryWindowThumbnailSourceSize | HRESULT, source HWND, BOOL client-only, SIZE output |
| 163 | SharedVirtualDesktop / SharedMultiWindow creation | HRESULT, destination HWND, device pointer, visual output, thumbnail-handle output |
| 164 | SharedVirtualDesktop / SharedMultiWindow update | HRESULT, handle, include/exclude HWND arrays and counts, RECT/SIZE outputs; newer variant adds DWORD flags |

The article shows setting an `IDCompositionVisual2` as a composition target's root,
committing, and releasing the visual rather than unregistering its thumbnail
handle. Those are research claims, **not runtime-verified ownership rules here**.

Export presence alone proves neither identity, signature, ownership, behavior nor
build compatibility. There is deliberately no guessed function-pointer binding,
ordinal lookup through GetProcAddress, force override, or enabled build range.
The supported-demo build allowlist is empty. `--demo` fails before any OS/file
access with `demo_status=unsupported` and exit code 2.

A future demo requires independently established signatures and lifetime rules
for exact OS/DLL builds and architecture, plus an explicit opt-in and a finite,
local test. Do not enable it merely because these ordinals exist.

## Build and read-only usage

From repository root (Rust with a Windows MSVC toolchain):

```powershell
cargo test --manifest-path experiments/shared-visual-probe/Cargo.toml --target-dir target/shared-visual-probe --offline
cargo build --manifest-path experiments/shared-visual-probe/Cargo.toml --target-dir target/shared-visual-probe --offline
.\target\shared-visual-probe\debug\shared-visual-probe.exe --inventory
.\target\shared-visual-probe\debug\shared-visual-probe.exe --pe C:\Windows\System32\dwmapi.dll
```

If dependencies are not cached, omit `--offline`. No arguments or `--help` prints
help without reading the OS. `--inventory` reads Windows 10/11 registry build
metadata and the process architecture's system directory using public APIs, then
parses `dwmapi.dll` and `dcomp.dll` as ordinary files. Run the native architecture
binary to avoid WOW64 system-directory redirection. The output identifies the
process architecture and each file's PE machine field.

`--pe FILE` parses an arbitrary on-disk PE32/PE32+ file without executing it and is
also available on non-Windows hosts. Ordinal hints only have their article meaning
for **dwmapi.dll**, never for arbitrary input DLLs. Names are independently
reported for public DWM thumbnail exports and DirectComposition device creation.

Inventory success exits 0 but always reports `demo_status=unsupported`. Bad
arguments, missing metadata/files, malformed PE data and demo requests exit 2.
Input reads are capped at 64 MiB; sections at 96; export/name counts at 65536;
export/forwarder strings at 1024 bytes including the terminator.
Unmapped/truncated tables, invalid name indices, overflow and unterminated strings
are rejected. Zero-RVA ordinal holes are absent, not available capabilities;
forwarded exports are identified. This is an export-table inspector, not a PE
loader or complete binary validator.

## Verified in this worker run

Only HTTP research, compilation, parser/CLI tests and read-only OS/file inventory
were performed. **No desktop operation or private API execution occurred.**

Toolchain: Rust/Cargo 1.97.1, `x86_64-pc-windows-msvc`.
Tests: 5 passed, 0 failed (PE32/PE32+, ordinal-base mapping/holes, names/forwarders,
all truncated fixture prefixes, malformed counts/RVAs/name references/strings,
empty export directory, explicit CLI modes and unconditional demo refusal).
Offline build and all-target Clippy with `-D warnings` passed.

Host registry:
- CurrentBuildNumber + UBR: **26200.9550**, DisplayVersion **25H2**.
- BuildLabEx: `26100.1.amd64fre.ge_release.240331-1435`.
- Both inspected DLLs: PE machine **0x8664** (AMD64).

| File | File/product version | Nonzero exports | Relevant observed exports |
| --- | --- | --- | --- |
| System32/dwmapi.dll | 10.0.26100.8875 | 118 | Ordinals 114, 147, 162, 163, 164 present, unnamed, not forwarded; public DwmRegisterThumbnail and DwmUpdateThumbnailProperties present |
| System32/dcomp.dll | 10.0.26100.9549 | 42 | DCompositionCreateDevice, DCompositionCreateDevice2, DCompositionCreateDevice3 present |

Observed dwmapi candidate RVAs: 114=`0x97c0`, 147=`0x5e30`, 162=`0x2b80`,
163=`0x7740`, 164=`0xba30`. These bytes were not executed or disassembled.
The OS registry and individual DLL versions differ; do not infer an ABI from one
version number alone.

Read-only SHA-256 identity recorded with PowerShell Get-FileHash:
- dwmapi.dll: `733C4DBF3E3FC521199AE57A5ADC0B780D8C63D3DE96676670CBF6D27CFED5E2`
- dcomp.dll: `4077BF81A9A178D097288ED5B47A66B21881FB7581DDD142E53E66F0F94376ED`

## Untested / unsupported

Shared visual creation, transform, update, visual/thumbnail ownership, actual
composition behavior, minimized/offscreen sources, GPU/device changes, other OS
builds/architectures, and non-Windows compilation were not tested. Public device
export presence does not verify device initialization or shared-visual support.
There is no visual smoke command to run until the ABI gate is satisfied.
