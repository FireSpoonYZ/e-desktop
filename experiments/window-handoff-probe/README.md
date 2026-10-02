# Window handoff probe — stage 0 only

Standalone public Win32 experiment, not linked to the product. One explicitly named
disposable handoff fixture per invocation. **The worker has not performed a visual run.
Build, tests, API ordering, self-test or exit 0 do not prove cold-C is fixed.**

## Build / non-desktop checks

From the repository root, on Windows:

```powershell
cargo build --manifest-path experiments/window-handoff-probe/Cargo.toml --locked --offline
cargo test --manifest-path experiments/window-handoff-probe/Cargo.toml --locked --offline
cargo clippy --manifest-path experiments/window-handoff-probe/Cargo.toml --all-targets --locked --offline -- -D warnings
$probe = "experiments/window-handoff-probe/target/debug/window-handoff-probe.exe"
& $probe --help
& $probe --self-test
```

Own workspace/lock/target; no root workspace changes. Official windows-sys and serde
only. All six tests are pure logic or memory-DC: no HWND or screen DC. Self-test also
uses only logic/memory DCs. **Without --run there are no native preflight calls, window
creation, capture, output-directory creation or desktop mutations.** No args prints help.

## Parent-only visual invocation (NOT executed by worker)

Parent starts fresh handoff-fixture.exe processes and records full image paths,
PID/HWND/tag/role, cold starts, paint delay and recordings. Do not warm C before staged.
Output must be a **new** directory with an already existing parent. Illustrative
placeholders, not commands for the current desktop:

```powershell
& $probe --run --pid <fixturePID> --hwnd <hexHWND> --tag <token> --role C --mode staged --output <newTrialDirectory> --target "x,y,width,height" --cancel-file <newCancelFile>
```

Use identical target/animation/observation parameters and fixture paint delay for all
three modes. Target is the physical **visible** frame in screen pixels, not outer,
client, WinForms logical size or WINDOWPLACEMENT work-area coordinates. Negative origins
are supported. Measured padding converts target to outer; both are logged.
The whole-window animation is deliberately linear, identical across modes, not the
product spring. This is an independent baseline, not exact product timing.
The cover is a neutral opaque #262626 union of old/target outer rectangles, not a desktop
capture, wallpaper reconstruction or multi-window compositor.

**Parent MUST independently bound total real-machine trial/process time.** Consumer
deadlines DO NOT cancel native PrintWindow or other synchronous Win32/DWM calls.
There is exactly one detached capture worker and at most one request per trial. A hung
call retains its own DCs/bitmaps and identity/wake handles until it returns or the process
exits. No join, replacement worker, thread kill or cross-thread resource release.
Allow bounded graceful cancellation first (create --cancel-file or Ctrl+C), then check
restoration. Hard kill/console close/crash cannot promise cleanup: parent must recover
or terminate its disposable fixture. No SendInput, global hooks or system-setting edits.

## Admission / protection

Exact PID/HWND and title: e-desktop Handoff <tag> <role>. Tag: 1..64 ASCII
letters/digits/hyphens; role A..E. A live process handle plus a unique per-HWND cookie
is retained; PID/title/cookie/liveness are rechecked. Initial state must be visible,
normal, resizable, non-maximized, unowned, not topmost/cloaked/no-redirection, with no
custom region or visible same-process/owned companion. Active mouse capture, menus or
move/size interactions are refused. Initially iconic/hidden/unknown-old-geometry windows
are rejected. Last drawable geometry is measured **in this trial before minimize**,
separately from original undo/work-area placement.

Old outer, target outer and animation path must fit one monitor work area with unchanged
DPI and trusted padding. No offscreen/cross-screen parking, fullscreen/maximized path,
private cloak/ABI, injection or WGC. Covered operation rechecks monitor/DPI, identity,
ordinary state, protection admission, containment and overlapping windows above cover.
Unsafe changes cancel presentation and attempt original restore.

Successful nonzero GetWindowDisplayAffinity ALWAYS refuses. Unknown metadata defaults
to refusal. Supervisor-approved controlled fixture exception:
**--fixture-only-allow-unknown-affinity** allows only a non-layered ordinary source whose
query fails with ERROR_INVALID_PARAMETER (87), whose exact identity passed, and whose
process image basename is handoff-fixture.exe (case-insensitive). Parent must compare
the logged **full image path** with its build/start manifest. The basename is NOT
security authentication for arbitrary programs. Trial/events explicitly report
protectionMetadata=unavailable, admission=explicit-owned-fixture-exception and raw
error; they do NOT claim API-proven unprotected status. Layered sources, other errors
or known protected values still refuse. Source affinity/style is never changed to bypass
protection. This exception must not become the product default.

## Three paths / presentation

- **baseline:** no PrintWindow/frame. Minimize, restore directly at requested target
  geometry behind committed cover, animate the live whole-window DWM thumbnail.
  Source size comes from DwmQueryThumbnailSourceSize.
- **prehide:** asynchronously capture the still drawable full window within prehide
  consumer budget, pin the accepted immutable frame, then minimize; fixed proxy during
  restore/resize/animation, never replaced by a late result.
- **staged:** starts without a frame. Minimize, commit opaque cover + DwmFlush, restore
  at last measured drawable geometry and capture within one combined restore+capture
  budget; pin that frame, then request target and animate fixed proxy.

Capture-time outer/visible/client/DPI must match before and after native capture.
Controller validates generation/identity/current geometry/deadline. All-black captures
are conservatively rejected, including genuinely black applications.
Controller owns the HWND, pumps bounded message batches and waits with
MsgWaitForMultipleObjectsEx. Source placement is requested asynchronously; geometric
settling waits pump, not client-ready waits. Completion sends pixels/result on a bounded
private channel and signals an event, without placement callbacks. Reset-and-recheck
closes the lost-wake race. Cancellation/deadline invalidates generation; late results
can only be logged/released, never reposition or replace pinned pixels.

A reusable offscreen DIB renders the complete opaque scene; one BitBlt submits it.
New cover is transparent while first full paint is submitted, then opaque. DwmFlush
means only a caller submission boundary. Staged restoration/resize follows this.
Frozen modes have no live DWM layer over their GDI proxy. Cover absorbs local mouse hits
without activation/replay. Keyboard/global shortcuts are not intercepted; existing
capture/menu interactions are refused. Native foreground is logged; **DOM focus is
not observed by the probe and parent must measure it separately.**

Cover/input gate is removed at bounded animation end or absolute target-apply+handoff
deadline. Native overrun is degraded, not called a guaranteed hard deadline.
No marker, fixture JSON, equal frames, dimensions, timestamps, capture completion,
flush or elapsed time proves semantic client/business readiness: semanticReady and
visualPass are always null. Frozen-path missing/failed/timed-out frames explicitly
degrade to live fallback. Unsafe/resource/native failures stop and restore.
Degradation is NOT a visual pass.

## Budgets / output / cleanup

Defaults: --prehide-ms 100, --staged-ms 150 (each 1..5000); --animation-ms 160
(0..5000); --handoff-ms 250 (1..10000, >=animation). Handoff includes animation,
not another 250 ms after it. --minimized-hold-ms 200 and --observe-ms 300 (0..5000)
are **observation dwell times, NOT readiness criteria or sleeps**; messages continue
pumping. Parent separately budgets these, logging, native operations and cleanup.

UTF-8 output: events.jsonl, trial.json; accepted captures also capture-1.bgra/json.
BGRA8 is top-down, tightly packed, opaque, long edge <=1024, <=4 MiB; metadata carries
pixel size/stride, actual source outer/visible/client/pad/DPI, capture/generation/cookie,
native capture start/completion QPC. Common fields: schemaVersion=1, trialId, mode, PID,
hex HWND, relative monotonicUs, QPC/frequency, intent generation. Clock calibration
includes Unix microseconds; use cross-process QPC for alignment, not readiness.

Sampling is reported in trial/capture metadata: capture reduction and frozen proxy
StretchBlt use the new memory DC's GDI default BLACKONWHITE, **not an explicitly selected
nearest mode**. Whole-scene BitBlt does not resample; baseline DWM sampling is unknown.
The fixture analyzer only supports its explicit nearest mapping and lossless pixels;
do not label these scaled captures/proxies or lossy recordings as automatic pixel
passes. They may be unknown; this probe does not run or consume analyzer results.

Events cover original state, minimize, cover, capture request/complete/reject, staged
restore, target request/observed placement, pin/proxy presentation, handoff/input unblock,
observation, degradation/cancel, restore and finish. Trial reports capture wait,
extra prepare latency, target-apply-to-handoff/input-unblock time, restore outcome.
Completed-unverified exit 0 is procedural only. Refused/degraded/cancelled/failed returns
nonzero; pre-ownership refusal needs no geometry restore.

One raw capture DIB <=32 MiB, one reduced/pinned frame <=4 MiB, one scene <=64 MiB.
Reduced-DIB/Vec or Vec/pinned-DIB copies briefly coexist (<=8 MiB), within the 32 MiB
frame-store ceiling. No LRU/cache/cross-trial reuse. Raw worker capture is separate.
Runtime, JSON/file IO, OS/DWM/window backing allocations are NOT these memory ceilings.

Before first mutation save original normal WINDOWPLACEMENT/show/visibility/null-region
and DWM transition flag separately from last drawable geometry. Cleanup invalidates
presentation, removes cover/input gate, attempts original normal placement without
activation (two passes for WinForms restore), region and transition flag; checks original
physical outer/native normal/visibility. Only matching live identity may be restored;
destroyed/reused HWNDs are never mutated. Failed restoration is logged and returns
nonzero; parent owns fixture recovery. Forced termination/native hangs are not cleanup
guarantees. Native placement acceptance/geometry does not verify pixels or DOM focus.

Parent review and isolated cold-C recordings for all modes, restoration, focus/input,
delayed repaint, cancel/hang/unsupported source and resource/latency observation remain.
The probe neither proves improvement nor aggregates P50/P95/P99. Do not promote to
product integration until the design's real-machine gate passes.
