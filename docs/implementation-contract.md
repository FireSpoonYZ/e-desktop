# Implementation contract — desktop v1

Shared source of truth: `src-tauri/src/model.rs` and `ui/src/model.ts`. Rust owns layout; the frontend renders complete snapshots and submits commands. Application startup and refresh never enable management implicitly.

## Wire and state

- Serde camelCase fields; commands/actions are internally tagged `{ "type": "..." }`. IDs are opaque strings, never JS numeric native handles. Optional fields serialize as `null`.
- `get_snapshot() -> Snapshot`; `execute(command) -> Result<Snapshot, AppError>`; JS calls `invoke('execute', { command })`.
- `snapshot` events carry the full state. UI subscribes before fetching its initial snapshot and treats subsequent events as authoritative.
- Native surface commands: `open_surface({surface, monitorId?})`, `dismiss_surface({surface})`, `quit()`. Surface is `overview` or `commands`; invalid explicit monitor IDs are rejected. `surface-opened` carries the selected monitor ID or null.
- Commands: refresh, enable, disable, focusWindow(windowId), focusDirection(direction), switchPage(monitorId,pageId), addPage(monitorId), moveWindowToPage(windowId,pageId), moveWindow(direction), cycleWidth, setColumnWidth(width), adjustColumnWidth(delta), adjustWindowHeight(delta), resetWindowHeights, centerFocused, scroll(monitorId,delta), slideColumn(direction), toggleFloating, toggleFullscreen, closeWindow(windowId), dropWindow(windowId,x,y), dragEdge(monitorId,edge,delta), dragRow(monitorId,column,edge,delta), setFloatingRect(windowId,rect). slideColumn scrolls the active monitor until the next column left/right is fully visible and focuses it, or focuses the neighbouring column when that side is already visible. dropWindow takes a physical screen point: tiled windows stack into the column under it, form a new column on its outer quarters/empty space, or queue as a column just off screen when dropped on the left/right edge band. dragEdge/dragRow move a column or stacked-row boundary with 50% and edge snapping; a boundary dropped on a screen edge squeezes the windows it passed off screen (rows leave as one column queued on the side of the screen half their column is in). Drag previews run the same command on a copy of the engine. setFloatingRect applies only to floating, non-fullscreen windows. Directions are left/right/up/down. Scroll delta is signed physical horizontal pixels. Implicit commands address the focused window or active monitor.
- Rect uses physical screen pixels: signed x/y and unsigned width/height, including negative monitor origins. Native work areas remain unchanged. MonitorState.viewport reserves a pinned 36 CSS px topbar (none when `topBar` is false), converted using scaleFactor, then is inset by half of `gaps` on every side; tiled slots are inset by the other half and clip against the viewport expanded back by that half. No side rail is reserved.
- Monitors own independent pages and activePage; pages retain viewportX, ordered columns and floatingWindows. Columns contain top-to-bottom window IDs. Native metadata lives in Snapshot.windows[].native.
- Sizing commands (including cycleWidth) require a focused tiled, non-fullscreen window. Width is clamped to the viewport; explicit zero is rejected. Stack heights use session-local per-window weights, always positive and exactly filling the viewport; reset restores equal shares. Impossible geometry (more rows than viewport pixels) rejects the transaction.
- Floating windows leave tiling. Layout fullscreen fills the usable viewport; it is not OS-native fullscreen. Existing floating geometry survives minimized/iconic enumeration. Layout state is session-local.
- BackendStatus separates availability and individual capabilities. `ready` does not imply every capability. No sample/fabricated windows or silent unsupported enumeration.

## Modules and signatures

| Module | Exports |
| --- | --- |
| `src-tauri/src/layout/` | Engine and pure behavior tests |
| `src-tauri/src/platform/windows/` | Win32 Backend |
| `src-tauri/src/platform/linux/` | X11 Backend; explicitly unsupported Wayland |
| `src-tauri/src/platform/macos/` | Accessibility/CoreGraphics Backend |
| `ui/src/shell/` | TopBar, PageRail |
| `ui/src/overview/`, `ui/src/commands/` | Overview, CommandPalette |
| Integration | App.tsx, bridge.ts, app.rs, target-cfg platform selector, shared models/config |

```rust
// Pure layout, no OS or Tauri calls.
Engine::new(BackendStatus) -> Self;
Engine::snapshot(&self) -> &Snapshot;
Engine::set_backend(&mut self, BackendStatus); // no layout reset
Engine::set_viewports(&mut self, BTreeMap<MonitorId, Rect>);
Engine::set_window_rules(&mut self, Vec<WindowRule>) -> Result<(), AppError>; // validates atomically, new IDs only
Engine::reconcile(&mut self, SystemSnapshot) -> Result<Transition, AppError>;
Engine::dispatch(&mut self, Command) -> Result<Transition, AppError>;

// Each statically selected platform Backend has these inherent methods.
Backend::new() -> Result<Self, AppError>;
Backend::status(&self) -> BackendStatus;
Backend::enumerate(&mut self) -> Result<SystemSnapshot, AppError>;
Backend::apply(&mut self, &[NativeAction]) -> Result<(), AppError>;
Backend::restore(&mut self) -> Result<(), AppError>;
```

`Transition` contains an updated snapshot and ordered native actions. Commands commit layout transactionally. Close emits a request; a window is removed after native enumeration confirms its lifetime ended.

Integration creates and retains Backend on a dedicated OS thread, including non-Send macOS AX objects. A bounded request queue separates IPC from native work. Blocking native requests never run on the WebView/Cocoa main thread. Native enumeration and configuration contents are checked on a 500 ms deadline, including under queued command traffic. Runtime backend status is synchronized into Engine before capability checks. Viewport overrides are supplied before reconciliation.

Background reconciliation does not force foreground focus. Unchanged placements are skipped when the observed native state still matches. Native apply failures refresh metadata; Placement/Restore failures pause automatic layout and attempt recovery.

## Native ownership and lifecycle

NativeAction separates Placement(rect,clip,minimized), Focus, graceful Close and per-window Restore. Clip is in physical screen coordinates; the backend converts it to native region coordinates. `None` removes manager-owned clipping, not pre-existing application regions.

Before first mutation, retain original geometry/show/region state as supported by the platform. Manager-minimized windows remain enumerable; windows the application hides leave the layout (and are restored hidden). Fully offscreen windows use recoverable minimization; on non-clipping backends, partially offscreen tiled windows also minimize. Windows regions do not clip DirectComposition content, so WS_EX_NOREDIRECTIONBITMAP windows are not repeatedly region-masked. The Windows backend lowers a partial window in the Z order on entry to clipping (or after activation/restoration), where the neighbouring monitor's windows cover the cut-off part; a partially offscreen window whose rectangle reaches a monitor showing no tiled windows minimizes instead. Managed windows have DWM minimize/restore transitions disabled and restore directly into their target slot. Windows layout animations are composited per monitor: an opaque click-through overlay covers each animating monitor and draws DWM thumbnails of its windows at the frame positions, cropped to that monitor; real windows take their final placement once under the overlay (windows shrinking out of view or ending hidden at the end), and the overlay is removed when the animation finishes, is cancelled or management stops. If the overlay fails, frames move the real windows. A refused foreground activation is not worked around and never ends the session; the next enumeration adopts the actual focus. The focused column fits and becomes fully visible. Floating windows retain free placement.

Validate window instance identity rather than trusting reusable native IDs. Windows uses per-HWND lifetime properties; X11 uses session/generation properties and guarded request submission; macOS retains AX identity. These are lifecycle protections, not isolation from malicious clients with equivalent OS privileges.

Disable and normal exit attempt restoration of all owned windows. Failed restoration retains ownership for retry and surfaces errors. Having no originals is a successful no-op, even without AX permission or on unsupported Wayland; enumerate/apply remain explicit failures on unsupported sessions. Do not infer absence of restore ownership from paused state.

Application ExitRequested and native window-close paths use the same asynchronous worker restore handshake. Only successful restoration permits final application exit; failure leaves the app paused with an error. No synchronous main-thread wait is allowed, because macOS screen collection uses the main queue. Forced termination/crash recovery is not guaranteed.

## Native UI surfaces

Each monitor has one topbar; dynamic labels use `topbar-N` and have matching local capabilities. PageRail renders as inline workspace buttons inside TopBar, not a separate native window. Controls have no native shadow, so their outer/client bounds match the reserved area. Permanent controls are topmost only while enabled. Overview/commands are temporarily topmost when opened, above the controls, and hidden on dismissal.

All components receive `{snapshot, onCommand, busy?}`. TopBar also receives `onOpenOverview`, `onOpenCommands`, optional `onQuit`; PageRail accepts optional monitorId; Overview and CommandPalette accept onDismiss. Components do not invoke IPC or implement a second layout state machine. App owns IPC errors; shell consumes rejected promises, while transient surfaces dismiss only after successful commands.

Overview displays the selected monitor's vertically arranged workspaces at half logical size, capped to fit the destination display. Column widths and viewportX share that scale; signed offsets retain centering and trailing space. Windows DWM previews fill their reserved rectangles, with captions and collapsed layout controls outside the native preview area. Other platforms and unavailable sources show explicit fallback text. Floating windows remain a separate strip. Command palette handles selection, IME composition, focus containment and escape dismissal. Topbar scrolling targets only its selected monitor; the dedicated wheel region converts to physical pixels and batches input with one request in flight. Palette sizing commands use the same Rust command contract.

JSON configuration supplies replace-all shortcut bindings and ordered initial window rules; see [configuration](configuration.md). Invalid configuration retains the last accepted mapping/rules. Registration failures preserve the old mapping with best-effort rollback and visible errors. Rule matching is literal case-insensitive app/title substring matching, with later matching action fields taking precedence. Rules affect only new IDs; paused discovery does not perform native actions. Missing monitor/page targets fall back without creating pages or activating background pages.

Pointer-follow focus, compositor-level window decoration/overview animation, networking/mobile/LLM services and persistent session layouts remain outside this iteration.

## Validation

`npm test`, `npm run typecheck`, `cargo test --workspace --no-default-features --locked`, and `npm run tauri -- build --debug --no-bundle` pass on the Windows host. `scripts/windows-smoke.ps1` exercises the actual Win32 backend and Engine with PID-scoped disposable windows. See [the dated validation record](validation/windows-2026-09-26.md) for actual GUI coverage and remaining limits. Host harness checks for X11/macOS are not target-platform runtime verification.
