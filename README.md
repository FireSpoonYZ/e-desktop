# e-desktop

Rust + Tauri 2 + React desktop-window manager foundation, aimed at niri-like horizontal columns, vertical stacks and pages. This baseline **does not manage native windows**. It starts paused and advertises no platform capabilities; the start-tiling control is unavailable until a real backend is integrated.

## Development

Prerequisites: current stable Rust, Node.js 22.12+ and npm; Windows MSVC build tools, Windows SDK and WebView2. Linux/macOS additionally need their normal Tauri platform prerequisites; neither platform was built on this Windows host.

```sh
npm ci
npm run typecheck
npm run build
npm test
cargo test --workspace --no-default-features
cargo build --workspace
```

`npm run dev` serves the frontend alone on 127.0.0.1:1420; IPC needs Tauri. `npm run tauri -- dev` launches native windows. `npm run tauri -- build --no-bundle` builds a release executable without installers. Do not run GUI/native-window experiments in component workers; the integration owner performs them explicitly.

See [implementation contract](docs/implementation-contract.md) for shared types, lane ownership and precise signatures. No phone, remote/network service, tunnel, tasks/projects or LLM integration. No claim of equivalent Windows/X11/macOS support; ordinary Wayland clients cannot generally manage other applications' windows.
