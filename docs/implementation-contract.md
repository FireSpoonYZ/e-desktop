# Implementation contract — baseline v1

Shared source of truth: `src-tauri/src/model.rs` and `ui/src/model.ts`. Changes require parent coordination. Baseline is a compiling seam, **not implemented window management**. Starts paused; every mutation returns `notImplemented`; no shortcuts are registered. No GUI was launched during baseline work.

## Wire and state

- Serde camelCase fields; commands/actions use internally tagged `{ "type": "..." }`. IDs are opaque strings, never JS numeric native handles. Optional fields serialize as `null`.
- `get_snapshot() -> Snapshot`; `execute(command: Command) -> Result<Snapshot, AppError>`; JS `invoke('execute', { command })`. Intended event: `snapshot` with complete `Snapshot`; baseline emits none.
- Commands: refresh, enable, disable, focusWindow(windowId), focusDirection(direction), switchPage(monitorId,pageId), addPage(monitorId), moveWindowToPage(windowId,pageId), moveWindow(direction), cycleWidth, centerFocused, scroll(monitorId,delta), toggleFloating, toggleFullscreen, closeWindow(windowId). Directions: left/right/up/down. Scroll delta: signed physical horizontal pixels. Implicit commands address focused window/active monitor. Page IDs are globally unique within a snapshot.
- Physical screen-pixel Rect has signed x/y and unsigned width/height. UI converts physical geometry to CSS using monitor scaleFactor. MonitorState owns pages, activePage and usable viewport; each Page retains viewportX, ordered columns and floatingWindows. Columns contain top-to-bottom window IDs. Native metadata lives in Snapshot.windows[].native. No task/project objects.
- Floating exits automatic placement; fullscreen means layout fills the usable monitor viewport, **not** OS-native fullscreen. Layout/integration must agree control-window reservations before enabling management.
- BackendStatus explicitly separates availability from capabilities. `ready` does not imply every capability. No fabricated sample windows or silent successful unsupported operations.

## Exclusive lanes and signatures

| Owner | Files / exports |
| --- | --- |
| Layout | `src-tauri/src/layout/`: `Engine::new(BackendStatus) -> Self`, `snapshot(&self) -> &Snapshot`, `reconcile(&mut self, SystemSnapshot) -> Result<Transition, AppError>`, `dispatch(&mut self, Command) -> Result<Transition, AppError>` |
| Windows | `src-tauri/src/platform/windows/mod.rs`: `pub struct Backend` with methods below |
| Linux | `src-tauri/src/platform/linux/mod.rs`: same Backend methods; reject unsupported Wayland sessions explicitly |
| macOS | `src-tauri/src/platform/macos/mod.rs`: same Backend methods; report AX permission requirements |
| Shell | `ui/src/shell/index.tsx`: named `TopBar(TopBarProps)`, `PageRail(PageRailProps)` |
| Overview/commands | `ui/src/overview/index.tsx`: named `Overview(OverviewProps)`; `ui/src/commands/index.tsx`: named `CommandPalette(CommandPaletteProps)` |
| Parent integration | `App.tsx`, `bridge.ts`, `src-tauri/src/app.rs`, `platform/mod.rs`, manifests/locks, shared models and theme |

Backend inherent methods (no trait registry):

```rust
pub fn new() -> Result<Self, AppError>;
pub fn status(&self) -> BackendStatus;
pub fn enumerate(&mut self) -> Result<SystemSnapshot, AppError>;
pub fn apply(&mut self, actions: &[NativeAction]) -> Result<(), AppError>;
pub fn restore(&mut self) -> Result<(), AppError>;
```

Parent replaces baseline `platform/mod.rs` stub with target-cfg Backend reexports. Platform modules do not call layout or Tauri. No preview signature/dependency is frozen; coordinate before adding thumbnail capture. Windows windows-sys and Linux x11rb dependencies are already declared. macOS may use narrow native framework FFI; dependency additions belong to parent.

NativeAction separates Placement(rect,clip,minimized), Focus, Close and per-window Restore. Clip uses physical screen coordinates; backend converts to native region coordinates. `None` removes manager-owned clipping, not pre-existing application regions. Preserve original native placement/region/state before first mutation. Retain manager-minimized windows in enumeration; absence is not closure unless native lifetime checks confirm it. Fully offscreen windows use recoverable minimization, never inaccessible hiding. Close is graceful, not process termination. Disable/normal exit call restore; restore must attempt all owned windows and surface failure. Partial apply failures require fresh enumeration, not claiming all actions succeeded. Abnormal-exit restoration is not guaranteed by this baseline.

`Transition` contains updated Snapshot plus ordered NativeAction list. Pure layout has no OS/Tauri dependency; integration owns refresh/enumeration, native application, error publication, activation/restore lifecycle and global shortcuts. Never enable as a side effect of construction or refresh.

All UI props are exported from `ui/src/model.ts`: common `{snapshot, onCommand, busy?}`; TopBar adds `onOpenOverview`, `onOpenCommands`; PageRail adds optional `monitorId`; Overview and CommandPalette add `onDismiss`. `onCommand: (Command) => void | Promise<void>`. Components do not invoke IPC or own a second layout state. Import named exports from their directory; all skeletons participate in App's typecheck/build. Parent handles showing/hiding native surfaces. Topbar is the only initially visible window; pagerail, overview and commands remain hidden until integration positions/shows them. No fullscreen daily web overlay.

## Checks

From repository root: `npm ci`; `npm run typecheck`; `npm run build`; `npm test`; `cargo test --workspace --no-default-features`; `cargo build --workspace`. `npm run tauri -- dev` launches GUI (parent only); `npm run tauri -- build --no-bundle` builds desktop release. Both lockfiles are committed. Only Windows compilation has been checked; Linux/macOS and all real-window behavior require independent verification.
