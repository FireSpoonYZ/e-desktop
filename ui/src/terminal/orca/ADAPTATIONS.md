# Orca reuse

Read-only source: D:/project/.agent-work/e-terminal/orca-reference
Commit: de8bffe24045b396212f4f63de8960ec8380ea07
License: MIT, Copyright (c) 2026 Lovecast Inc. Full notice in LICENSE.

Copied pure modules, unchanged except provenance headers:
- terminal-query-reply.ts ← src/shared/terminal-query-reply.ts
- terminal-kitty-keyboard-flags.ts ← src/shared/terminal-kitty-keyboard-flags.ts
- terminal-unicode-provider.ts ← src/shared/terminal-unicode-provider.ts

xterm.patch is the exact lib/xterm.mjs diff extracted from upstream
config/patches/@xterm__xterm@6.1.0-beta.303.patch. It is applied to the
pinned npm @xterm/xterm 6.1.0-beta.303 by the root postinstall script.
Only the ESM runtime used by Vite is patched; the CJS bundle, TypeScript
sources and source maps are not shipped as patched runtime alternatives.
The upstream CompositionHelper / WidthCache fixes are reused, not rewritten.
Xterm's original MIT notice is in XTERM-LICENSE and remains in the bundle.
No image addon, daemon, Electron graph, or mobile document graph is copied.

../input-authority.ts is a NEW small desktop adapter, not an Orca source copy.
It preserves synchronous xterm coreService.triggerDataEvent wasUserInput
provenance before public onData loses it. Known user input bypasses Orca's
unchanged reply classifier (modified F3 and CPR have identical bytes).
Parser replies are discarded locally; the sidecar alone answers queries.
The adapter uses try/finally, restores the previous function on disposal,
and fails explicitly if the pinned private xterm seam is missing.
Tests instantiate the real patched ESM xterm core without a DOM/browser.

../stream.ts is a new wire-v1 adapter: ordered bounded snapshot/live queue,
sequence continuity, reset/resize, and Kitty flags seeded BEFORE snapshot
ANSI, because its last bytes can be an unfinished escape sequence.
Every renderer and the host must activate the same Orca Unicode provider.
