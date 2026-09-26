# macOS native lane

Implemented AX/CF/CoreGraphics/AppKit FFI; **not built or exercised on macOS yet**. No dependencies, manifests, shared contracts, or application wiring changed. Windows host checks do not validate Apple's ABI/framework linkage.

## Parent integration

- Select `macos::Backend` under `cfg(target_os = "macos")` in the parent-owned platform selector.
- Construct, call and drop Backend on the same dedicated OS worker. Owned CF/AX references intentionally make it non-Send; do not add `unsafe impl Send`.
- Keep the Cocoa main run loop running. NSScreen reads synchronously dispatch to the main queue; the main thread **must not block waiting for the backend**, including startup and exit/restore. Use asynchronous request/reply integration.
- Start paused. Refresh permission/status after enumerate/apply failures. Re-enumerate after any partial apply failure; earlier mutations remain recorded for restoration. Call `restore()` before normal shutdown/disable; failures retain originals for retry. `Drop` does not mutate windows. Forced termination is not recoverable.
- Honor `clipping=false`: no `clip: Some(...)` actions. There is no compositor-like isolation, arbitrary clipping, pointer-follow focus, native fullscreen management or shortcut registration here. Restrict viewports to whole-window visibility in layout.
- Same backing scale across all NSScreens is required. Global CG origins and dimensions are multiplied by this one scale. Mixed backing scales return UnsupportedSession and no window-control capability; no overlapping pseudo-physical coordinate space is fabricated. Restoration uses saved native logical frames even if topology changed.
- Only AX standard windows with writable position and size are initially admitted. Own-process windows, dialogs, native fullscreen and non-AX windows are excluded. IDs are monotonic backend-session IDs matched with CFEqual, not recycled CG numbers. Managed/minimized references are retained when missing from AXWindows; only explicit AX invalid-element errors prove closure. Other Spaces may be inaccessible. Application AX failures and constrained/deferred geometry are errors, not fabricated success.

## macOS build and manual acceptance (parent only)

1. On Apple Silicon and/or Intel macOS with the Rust toolchain and Xcode command-line tools: after platform selection, run `cargo test --workspace --no-default-features` and `cargo build --workspace`. The geometry test covers Retina conversion, negative origins, Cocoa/CG Y inversion, invalid geometry, and display intersection. Verify framework linkage on the real target; host typechecking cannot do this.
2. Launch through the parent's approved GUI workflow, initially without Accessibility permission: expect PermissionRequired, all capabilities false, and no security-setting change/prompt initiated by the backend.
3. Manually grant the actual executable/app in **System Settings > Privacy & Security > Accessibility**, restarting it if required by macOS. Refresh; on a single/uniform-scale display expect Ready. Screen Recording permission is not requested or needed; no capture is implemented.
4. With disposable ordinary windows (for example TextEdit), compare titles, app names, bounds, work areas (menu bar/Dock excluded), focus, and stable IDs across repeated refreshes. Check Retina and negative-origin second monitors with the same backing scale.
5. Explicitly enable via parent integration; verify move/resize, app minimum-size denial, focus/raise, minimize then refresh (same ID remains), unminimize, and disable/restore to original bounds/minimized state. Also test a window originally minimized. Do not use valuable unsaved documents for initial tests.
6. Close a disposable window gracefully, including an unsaved-document confirmation dialog: cancelling must keep its ID/restoration bookkeeping; never terminate the process. Quit a target application, revoke AX permission, and test a hung/refusing AX app; expect explicit errors and no wholesale fake disappearance/success.
7. Change to mixed backing scales; refresh/apply must reject the session before moving windows. Check that restore attempts every saved window even when one fails; originals remain available for retry. Native fullscreen must be left alone. Confirm normal shutdown restoration and absence of main-queue deadlock.

No real windows, desktop settings, or GUI were touched in this lane. Position/size restoration does not restore historical z-order/focus, Spaces assignment, or OS-native fullscreen; moving a display after capture can make its old logical coordinates obsolete. AX can acknowledge asynchronously, so immediate geometry verification may report a deferred operation as denied; the next enumeration is authoritative.
