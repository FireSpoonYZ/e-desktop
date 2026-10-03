# Exact Orca node-pty deployment

This lane reuses config/patches/node-pty@1.1.0.patch byte-for-byte from
stablyai/orca commit de8bffe24045b396212f4f63de8960ec8380ea07 (Git blob
40b240f3296c050167062eff0c8e920c3d13c937). Its SHA256 is
92c95cffab383d86b3b13a460c75a08074468ebf1f3911db8e874e083201f192.
native-provenance.json identifies every modified package file by pristine and
patched SHA256. Git applies with explicit LF output, independent of Windows
checkout defaults. Mixed, unexpected-version, modified, and partially patched
trees fail instead of being treated as stock or patched.

## Runtime/build contract

The existing desktop uses an **external Node**, resolved by Rust from
E_DESKTOP_NODE or PATH. Neither the incoming resource preparation nor Rust
bundles Node. This port does not add a runtime or change Rust resolution.

Set E_DESKTOP_NODE to an absolute path to the actual desktop host Node
(>=22.12). Without it, preparation uses process.execPath. For example on Windows:

    $env:E_DESKTOP_NODE = 'D:\software\nvm\v22.19.0\node.exe'
    npm run prepare:terminal-host

In terminal-host alone:

    npm ci
    npm run check:deployment
    npm run test:native
    npm run smoke:native

npm install also invokes the same prepare lifecycle. npm ci is the release
preparation path and requires the committed lock and development build tools.
If lifecycle scripts were explicitly disabled, run npm run rebuild:native and
npm run build before the deployment check. No stock prebuild is accepted.

prepare runs the exact patch, deletes all prebuild/debug/native-build candidates,
and invokes pinned node-gyp 11.4.2 using the selected Node itself, its version
headers and architecture. node-addon-api is pinned to 7.1.1; node-pty to 1.1.0.
Install normal native platform tools (Windows MSVC BuildTools with Windows SDK,
or the platform C++ toolchain/Python). No MSYS SDK/system DLL replacement is used.
Upstream post-install stages its own conpty.dll and OpenConsole.exe into
build/Release/conpty. orca-native-build.json records the executable, Node version,
ABI, platform, architecture, build dependencies, patch ID and generated hashes.

The deployment gate verifies the exact source, resource inventory and hashes,
then actually loads the Release addon, checks the exact native export set and
proves node-pty resolves the same loaded module. It runs before resource copying
and again from the copied resource. The Windows smoke creates PowerShell, Nu and
Bash PTYs with the Orca DLL backend, observes output and explicit-close physical
exit. It is not an interactive input/resize or first-character fix assertion.

The exact patch intentionally builds only conpty.node on Windows (not legacy
winpty or conpty_console_list.node). Integration must use upstream Windows
useConpty:true/useConptyDll:true; the incoming non-DLL launch is not compatible
with the reduced upstream build. Integrators must merge the Session build entries
and dependencies into package.json without removing this native prepare step.

A different runtime selected later by Rust is not prevented by a build-time
check. Integration must run the deployment gate under the actually selected
Node before starting the server, and fail actionably on mismatch. Rebuild with
that executable or keep E_DESKTOP_NODE consistently set. Do not fall back to a
stock/debug addon. The root prepare script never changes original installations.

## Licenses and limits

ORCA-LICENSE is Orca's MIT license. NODE-PTY-LICENSE and
NODE-ADDON-API-LICENSE reproduce their npm source licenses; their installed
copies are node_modules/node-pty/LICENSE and
node_modules/node-addon-api/LICENSE.md. node-pty ships the versioned Microsoft
ConPTY DLL/EXE in third_party/conpty/1.23.251008001/win10-{x64,arm64};
the npm package provides no separate license file in that directory. This is
reported for release notice review, not silently assigned an invented license.

The diagnostic rebuilt ABI147 addon is not copied. This lane compiles actual
source for the chosen runtime and records each build hash; MSVC output is not
claimed to be byte-identical across rebuilds. Other operating systems have
source/build paths but have not been validated by this Windows lane.
