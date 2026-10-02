# Windows backend checks (parent/operator only)

The implementation lane did not operate the desktop. The integration owner subsequently completed PID-scoped real-window and Tauri GUI checks; see [the current validation record](../../../../docs/validation/windows-2026-10-02.md) and [the earlier GUI record](../../../../docs/validation/windows-2026-09-26.md). Run `powershell -NoProfile -File scripts/windows-smoke.ps1` from the repository root for the reproducible native smoke test. `check.rs` remains the original standalone pure-check harness; the normal workspace tests now include the backend.

## Headless check

From repository root, in bash on Windows with the Rust MSVC toolchain available:

```bash
cargo build --workspace --no-default-features
rustc --edition=2024 --test src-tauri/src/platform/windows/check.rs \
  -L dependency=target/debug/deps \
  --extern serde=$(find target/debug/deps -name 'libserde-*.rlib' | head -1) \
  --extern serde_json=$(find target/debug/deps -name 'libserde_json-*.rlib' | head -1) \
  --extern windows_sys=$(find target/debug/deps -name 'libwindows_sys-*.rlib' | head -1) \
  -o target/windows-check.exe
./target/windows-check.exe
```

If a reused target directory contains multiple dependency-feature builds, use a clean target directory or select the matching artifacts. Ordinary `cargo test --workspace --no-default-features` already includes the backend helper tests on Windows.

## Integration / limits

- Parent exports this directory's `Backend` from `platform/mod.rs`; no shared contract or dependency changes are needed. Backend is `Send`; serialize access on the dedicated controller thread. Win32 calls can wait for foreign window procedures: do not run the polling/control loop on the WebView UI thread. No desktop-changing operation occurs in `new()` or `status()`.
- Poll `enumerate()` approximately every 250–500 ms, plus after actions. No event hook/message-loop dependency. Enumeration reads desktop metadata and attaches a private lifetime property to eligible windows, but never moves, focuses, minimizes or closes them. The OS removes this property on destruction, preventing same-process handle reuse from adopting stale IDs. Elevated/UIPI-denied windows are excluded; access is not escalated.
- Start paused. Explicitly enable only after parent reserves overlay viewport. Preserve ordered placement-before-focus actions. Do not focus on every poll. Manager-minimized windows remain enumerated, including their actual iconic native geometry; engine should retain its own column geometry. Only explicit `Focus` requests activation. Foreground lock rejection is an error, not a workaround via synthetic input or thread-input attachment.
- `clip` is an absolute screen-pixel rectangle, intersected with the target window and the selected monitor work area. Original application region is intersected, not discarded. No-region originals are restored with NULL. Region ownership transfers to USER only after successful `SetWindowRgn`; private copies are deleted by RAII. Partial clips reject layered and RTL windows. Fully excluded windows minimize recoverably; no `SW_HIDE` is used.
- Ordinary GDI/DWM region-compatible windows are the supported clipping case. This is not security isolation: applications can reset their regions or use independent owned popups, and compositor shadows/custom rendering need real-machine verification. Layered/RTL clipping is explicitly denied; per-window constraints/unsupported behavior produce errors. No claim of compositor-equivalent clipping for every app.
- On failed placement the backend attempts to restore that window's original state, including recovering an empty transition mask. Parent must refresh after **any** apply failure; earlier actions in a batch are not rolled back. Report errors. On disable/normal exit call `restore()` explicitly and surface aggregate failures. Drop performs a final best-effort restoration and logs failures, but crash/forced termination cannot restore.
- The overview has live DWM previews and a bounded static pre-hide fallback. The snapshot worker captures only explicitly manager-owned, fully drawable windows; enumeration-only entries are not captured. The smoke checks live → prewarm → minimize/static → clear/reopen → restore/live, plus real compositor preparation, retarget, mouse hit policy and cleanup. These API/geometry checks do not assert visual pixels or physical mouse delivery. No global shortcuts in this backend; the app starts a separate `WH_MOUSE_LL` hook thread (`hook.rs`) for pointer focus, modifier drags and hot corners. Windows 10+ DPI context APIs are used, with scoped per-monitor-v2 thread awareness.

## Safe manual smoke sequence

Use only disposable test windows with no unsaved work. Ensure explicit disable/restore is reachable before enable. Do not automate against arbitrary user windows. Parent performs all steps, not the implementation lane.

1. While paused, inspect enumeration: own Tauri process/taskbar/desktop/tool/hidden/cloaked windows absent; real app title, executable basename (or honest PID fallback), rect, monitor work area and DPI match the OS. Include negative-origin and mixed-DPI monitors if available.
2. Restrict the test action list to one disposable ordinary resizable window. Record its native placement and region, including initially maximized/minimized variants. Place it inside reserved viewport and verify exact physical bounds; focus stays unchanged. A size-constrained app must produce an explicit error rather than report an invented placement.
3. Clip the test window at left/right/top/bottom edges, then move across monitors. Check no content escapes requested work area; test a pre-shaped region too. Remove clip and verify original region returns. Try layered/RTL test windows: expect clear denial and original-state recovery, not destructive clipping.
4. Minimize via placement for a noncurrent page/fully offscreen column. Poll repeatedly: same lifetime ID remains with `minimizedByManager=true`. Return to page: recover without repeated activation; explicit focus may activate or honestly report foreground-policy denial.
5. Close only a disposable document using `Close`; confirm normal app save-prompt behavior (never process termination). Cancel prompt and continue. Destroy/recreate disposable windows, including rapid same-process creation: old IDs must fail with `windowGone`, never mutate the replacement.
6. Disable after normal placement, partial clipping, maximization and page minimization. Verify initial placement/show state/region returns. Repeat enable/disable. Normal application exit should restore too. Force an operation failure and confirm no permanent empty mask; restoration error must be visible if recovery is denied.
7. Disconnect/reconnect a secondary monitor and change DPI/work area while paused, then refresh. Recheck metadata before enabling. Test hung/elevated/custom-rendered applications separately; they are not assumed fully supported.

## Mixed-DPI visible-frame regression

With e-desktop stopped, launch a disposable Edge app window titled `EDPEEKTEST`
using a separate `--user-data-dir`. Pass its process ID to:

```powershell
cargo run --manifest-path src-tauri/Cargo.toml --example windows_dpi_smoke --no-default-features -- <fixture-pid>
```

Start the fixture on a 100% side monitor when the primary is 150%. The check
moves only that PID/title-matched fixture, applies repeated clipped placements
on each monitor, temporarily removes the fixture clip, and compares DWM's actual
visible frame against the target. This bypasses the backend's cached padding,
which previously hid a missing-DPI-cache error from ordinary readback checks.
The fixture placement is restored after each monitor and before reporting errors.
