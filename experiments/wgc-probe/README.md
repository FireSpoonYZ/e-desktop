# WGC → D3D11 texture probe

Standalone **Windows-only experiment**, not part of e-desktop or its workspace.
Requires Windows 10 **1903 (build 18362)** or newer (public HWND capture interop),
a working hardware D3D11 device, and Rust with the Windows MSVC toolchain.
Uses the already-cached `windows` 0.61.3 projections for WinRT/public COM interop.
No private exports, hooks, injected code, window control, renderer, image files,
or production integration.

## Build and checks (no capture)

Run from the repository root; the target directory is isolated from other lanes:

```powershell
cargo check --manifest-path experiments/wgc-probe/Cargo.toml --locked --target-dir experiments/wgc-probe/target
cargo test --manifest-path experiments/wgc-probe/Cargo.toml --locked --target-dir experiments/wgc-probe/target
cargo build --manifest-path experiments/wgc-probe/Cargo.toml --locked --target-dir experiments/wgc-probe/target
```

Add `--offline` when the crate cache is populated. Tests exercise only argument
validation and the projection's empty-frame representation: no HWND validation,
COM initialization, D3D device creation or capture is executed by tests.

## Usage — operator only

```text
wgc-probe <HWND decimal or 0xHEX> [duration-ms: 1..30000, default 3000]
```

Supply the handle of a **specific live top-level window** that you consent to
capture. The probe never selects a foreground window or enumerates targets.
Get the handle from your existing diagnostic tool; do not paste an example
handle without verifying the target.

Example (replace the handle; this command really starts capture):

```powershell
experiments/wgc-probe/target/debug/wgc-probe.exe 0x123456 3000
```

The probe validates HWND/root/PID/thread, initializes WinRT in an MTA, creates
`GraphicsCaptureItem` through public `IGraphicsCaptureItemInterop::CreateForWindow`,
wraps a BGRA-capable D3D11 device as `IDirect3DDevice`, and starts a two-buffer
`CreateFreeThreaded` pool. No dispatcher/message pump is required. It polls
approximately every 10 ms, obtains each real `ID3D11Texture2D` through
`IDirect3DDxgiInterfaceAccess`, then reports:

```text
frame=<count> content=<w>x<h> texture=<w>x<h> format=<DXGI integer> timestamp_100ns=<SystemRelativeTime>
summary frames=<count> elapsed_ms=<elapsed>
```

Output above is a **schema, not recorded capture evidence**. Timestamp units are
100 ns on the system-relative clock, not Unix/wall time. Format 87 is BGRA8 UNORM.
During resize, content size can differ from the current texture size for a frame;
the pool is recreated only after all frame/texture/surface references are released.
Every retrieved frame is closed, even on inspection error; teardown closes the
session before the pool and releases COM objects before uninitializing WinRT.

Exit code 0 means at least one non-empty texture was obtained and the timed sample
completed. A zero-frame timeout, invalid handle, target destruction/identity change,
unsupported capture, or API failure exits nonzero. Individual API errors retain
their Windows error text/HRESULT. Teardown errors are logged to stderr.
The source is never moved, activated, restored or minimized by this probe.
Normal system capture indication/border is left enabled.

## Bounded manual smoke — parent/operator, not automated here

1. Build first. Choose a disposable, restored, visible, non-protected window and
   verify its HWND. Run a **3000 ms** sample. Record OS build, GPU/driver, handle,
   exit code, all output, frame count, content/texture sizes and timestamps.
   Expect count > 0, positive texture dimensions and nondecreasing timestamps;
   animate the target manually if you need multiple changing frames.
2. Run another 3000 ms sample while manually resizing that same window. Confirm
   dimensions follow the new size without errors. This is not a visual-quality
   test: the probe obtains textures but does not display or read back their pixels.
3. Optional separate 3000 ms samples with the target occluded or manually
   minimized; record observed count/timestamps, including zero-frame/non-updating
   outcomes. **Do not infer that minimized windows keep producing fresh frames.**
4. Optional target-close sample: close the disposable source manually during the
   3000 ms sample. Expect a nonzero source-closed/API error, no surviving probe.
   Numeric HWND/PID/thread checks reduce reuse confusion but are not a durable
   identity across same-thread handle reuse; the capture item remains the original.
5. A duration bounds the polling sample, not a hung Windows driver/API or blocked
   console output. For a hard smoke-test ceiling, launch the built probe from a
   parent process and kill **only that probe** if it exceeds 10 seconds. For example:

```powershell
# Replace this handle with the verified, consented target's HWND.
$probe = Start-Process experiments/wgc-probe/target/debug/wgc-probe.exe `
    -ArgumentList @('0x123456', '3000') -PassThru `
    -RedirectStandardOutput wgc-smoke.out.txt -RedirectStandardError wgc-smoke.err.txt
if (-not $probe.WaitForExit(10000)) {
    $probe.Kill()
    $probe.WaitForExit()
    throw 'WGC smoke exceeded 10-second ceiling; capture not accepted'
}
$probe.Refresh()
$probe.ExitCode
Get-Content wgc-smoke.out.txt
Get-Content wgc-smoke.err.txt
```

Do not mistake received-frame count for changing pixels, HDR fidelity, protected
content access, per-monitor presentation, or minimized-window support. There is
no software-device fallback, display/compositor, screenshot encoding or cache.

## Validation recorded for this implementation

Compiled/checked on `x86_64-pc-windows-msvc`; four focused unit tests passed.
**Capture was not executed. Frames obtained during development: none.**
All live capture, resize, source-close, occlusion, minimize, driver and visual
behavior above remains for the parent/operator to test and accept.
