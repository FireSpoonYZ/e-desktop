# Third-party notices

## Orca terminal components

Source: https://github.com/stablyai/orca
Pinned commit: `de8bffe24045b396212f4f63de8960ec8380ea07`
License: MIT, Copyright (c) 2026 Lovecast Inc.

The terminal host adapts Orca's headless terminal, screen snapshot/mode restoration, partial escape tracking, Unicode width and cursor serialization helpers. Original paths and adaptations are recorded in [host documentation](docs/terminal-host.md) and source headers. Full license: [terminal-host/vendor/orca/LICENSE](terminal-host/vendor/orca/LICENSE).

The desktop renderer reuses terminal query classification, Kitty flags, Unicode handling and the exact pinned xterm ESM patch. See [renderer adaptations](ui/src/terminal/orca/ADAPTATIONS.md), [Orca MIT license](ui/src/terminal/orca/LICENSE) and [xterm.js MIT license](ui/src/terminal/orca/XTERM-LICENSE).

Installed dependencies retain their respective licenses within their packages. No Orca cloud service is bundled.

## Orca Session and native dependency port

The host now directly vendors Orca's Session, subprocess, shell launch/environment,
query/startup handling, terminal writer/driver and headless emulator dependency graph.
The 293 source modules and four import-binding adaptations are listed in
[the source manifest](terminal-host/src/orca/provenance.json); this is build-input
provenance, not a claim that every function or the entire Orca application executes.
Full MIT text: [source license](terminal-host/src/orca/LICENSE).

The exact pinned `config/patches/node-pty@1.1.0.patch` is applied to node-pty 1.1.0
and rebuilt for the external Node runtime. Patch, original/patched hashes and
licenses: [native manifest](terminal-host/orca-patches/native-provenance.json),
[Orca](terminal-host/orca-patches/ORCA-LICENSE),
[node-pty](terminal-host/orca-patches/NODE-PTY-LICENSE),
[node-addon-api](terminal-host/orca-patches/NODE-ADDON-API-LICENSE).
The Microsoft ConPTY DLL/OpenConsole resources come from node-pty's pinned
`third_party/conpty` distribution; they are not a modified user MSYS/Git runtime.
