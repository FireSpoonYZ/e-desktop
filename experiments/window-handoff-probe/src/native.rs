//! Public Win32 stage-0 experiment. All HWND mutation belongs to the pumping thread.
//! The sole PrintWindow call owns its DCs on a detached worker until actual return.
use crate::logic::*;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    mem::{size_of, zeroed},
    path::Path,
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    Storage::Xps::PrintWindow,
    System::{Console::*, LibraryLoader::GetModuleHandleW, Performance::*, Threading::*},
    UI::{HiDpi::*, WindowsAndMessaging::*},
};

#[path = "offline.rs"]
mod offline;
pub use offline::offline_proxy;

static CANCELLED: AtomicBool = AtomicBool::new(false);
unsafe extern "system" fn console_cancel(kind: u32) -> i32 {
    if kind == CTRL_C_EVENT || kind == CTRL_BREAK_EVENT {
        CANCELLED.store(true, Ordering::Release);
        1
    } else {
        0
    } // Forced console close/termination cannot promise restoration.
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn win(ok: i32, name: &str) -> Result<(), String> {
    if ok == 0 {
        Err(format!("{name}: {}", std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}
fn hr(value: i32, name: &str) -> Result<(), String> {
    if value < 0 {
        Err(format!("{name}: HRESULT {value:#x}"))
    } else {
        Ok(())
    }
}
fn qpc() -> i64 {
    let mut v = 0;
    unsafe { QueryPerformanceCounter(&mut v) };
    v
}
fn hwnd_string(h: HWND) -> String {
    format!("0x{:x}", h as usize)
}
fn native(r: Rect) -> RECT {
    RECT {
        left: r.x,
        top: r.y,
        right: r.right(),
        bottom: r.bottom(),
    }
}
fn rect(r: RECT) -> Result<Rect, String> {
    Rect::new(
        r.left,
        r.top,
        r.right.checked_sub(r.left).ok_or("rectangle overflow")?,
        r.bottom.checked_sub(r.top).ok_or("rectangle overflow")?,
    )
}
fn outer(h: HWND) -> Result<Rect, String> {
    let mut r = unsafe { zeroed() };
    win(unsafe { GetWindowRect(h, &mut r) }, "GetWindowRect")?;
    rect(r)
}
struct DpiScope(isize);
impl DpiScope {
    fn enter() -> Result<Self, String> {
        let old =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if old.is_null() {
            return Err("SetThreadDpiAwarenessContext failed".into());
        }
        Ok(Self(old as isize))
    }
}
impl Drop for DpiScope {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0 as _);
        }
    }
}
struct Handle(usize);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0 as HANDLE);
        }
    }
}
struct Wake(Handle);
impl Wake {
    fn new() -> Result<Arc<Self>, String> {
        let h = unsafe { CreateEventW(null(), 1, 0, null()) };
        if h.is_null() {
            return Err("CreateEventW failed".into());
        }
        Ok(Arc::new(Self(Handle(h as usize))))
    }
    fn handle(&self) -> HANDLE {
        self.0.0 as HANDLE
    }
    fn signal(&self) {
        unsafe {
            SetEvent(self.handle());
        }
    }
    fn reset(&self) -> Result<(), String> {
        win(unsafe { ResetEvent(self.handle()) }, "ResetEvent")
    }
}

struct Log {
    file: File,
    config: Config,
    trial_id: String,
    start: Instant,
    qpc_start: i64,
    qpc_frequency: i64,
    unix_start_us: u128,
}
impl Log {
    fn new(config: Config) -> Result<Self, String> {
        // Never overwrite another trial or parent evidence.
        fs::create_dir(&config.output).map_err(|e| format!("new output directory: {e}"))?;
        let unix_start_us = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_micros();
        let mut qpc_frequency = 0;
        win(
            unsafe { QueryPerformanceFrequency(&mut qpc_frequency) },
            "QueryPerformanceFrequency",
        )?;
        let file = File::create(config.output.join("events.jsonl")).map_err(|e| e.to_string())?;
        Ok(Self {
            file,
            trial_id: format!("{}-{}-{unix_start_us}", config.tag, std::process::id()),
            config,
            start: Instant::now(),
            qpc_start: qpc(),
            qpc_frequency,
            unix_start_us,
        })
    }
    fn event(&mut self, event: &str, generation: u64, data: Value) -> Result<(), String> {
        let value = json!({
            "schemaVersion": 1, "trialId": self.trial_id, "mode": self.config.mode,
            "pid": self.config.pid, "hwnd": format!("0x{:x}", self.config.hwnd),
            "monotonicUs": self.start.elapsed().as_micros(), "qpc": qpc(),
            "qpcFrequency": self.qpc_frequency, "intentGeneration": generation,
            "event": event, "controllerThreadId": unsafe { GetCurrentThreadId() },
            "foregroundHwnd": hwnd_string(unsafe { GetForegroundWindow() }),
            "sourceVisible": unsafe { IsWindowVisible(self.config.hwnd as HWND) } != 0,
            "sourceIconic": unsafe { IsIconic(self.config.hwnd as HWND) } != 0,
            "transitionPolicy": "unchanged-by-probe", "transitionOriginalQueried": false,
            "data": data,
        });
        serde_json::to_writer(&mut self.file, &value).map_err(|e| e.to_string())?;
        self.file
            .write_all(b"\n")
            .and_then(|_| self.file.flush())
            .map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Geometry {
    outer: Rect,
    visible: Rect,
    client: Rect,
    dpi: u32,
    pad: [i32; 4],
}
fn geometry(h: HWND) -> Result<Geometry, String> {
    if unsafe { IsIconic(h) } != 0 || unsafe { IsWindowVisible(h) } == 0 {
        return Err("source not drawable".into());
    }
    let outer = outer(h)?;
    let mut visible = unsafe { zeroed::<RECT>() };
    hr(
        unsafe {
            DwmGetWindowAttribute(
                h,
                DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                &mut visible as *mut _ as _,
                size_of::<RECT>() as u32,
            )
        },
        "extended frame bounds",
    )?;
    let visible = rect(visible)?;
    let pad = [
        visible.x - outer.x,
        visible.y - outer.y,
        outer.right() - visible.right(),
        outer.bottom() - visible.bottom(),
    ];
    if !pad.iter().all(|v| (0..=32).contains(v)) {
        return Err("untrusted frame padding".into());
    }
    let mut client = unsafe { zeroed::<RECT>() };
    win(unsafe { GetClientRect(h, &mut client) }, "GetClientRect")?;
    let mut origin = POINT {
        x: client.left,
        y: client.top,
    };
    win(unsafe { ClientToScreen(h, &mut origin) }, "ClientToScreen")?;
    let client = Rect::new(
        origin.x,
        origin.y,
        client.right - client.left,
        client.bottom - client.top,
    )?;
    let dpi = unsafe { GetDpiForWindow(h) };
    if dpi == 0 {
        return Err("unknown source DPI".into());
    }
    Ok(Geometry {
        outer,
        visible,
        client,
        dpi,
        pad,
    })
}
fn target_outer(target: Rect, pad: [i32; 4]) -> Result<Rect, String> {
    Rect::new(
        target.x.checked_sub(pad[0]).ok_or("target overflow")?,
        target.y.checked_sub(pad[1]).ok_or("target overflow")?,
        target
            .width
            .checked_add(pad[0] + pad[2])
            .ok_or("target overflow")?,
        target
            .height
            .checked_add(pad[1] + pad[3])
            .ok_or("target overflow")?,
    )
}

/// No GDI handle crosses threads. The DC and selected bitmap die on their creator.
struct Dib {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    bytes: usize,
    w: i32,
    h: i32,
}
impl Dib {
    fn new(w: i32, h: i32, limit: usize) -> Result<Self, String> {
        let bytes = pixel_bytes(w, h, limit)?;
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        if dc.is_null() {
            return Err("CreateCompatibleDC failed".into());
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..unsafe { zeroed() }
            },
            ..unsafe { zeroed() }
        };
        let mut bits = null_mut();
        let bitmap =
            unsafe { CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0) };
        if bitmap.is_null() || bits.is_null() {
            unsafe {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                DeleteDC(dc);
            }
            return Err("CreateDIBSection failed".into());
        }
        let previous = unsafe { SelectObject(dc, bitmap) };
        if previous.is_null() || previous as isize == -1 {
            unsafe {
                DeleteObject(bitmap);
                DeleteDC(dc);
            }
            return Err("SelectObject failed".into());
        }
        unsafe {
            std::ptr::write_bytes(bits, 0, bytes);
        }
        Ok(Self {
            dc,
            bitmap,
            previous,
            bits: bits.cast(),
            bytes,
            w,
            h,
        })
    }
    fn pixels(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.bytes) }
    }
    fn background(&mut self) {
        for p in self.pixels().chunks_exact_mut(4) {
            p.copy_from_slice(&[38, 38, 38, 255]);
        }
    }
    fn blit(&self, dest: HDC) -> Result<(), String> {
        win(
            unsafe { BitBlt(dest, 0, 0, self.w, self.h, self.dc, 0, 0, SRCCOPY) },
            "complete scene BitBlt",
        )
    }
    fn render(&mut self, source: &Dib, destination: Rect) -> Result<(), String> {
        self.background(); // Entire offscreen surface is rebuilt; visible DC is never cleared.
        win(
            unsafe { SetStretchBltMode(self.dc, COLORONCOLOR) },
            "proxy COLORONCOLOR",
        )?;
        win(
            unsafe {
                StretchBlt(
                    self.dc,
                    destination.x,
                    destination.y,
                    destination.width,
                    destination.height,
                    source.dc,
                    0,
                    0,
                    source.w,
                    source.h,
                    SRCCOPY,
                )
            },
            "proxy StretchBlt",
        )?;
        win(unsafe { GdiFlush() }, "GdiFlush")
    }
}
impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

#[derive(Clone)]
struct Identity {
    hwnd: usize,
    pid: u32,
    property: Vec<u16>,
    cookie: usize,
    title: Vec<u16>,
    process: Arc<Handle>,
}
impl Identity {
    fn h(&self) -> HWND {
        self.hwnd as HWND
    }
    fn alive(&self) -> bool {
        let mut pid = 0;
        let mut title = vec![0; self.title.len() + 1];
        unsafe {
            if IsWindow(self.h()) == 0
                || GetWindowThreadProcessId(self.h(), &mut pid) == 0
                || GetWindowTextW(self.h(), title.as_mut_ptr(), title.len() as i32) as usize
                    != self.title.len() - 1
            {
                return false;
            }
            identity_matches(
                self.pid,
                pid,
                &self.title,
                &title[..self.title.len()],
                self.cookie,
                GetPropW(self.h(), self.property.as_ptr()) as usize,
                WaitForSingleObject(self.process.0 as HANDLE, 0) == WAIT_TIMEOUT,
            )
        }
    }
}
struct Admission {
    process: Arc<Handle>,
    image: String,
    affinity: Result<u32, u32>,
    name: &'static str,
}
fn affinity(h: HWND) -> Result<u32, u32> {
    let mut value = 0;
    if unsafe { GetWindowDisplayAffinity(h, &mut value) } != 0 {
        Ok(value)
    } else {
        Err(unsafe { GetLastError() })
    }
}
fn preflight(config: &Config) -> Result<Admission, String> {
    let h = config.hwnd as HWND;
    let mut pid = 0;
    win(
        unsafe { GetWindowThreadProcessId(h, &mut pid) } as i32,
        "GetWindowThreadProcessId",
    )?;
    if pid != config.pid {
        return Err("PID/HWND mismatch".into());
    }
    let title = wide(&format!("e-desktop Handoff {} {}", config.tag, config.role));
    let mut got = vec![0; title.len() + 1];
    let n = unsafe { GetWindowTextW(h, got.as_mut_ptr(), got.len() as i32) };
    if n as usize != title.len() - 1 || got[..title.len()] != title {
        return Err("exact fixture title mismatch".into());
    }
    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if process.is_null() {
        return Err("OpenProcess failed".into());
    }
    let process = Arc::new(Handle(process as usize));
    let mut path = vec![0; 32768];
    let mut count = path.len() as u32;
    win(
        unsafe {
            QueryFullProcessImageNameW(process.0 as HANDLE, 0, path.as_mut_ptr(), &mut count)
        },
        "process image",
    )?;
    let image = String::from_utf16(&path[..count as usize]).map_err(|e| e.to_string())?;
    let fixture_image = Path::new(&image)
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("handoff-fixture.exe"));
    let layered = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32 & WS_EX_LAYERED != 0;
    let affinity = affinity(h);
    let name = affinity_admission(
        affinity,
        layered,
        config.fixture_only_allow_unknown_affinity,
        fixture_image,
    )?;
    Ok(Admission {
        process,
        image,
        affinity,
        name,
    })
}

struct Saved {
    placement: WINDOWPLACEMENT,
    outer: Rect,
    visible: bool,
    // Admission rejects custom regions; saved original region is therefore explicitly None.
}
fn saved(h: HWND) -> Result<Saved, String> {
    let mut placement: WINDOWPLACEMENT = unsafe { zeroed() };
    placement.length = size_of::<WINDOWPLACEMENT>() as u32;
    win(
        unsafe { GetWindowPlacement(h, &mut placement) },
        "GetWindowPlacement",
    )?;
    let region = unsafe { CreateRectRgn(0, 0, 0, 0) };
    if region.is_null() {
        return Err("CreateRectRgn failed".into());
    }
    let kind = unsafe { GetWindowRgn(h, region) };
    unsafe {
        DeleteObject(region);
    }
    if kind != ERROR {
        return Err("custom source region unsupported; no mutation".into());
    }
    Ok(Saved {
        placement,
        outer: outer(h)?,
        visible: unsafe { IsWindowVisible(h) } != 0,
    })
}
fn monitor_bounds(m: HMONITOR) -> Result<Rect, String> {
    let mut info: MONITORINFO = unsafe { zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    win(unsafe { GetMonitorInfoW(m, &mut info) }, "GetMonitorInfoW")?;
    rect(info.rcWork)
}
unsafe extern "system" fn popup_check(h: HWND, param: LPARAM) -> i32 {
    let (target, pid, bad) = unsafe { &mut *(param as *mut (usize, u32, bool)) };
    let mut found = 0;
    unsafe {
        GetWindowThreadProcessId(h, &mut found);
    }
    if h as usize != *target
        && unsafe { IsWindowVisible(h) } != 0
        && (found == *pid || unsafe { GetWindow(h, GW_OWNER) } as usize == *target)
    {
        *bad = true;
    }
    1
}
fn ordinary(h: HWND, pid: u32) -> Result<(), String> {
    let style = unsafe { GetWindowLongPtrW(h, GWL_STYLE) } as u32;
    let ex = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32;
    if style & WS_CHILD != 0
        || style & WS_CAPTION != WS_CAPTION
        || style & WS_THICKFRAME == 0
        || ex & (WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOREDIRECTIONBITMAP) != 0
        || !unsafe { GetWindow(h, GW_OWNER) }.is_null()
        || unsafe { IsZoomed(h) } != 0
    {
        return Err("not an ordinary unowned resizable non-maximized window".into());
    }
    let thread = unsafe { GetWindowThreadProcessId(h, null_mut()) };
    let mut gui: GUITHREADINFO = unsafe { zeroed() };
    gui.cbSize = size_of::<GUITHREADINFO>() as u32;
    win(
        unsafe { GetGUIThreadInfo(thread, &mut gui) },
        "target GUI thread info",
    )?;
    if !gui.hwndCapture.is_null()
        || gui.flags & (GUI_INMOVESIZE | GUI_INMENUMODE | GUI_POPUPMENUMODE | GUI_SYSTEMMENUMODE)
            != 0
    {
        return Err("active capture/menu/move interaction cannot be safely input-covered".into());
    }
    let mut cloaked = 0u32;
    hr(
        unsafe {
            DwmGetWindowAttribute(
                h,
                DWMWA_CLOAKED as u32,
                &mut cloaked as *mut _ as _,
                size_of::<u32>() as u32,
            )
        },
        "cloaked attribute",
    )?;
    if cloaked != 0 {
        return Err("cloaked window unsupported".into());
    }
    let mut check = (h as usize, pid, false);
    win(
        unsafe { EnumWindows(Some(popup_check), &mut check as *mut _ as isize) },
        "EnumWindows",
    )?;
    if check.2 {
        return Err("visible same-process/owned popup cannot be safely covered".into());
    }
    Ok(())
}

struct Frame {
    generation: u64,
    geometry: Geometry,
    capture_qpc: i64,
    completed_qpc: i64,
    w: i32,
    h: i32,
    pixels: Vec<u8>,
}
struct CaptureResult {
    generation: u64,
    result: Result<Frame, String>,
}
struct Capture {
    receiver: mpsc::Receiver<CaptureResult>,
    cancelled: Arc<AtomicBool>,
}
fn capture_frame(
    identity: &Identity,
    expected: &Geometry,
    generation: u64,
    cancel: &AtomicBool,
) -> Result<Frame, String> {
    let _dpi = DpiScope::enter()?;
    if cancel.load(Ordering::Acquire) || !identity.alive() {
        return Err("cancelled/invalid identity before capture".into());
    }
    if geometry(identity.h())? != *expected {
        return Err("capture-time geometry changed".into());
    }
    let mut full = Dib::new(expected.outer.width, expected.outer.height, RAW_BYTES)?;
    let capture_qpc = qpc();
    win(
        unsafe { PrintWindow(identity.h(), full.dc, 2) },
        "PrintWindow PW_RENDERFULLCONTENT",
    )?;
    let completed_qpc = qpc();
    if cancel.load(Ordering::Acquire) || !identity.alive() {
        return Err("cancelled/invalid identity after capture".into());
    }
    if geometry(identity.h())? != *expected {
        return Err("geometry changed during native capture".into());
    }
    win(unsafe { GdiFlush() }, "capture GdiFlush")?;
    // Deliberately conservative: an actually black application is refused as well.
    if !full.pixels().chunks_exact(4).any(|p| p[..3] != [0, 0, 0]) {
        return Err("all-black capture unsupported".into());
    }
    let (w, h) = reduced_size(full.w, full.h);
    let mut reduced = Dib::new(w, h, FRAME_BYTES)?;
    win(
        unsafe { SetStretchBltMode(reduced.dc, COLORONCOLOR) },
        "capture reduction COLORONCOLOR",
    )?;
    win(
        unsafe {
            StretchBlt(
                reduced.dc, 0, 0, w, h, full.dc, 0, 0, full.w, full.h, SRCCOPY,
            )
        },
        "reduce capture",
    )?;
    win(unsafe { GdiFlush() }, "reduced GdiFlush")?;
    for p in reduced.pixels().chunks_exact_mut(4) {
        p[3] = 255;
    }
    let pixels = reduced.pixels().to_vec();
    Ok(Frame {
        generation,
        geometry: expected.clone(),
        capture_qpc,
        completed_qpc,
        w,
        h,
        pixels,
    })
}
impl Capture {
    fn start(
        identity: Identity,
        expected: Geometry,
        generation: u64,
        wake: Arc<Wake>,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let signal = wake.clone();
        std::thread::Builder::new()
            .name("handoff-printwindow".into())
            .spawn(move || {
                let result = capture_frame(&identity, &expected, generation, &flag);
                // try_send never blocks after the controller has cancelled or exited.
                let _ = sender.try_send(CaptureResult { generation, result });
                signal.signal();
            })
            .map_err(|e| e.to_string())?; // Intentionally detached; never join a synchronous capture.
        Ok(Self {
            receiver,
            cancelled,
        })
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct Backdrop {
    scene: Dib,
    paint_failed: bool,
}
unsafe extern "system" fn cover_proc(h: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let p = unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) } as *mut Backdrop;
    match message {
        WM_CLOSE | WM_DISPLAYCHANGE | WM_DPICHANGED => {
            CANCELLED.store(true, Ordering::Release);
            return 0;
        }
        WM_NCHITTEST => return HTCLIENT as LRESULT,
        WM_MOUSEACTIVATE => return MA_NOACTIVATEANDEAT as LRESULT,
        WM_MOUSEFIRST..=WM_MOUSELAST => return 0,
        WM_ERASEBKGND => return 1,
        WM_PAINT if !p.is_null() => {
            let mut paint: PAINTSTRUCT = unsafe { zeroed() };
            let dc = unsafe { BeginPaint(h, &mut paint) };
            let data = unsafe { &mut *p };
            data.paint_failed = data.scene.blit(dc).is_err();
            unsafe {
                EndPaint(h, &paint);
            }
            return 0;
        }
        _ => {}
    }
    unsafe { DefWindowProcW(h, message, w, l) }
}
struct Cover {
    hwnd: usize,
    bounds: Rect,
    backdrop: Box<Backdrop>,
    thumbnail: Option<isize>,
}
impl Cover {
    fn new(bounds: Rect) -> Result<Self, String> {
        let mut scene = Dib::new(bounds.width, bounds.height, SCENE_BYTES)?;
        scene.background();
        let mut backdrop = Box::new(Backdrop {
            scene,
            paint_failed: false,
        });
        let class = wide("e-desktop-window-handoff-probe");
        let instance = unsafe { GetModuleHandleW(null()) };
        let wc = WNDCLASSW {
            lpfnWndProc: Some(cover_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..unsafe { zeroed() }
        };
        if unsafe { RegisterClassW(&wc) } == 0
            && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
        {
            return Err("RegisterClassW failed".into());
        }
        let h = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                class.as_ptr(),
                null(),
                WS_POPUP,
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        if h.is_null() {
            return Err("CreateWindowExW failed".into());
        }
        unsafe {
            SetWindowLongPtrW(h, GWLP_USERDATA, (&mut *backdrop as *mut Backdrop) as isize);
        }
        let cover = Self {
            hwnd: h as usize,
            bounds,
            backdrop,
            thumbnail: None,
        };
        win(
            unsafe { SetLayeredWindowAttributes(h, 0, 0, LWA_ALPHA) },
            "prepare transparent cover",
        )?;
        Ok(cover)
    }
    fn h(&self) -> HWND {
        self.hwnd as HWND
    }
    fn local(&self, r: Rect) -> Rect {
        Rect {
            x: r.x - self.bounds.x,
            y: r.y - self.bounds.y,
            ..r
        }
    }
    fn live(&mut self, source: HWND, r: Rect) -> Result<(), String> {
        if self.thumbnail.is_none() {
            let mut handle = 0;
            hr(
                unsafe { DwmRegisterThumbnail(self.h(), source, &mut handle) },
                "DwmRegisterThumbnail",
            )?;
            self.thumbnail = Some(handle);
        }
        self.live_rect(r)
    }
    fn live_rect(&self, r: Rect) -> Result<(), String> {
        let handle = self.thumbnail.ok_or("missing baseline thumbnail")?;
        let mut size: SIZE = unsafe { zeroed() };
        hr(
            unsafe { DwmQueryThumbnailSourceSize(handle, &mut size) },
            "DwmQueryThumbnailSourceSize",
        )?;
        if size.cx <= 0 || size.cy <= 0 {
            return Err("DWM source size unavailable".into());
        }
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_RECTSOURCE
                | DWM_TNP_OPACITY
                | DWM_TNP_VISIBLE
                | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: native(self.local(r)),
            rcSource: RECT {
                left: 0,
                top: 0,
                right: size.cx,
                bottom: size.cy,
            },
            opacity: 255,
            fVisible: 1,
            fSourceClientAreaOnly: 0,
        };
        hr(
            unsafe { DwmUpdateThumbnailProperties(handle, &props) },
            "DwmUpdateThumbnailProperties",
        )
    }
    fn paint(&mut self) -> Result<(), String> {
        self.backdrop.paint_failed = false;
        win(
            unsafe { InvalidateRect(self.h(), null(), 0) },
            "InvalidateRect",
        )?;
        win(unsafe { UpdateWindow(self.h()) }, "UpdateWindow")?;
        if self.backdrop.paint_failed {
            Err("complete scene submission failed".into())
        } else {
            Ok(())
        }
    }
    fn show(&mut self) -> Result<(), String> {
        win(
            unsafe {
                SetWindowPos(
                    self.h(),
                    HWND_TOPMOST,
                    self.bounds.x,
                    self.bounds.y,
                    self.bounds.width,
                    self.bounds.height,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )
            },
            "show cover",
        )?;
        self.paint()?;
        win(
            unsafe { SetLayeredWindowAttributes(self.h(), 0, 255, LWA_ALPHA) },
            "opaque cover",
        )?;
        hr(unsafe { DwmFlush() }, "cover submission DwmFlush")
    }
    fn proxy(&mut self, source: &Dib, r: Rect) -> Result<(), String> {
        let local = self.local(r);
        self.backdrop.scene.render(source, local)?;
        self.paint()
    }
    fn hide(&self) {
        unsafe {
            ShowWindow(self.h(), SW_HIDE);
        }
    }
}
impl Drop for Cover {
    fn drop(&mut self) {
        self.hide();
        if let Some(handle) = self.thumbnail.take() {
            unsafe {
                DwmUnregisterThumbnail(handle);
            }
        }
        unsafe {
            SetWindowLongPtrW(self.h(), GWLP_USERDATA, 0);
            DestroyWindow(self.h());
        }
    }
}

struct Session {
    config: Config,
    identity: Identity,
    saved: Saved,
    last_drawable: Geometry,
    monitor: usize,
    work: Rect,
    cover_bounds: Rect,
    target: Rect,
    admission: Admission,
    cover: Option<Cover>,
    proxy: Option<Dib>,
    capture: Option<Capture>,
    result: Option<CaptureResult>,
    result_seen: bool,
    wake: Arc<Wake>,
    gate: CaptureGate,
    degraded: Vec<String>,
    capture_wait_us: Option<u128>,
    handoff_wait_us: Option<u128>,
    extra_prepare_us: u128,
    restored: bool,
}
impl Session {
    fn new(config: Config, admission: Admission) -> Result<Self, String> {
        let h = config.hwnd as HWND;
        ordinary(h, config.pid)?;
        // Not rcNormalPosition: measure actual, drawable, normal geometry THIS trial.
        let last_drawable = geometry(h)?;
        let original = saved(h)?;
        if original.placement.showCmd != SW_SHOWNORMAL as u32 {
            return Err(
                "initial show state must be normal/drawable; no trusted minimized geometry".into(),
            );
        }
        let target = target_outer(config.target, last_drawable.pad)?;
        pixel_bytes(
            last_drawable.outer.width,
            last_drawable.outer.height,
            RAW_BYTES,
        )?;
        let monitor = unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONULL) };
        if monitor.is_null() {
            return Err("no source monitor".into());
        }
        let work = monitor_bounds(monitor)?;
        let target_monitor = unsafe { MonitorFromRect(&native(target), MONITOR_DEFAULTTONULL) };
        let cover_bounds = last_drawable.outer.union(target)?;
        if target_monitor != monitor || !work.contains(cover_bounds) {
            return Err("old/target/path not fully inside same monitor work area".into());
        }
        pixel_bytes(cover_bounds.width, cover_bounds.height, SCENE_BYTES)?;
        let wake = Wake::new()?;
        let property = wide(&format!(
            "e-desktop-handoff-{}-{}",
            std::process::id(),
            qpc()
        ));
        if !unsafe { GetPropW(h, property.as_ptr()) }.is_null() {
            return Err("identity property collision".into());
        }
        let cookie = (qpc() as usize ^ std::process::id() as usize).max(1);
        let identity = Identity {
            hwnd: config.hwnd,
            pid: config.pid,
            title: wide(&format!("e-desktop Handoff {} {}", config.tag, config.role)),
            property,
            cookie,
            process: admission.process.clone(),
        };
        // All rejectable geometry/resource preconditions precede this first mutation.
        win(
            unsafe { SetPropW(h, identity.property.as_ptr(), cookie as HANDLE) },
            "identity SetPropW",
        )?;
        let mut session = Self {
            config,
            identity,
            saved: original,
            last_drawable,
            monitor: monitor as usize,
            work,
            cover_bounds,
            target,
            admission,
            cover: None,
            proxy: None,
            capture: None,
            result: None,
            result_seen: false,
            wake,
            gate: CaptureGate::new(),
            degraded: vec![],
            capture_wait_us: None,
            handoff_wait_us: None,
            extra_prepare_us: 0,
            restored: false,
        };
        if !session.identity.alive() {
            let _ = session.restore();
            return Err("identity changed while acquiring ownership".into());
        }
        Ok(session)
    }
    fn emit(&self, log: &mut Log, event: &str, value: Value) -> Result<(), String> {
        log.event(event, self.gate.generation, value)
    }
    fn cancelled(&self) -> bool {
        CANCELLED.load(Ordering::Acquire)
            || self.config.cancel_file.as_ref().is_some_and(|p| p.exists())
    }
    fn guard(&self) -> Result<(), String> {
        if self.cancelled() {
            return Err("cancelled".into());
        }
        if !self.identity.alive() {
            return Err("source lifetime/PID/title changed".into());
        }
        let h = self.identity.h();
        ordinary(h, self.config.pid)?;
        let layered = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32 & WS_EX_LAYERED != 0;
        let image = Path::new(&self.admission.image)
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("handoff-fixture.exe"));
        affinity_admission(
            affinity(h),
            layered,
            self.config.fixture_only_allow_unknown_affinity,
            image,
        )?;
        if monitor_bounds(self.monitor as HMONITOR)? != self.work
            || unsafe { GetDpiForWindow(h) } != self.last_drawable.dpi
            || unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONULL) } as usize != self.monitor
        {
            return Err("monitor/workarea/DPI changed".into());
        }
        if unsafe { IsIconic(h) } == 0 && !self.cover_bounds.contains(outer(h)?) {
            return Err("drawable source escaped safe cover".into());
        }
        if let Some(cover) = self
            .cover
            .as_ref()
            .filter(|c| unsafe { IsWindowVisible(c.h()) } != 0)
        {
            if outer(cover.h())? != self.cover_bounds
                || unsafe { GetDpiForWindow(cover.h()) } != self.last_drawable.dpi
            {
                return Err("cover bounds/DPI changed".into());
            }
            // Reject an intervening visible top-level window above the cover in its area.
            let mut above = unsafe { GetWindow(cover.h(), GW_HWNDPREV) };
            while !above.is_null() {
                if unsafe { IsWindowVisible(above) } != 0 && unsafe { IsIconic(above) } == 0 {
                    let r = outer(above)?;
                    if r.x < self.cover_bounds.right()
                        && r.right() > self.cover_bounds.x
                        && r.y < self.cover_bounds.bottom()
                        && r.bottom() > self.cover_bounds.y
                    {
                        return Err("another visible window is above safe cover".into());
                    }
                }
                above = unsafe { GetWindow(above, GW_HWNDPREV) };
            }
        }
        Ok(())
    }
    fn poll_result(&mut self) {
        if self.result_seen {
            return;
        }
        if let Some(capture) = &self.capture
            && let Ok(result) = capture.receiver.try_recv()
        {
            self.result = Some(result);
            self.result_seen = true;
        }
    }
    fn wait_step(&mut self, deadline: Instant) -> Result<(), String> {
        let mut message: MSG = unsafe { zeroed() };
        for _ in 0..32 {
            if unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } == 0 {
                break;
            }
            if message.message == WM_QUIT {
                CANCELLED.store(true, Ordering::Release);
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            if Instant::now() >= deadline || self.cancelled() {
                break;
            }
        }
        self.guard()?;
        self.poll_result();
        self.wake.reset()?;
        self.poll_result(); // Reset-and-recheck closes the enqueue/reset lost-wake race.
        if self.result.is_some() || Instant::now() >= deadline {
            return Ok(());
        }
        let next = deadline.min(Instant::now() + Duration::from_millis(16));
        let remaining = next.saturating_duration_since(Instant::now());
        let ms = (remaining.as_millis()
            + u128::from(!remaining.subsec_nanos().is_multiple_of(1_000_000)))
            as u32;
        let handle = self.wake.handle();
        let result = unsafe {
            MsgWaitForMultipleObjectsEx(1, &handle, ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        if result == WAIT_FAILED {
            return Err("MsgWaitForMultipleObjectsEx failed".into());
        }
        Ok(())
    }
    fn reject_late(&mut self, log: &mut Log) -> Result<(), String> {
        if let Some(result) = self.result.take() {
            self.emit(log, "capture_rejected", json!({"captureId": 1, "captureGeneration": result.generation,
                "reason": "late result cannot mutate placement/pinned frame", "nativeSucceeded": result.result.is_ok()}))?;
        }
        Ok(())
    }
    fn hold(&mut self, ms: u64, log: &mut Log, phase: &str) -> Result<(), String> {
        self.emit(
            log,
            "observation_hold",
            json!({"phase": phase, "milliseconds": ms, "semanticReadyCondition": false}),
        )?;
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            self.wait_step(deadline)?;
            self.reject_late(log)?;
        }
        self.guard()
    }
    fn degrade(&mut self, log: &mut Log, reason: String) -> Result<(), String> {
        self.degraded.push(reason.clone());
        self.emit(
            log,
            "degraded",
            json!({"reason": reason, "visualPass": null}),
        )
    }

    fn cancel_capture(&mut self) {
        if !self.gate.cancelled {
            self.gate.cancel();
        }
        if let Some(capture) = &self.capture {
            capture.cancelled.store(true, Ordering::Release);
        }
    }
    fn acquire(
        &mut self,
        expected: Geometry,
        deadline: Instant,
        log: &mut Log,
    ) -> Result<bool, String> {
        self.guard()?;
        let started = Instant::now();
        let generation = self.gate.request()?;
        self.emit(
            log,
            "capture_requested",
            json!({"captureId": 1, "sourceGeometry": expected,
            "budgetRemainingUs": deadline.saturating_duration_since(started).as_micros()}),
        )?;
        self.capture = Some(Capture::start(
            self.identity.clone(),
            expected.clone(),
            generation,
            self.wake.clone(),
        )?);
        loop {
            self.poll_result();
            if let Some(result) = self.result.take() {
                self.capture_wait_us = Some(started.elapsed().as_micros());
                self.emit(
                    log,
                    "capture_completed",
                    json!({"captureId": 1, "captureGeneration": result.generation,
                    "waitUs": self.capture_wait_us, "nativeSucceeded": result.result.is_ok()}),
                )?;
                self.guard()?;
                let same_geometry = geometry(self.identity.h()).is_ok_and(|g| g == expected);
                if !self.gate.accepts(
                    result.generation,
                    Instant::now() < deadline,
                    self.identity.alive(),
                    same_geometry,
                ) {
                    self.cancel_capture();
                    self.emit(
                        log,
                        "capture_rejected",
                        json!({"reason": "deadline/identity/generation/geometry invalid"}),
                    )?;
                    self.degrade(log, "capture result rejected".into())?;
                    return Ok(false);
                }
                match result.result {
                    Ok(frame) => {
                        self.pin(frame, log)?;
                        if Instant::now() >= deadline {
                            self.proxy = None;
                            self.cancel_capture();
                            self.degrade(
                                log,
                                "frame preparation exceeded consumer deadline".into(),
                            )?;
                            return Ok(false);
                        }
                        return Ok(true);
                    }
                    Err(error) => {
                        self.cancel_capture();
                        self.emit(log, "capture_rejected", json!({"reason": error}))?;
                        self.degrade(log, error)?;
                        return Ok(false);
                    }
                }
            }
            if Instant::now() >= deadline {
                self.capture_wait_us = Some(started.elapsed().as_micros());
                self.cancel_capture();
                self.degrade(
                    log,
                    "capture consumer deadline; native call may still be running".into(),
                )?;
                return Ok(false);
            }
            self.wait_step(deadline)?;
        }
    }
    fn pin(&mut self, frame: Frame, log: &mut Log) -> Result<(), String> {
        if frame.generation != self.gate.generation || frame.geometry.dpi != self.last_drawable.dpi
        {
            return Err("invalid pinned frame metadata".into());
        }
        pixel_bytes(frame.w, frame.h, FRAME_BYTES)?;
        let metadata = json!({"schemaVersion": 1, "trialId": log.trial_id, "captureId": 1,
            "intentGeneration": frame.generation, "pid": self.identity.pid, "hwnd": hwnd_string(self.identity.h()),
            "lifetimeCookie": format!("0x{:x}", self.identity.cookie),
            "sourceGeometry": frame.geometry, "pixelSize": {"width": frame.w, "height": frame.h},
            "strideBytes": frame.w * 4, "format": "BGRA8-top-down-opaque", "captureQpc": frame.capture_qpc,
            "completedQpc": frame.completed_qpc, "qpcFrequency": log.qpc_frequency,
            "semanticReady": null, "pixelsFile": "capture-1.bgra",
            "sampling": if frame.w == frame.geometry.outer.width && frame.h == frame.geometry.outer.height {
                "identity"
            } else { "GDI-COLORONCOLOR-reduction-composed-proxy-unknown" }});
        fs::write(self.config.output.join("capture-1.bgra"), &frame.pixels)
            .map_err(|e| format!("frame output: {e}"))?;
        fs::write(
            self.config.output.join("capture-1.json"),
            serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let mut source = Dib::new(frame.w, frame.h, FRAME_BYTES)?;
        source.pixels().copy_from_slice(&frame.pixels);
        self.proxy = Some(source);
        self.emit(log, "proxy_pinned", metadata)
    }
    fn minimize(&mut self, log: &mut Log) -> Result<(), String> {
        self.guard()?;
        if geometry(self.identity.h())? != self.last_drawable {
            return Err("last drawable geometry changed before minimize".into());
        }
        win(
            unsafe { ShowWindowAsync(self.identity.h(), SW_SHOWMINNOACTIVE) },
            "minimize async",
        )?;
        let deadline = Instant::now() + Duration::from_millis(self.config.handoff_ms);
        while unsafe { IsIconic(self.identity.h()) } == 0 {
            if Instant::now() >= deadline {
                return Err("minimize geometry deadline".into());
            }
            self.wait_step(deadline)?;
            self.reject_late(log)?;
        }
        self.emit(
            log,
            "minimized",
            json!({"lastDrawableGeometry": self.last_drawable}),
        )
    }
    fn place(&mut self, target: Rect, deadline: Instant) -> Result<(), String> {
        self.guard()?;
        let cover = self.cover.as_ref().ok_or("no cover")?;
        if unsafe { IsWindowVisible(cover.h()) } == 0 {
            return Err("placement requires presented cover".into());
        }
        if unsafe { IsIconic(self.identity.h()) } != 0 {
            let mut placement = self.saved.placement;
            placement.showCmd = SW_SHOWNOACTIVATE as u32;
            placement.flags =
                (placement.flags & !WPF_RESTORETOMAXIMIZED) | WPF_ASYNCWINDOWPLACEMENT;
            let mut info: MONITORINFO = unsafe { zeroed() };
            info.cbSize = size_of::<MONITORINFO>() as u32;
            win(
                unsafe { GetMonitorInfoW(self.monitor as HMONITOR, &mut info) },
                "restore monitor",
            )?;
            placement.rcNormalPosition = native(workspace_rect(
                target,
                rect(info.rcWork)?,
                rect(info.rcMonitor)?,
            )?);
            win(
                unsafe { SetWindowPlacement(self.identity.h(), &placement) },
                "restore directly at requested geometry async",
            )?;
            while unsafe { IsIconic(self.identity.h()) } != 0 {
                if Instant::now() >= deadline {
                    return Err("restore geometric deadline".into());
                }
                self.wait_step(deadline)?;
            }
        }
        self.guard()?;
        win(
            unsafe {
                SetWindowPos(
                    self.identity.h(),
                    null_mut(),
                    target.x,
                    target.y,
                    target.width,
                    target.height,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_ASYNCWINDOWPOS,
                )
            },
            "target async SetWindowPos",
        )?;
        while outer(self.identity.h())? != target || unsafe { IsIconic(self.identity.h()) } != 0 {
            if Instant::now() >= deadline {
                return Err("target refused/geometric deadline".into());
            }
            self.wait_step(deadline)?;
        }
        self.guard()
    }
    fn execute(&mut self, log: &mut Log) -> Result<(), String> {
        self.emit(log, "original_saved", json!({"outer": self.saved.outer, "visible": self.saved.visible,
            "showCmd": self.saved.placement.showCmd, "placementFlags": self.saved.placement.flags,
            "normalPositionWorkAreaCoordinates": rect(self.saved.placement.rcNormalPosition)?,
            "region": null, "lastDrawableGeometry": self.last_drawable,
            "lifetimeCookie": format!("0x{:x}", self.identity.cookie),
            "targetOuter": self.target, "targetVisible": self.config.target,
            "coverBounds": self.cover_bounds, "workArea": self.work,
            "sourceImagePath": self.admission.image, "admission": self.admission.name,
            "protectionMetadata": if self.admission.affinity.is_ok() {"public-api-wda-none"} else {"unavailable"},
            "affinityQuery": self.admission.affinity, "geometryIsNotSemanticReady": true}))?;
        self.guard()?;
        let mut cover = Cover::new(self.cover_bounds)?;
        if self.config.mode == Mode::Baseline {
            cover.live(self.identity.h(), self.last_drawable.outer)?;
        }
        self.cover = Some(cover);
        if self.config.mode == Mode::Prehide {
            let preparation = Instant::now();
            let deadline = preparation + Duration::from_millis(self.config.prehide_ms);
            self.acquire(self.last_drawable.clone(), deadline, log)?;
            self.extra_prepare_us += preparation.elapsed().as_micros();
        }
        self.minimize(log)?;
        self.hold(self.config.minimized_hold_ms, log, "minimized-observation")?;
        if let Some(proxy) = &self.proxy {
            let cover = self.cover.as_mut().unwrap();
            cover.proxy(proxy, self.last_drawable.outer)?;
        }
        self.cover.as_mut().unwrap().show()?;
        self.guard()?;
        self.emit(log, "cover_presented", json!({"opaque": true, "completeOffscreenSubmission": true,
            "dwmFlushMeansOnlyCallerSubmissionBoundary": true, "inputGate": "nonactivating-cover-mouse-only"}))?;
        if self.config.mode == Mode::Staged {
            let preparation = Instant::now();
            let deadline = preparation + Duration::from_millis(self.config.staged_ms);
            match self.place(self.last_drawable.outer, deadline) {
                Ok(()) => {
                    let restored = geometry(self.identity.h())?;
                    if restored != self.last_drawable {
                        return Err("old restored geometry/DPI/pad/client mismatch".into());
                    }
                    self.emit(
                        log,
                        "staged_restored",
                        json!({"sourceGeometry": restored, "usesUndoRectangle": false}),
                    )?;
                    if Instant::now() < deadline {
                        self.acquire(restored, deadline, log)?;
                    } else {
                        self.degrade(
                            log,
                            "staged restore exhausted capture consumer budget".into(),
                        )?;
                    }
                }
                Err(error) if self.cancelled() || !self.identity.alive() => return Err(error),
                Err(error) => {
                    self.degrade(log, error)?;
                }
            }
            self.extra_prepare_us += preparation.elapsed().as_micros();
            if let Some(proxy) = &self.proxy {
                self.cover
                    .as_mut()
                    .unwrap()
                    .proxy(proxy, self.last_drawable.outer)?;
                self.emit(
                    log,
                    "proxy_presented",
                    json!({"captureId": 1, "fixed": true, "sourceGeometry": self.last_drawable}),
                )?;
                hr(unsafe { DwmFlush() }, "fixed proxy submission")?;
            }
        } else if self.proxy.is_some() {
            self.emit(
                log,
                "proxy_presented",
                json!({"captureId": 1, "fixed": true, "sourceGeometry": self.last_drawable}),
            )?;
        }
        if self.config.mode != Mode::Baseline && self.proxy.is_none() && self.degraded.is_empty() {
            self.degrade(log, "no fixed frame".into())?;
        }
        // Missing frames use explicitly labelled live fallback, never fixture pixels.
        if self.proxy.is_none()
            && self.config.mode != Mode::Baseline
            && unsafe { IsIconic(self.identity.h()) } == 0
        {
            self.cover
                .as_mut()
                .unwrap()
                .live(self.identity.h(), self.last_drawable.outer)?;
        }
        let target_start = Instant::now();
        let handoff_deadline = target_start + Duration::from_millis(self.config.handoff_ms);
        self.emit(
            log,
            "target_apply_requested",
            json!({"outer": self.target, "visible": self.config.target,
            "handoffBudgetMs": self.config.handoff_ms, "semanticReady": null}),
        )?;
        self.place(self.target, handoff_deadline)?;
        self.emit(
            log,
            "target_placed",
            json!({"sourceGeometry": geometry(self.identity.h())?,
            "geometryOnly": true, "applyWaitUs": target_start.elapsed().as_micros()}),
        )?;
        if self.proxy.is_none() {
            self.cover
                .as_mut()
                .unwrap()
                .live(self.identity.h(), self.last_drawable.outer)?;
        }
        let animation_start = Instant::now();
        let animation_end = animation_start + Duration::from_millis(self.config.animation_ms);
        loop {
            self.guard()?;
            let now = Instant::now();
            let t = if self.config.animation_ms == 0 {
                1.0
            } else {
                (now.duration_since(animation_start).as_secs_f64()
                    / (self.config.animation_ms as f64 / 1000.0))
                    .min(1.0)
            };
            let pose = self.last_drawable.outer.interpolate(self.target, t);
            let cover = self.cover.as_mut().unwrap();
            if let Some(proxy) = &self.proxy {
                cover.proxy(proxy, pose)?;
            } else {
                cover.live_rect(pose)?;
            }
            self.emit(
                log,
                "presentation_submitted",
                json!({
                    "outerPose": pose, "submittedQpc": qpc(),
                    "captureId": self.proxy.as_ref().map(|_| 1),
                    "fixedSourceRecord": self.proxy.as_ref().map(|_| "capture-1.json"),
                    "pixelSize": self.proxy.as_ref().map(|p| json!({"width": p.w, "height": p.h})),
                    "timingMeaning": "API submission returned; NOT DWM display confirmation",
                    "displayConfirmed": false
                }),
            )?;
            if now >= handoff_deadline && t < 1.0 {
                self.degrade(log, "absolute handoff deadline during animation".into())?;
                break;
            }
            if t >= 1.0 {
                break;
            }
            let tick = (now + Duration::from_millis(16))
                .min(animation_end)
                .min(handoff_deadline);
            while Instant::now() < tick {
                self.wait_step(tick)?;
                self.reject_late(log)?;
            }
        }
        self.guard()?;
        if outer(self.identity.h())? != self.target {
            return Err("source moved before handoff".into());
        }
        self.cancel_capture(); // Invalidate presentation before releasing cover and input gate.
        self.reject_late(log)?;
        self.cover.as_ref().unwrap().hide();
        self.handoff_wait_us = Some(target_start.elapsed().as_micros());
        if Instant::now() > handoff_deadline {
            self.degrade(
                log,
                "native/presentation work exceeded absolute handoff consumer budget".into(),
            )?;
        }
        self.emit(
            log,
            "handoff",
            json!({"handoffWaitUs": self.handoff_wait_us,
            "extraPrepareUs": self.extra_prepare_us, "mouseInputBlockRemoved": true,
            "reason": "bounded-animation-end-or-deadline", "semanticReady": null,
            "normalPresentationCandidate": self.degraded.is_empty(), "visualPass": null}),
        )?;
        self.hold(self.config.observe_ms, log, "uncovered-target-observation")
    }
    fn restore_tick(&mut self, deadline: Instant) -> Result<(), String> {
        let mut message: MSG = unsafe { zeroed() };
        for _ in 0..32 {
            if unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } == 0 {
                break;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        // Cleanup ignores cancellation, but NEVER ignores source identity.
        self.poll_result();
        // Retain late results only for logging after cleanup, never for an action.
        let ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(16) as u32;
        if unsafe { MsgWaitForMultipleObjectsEx(0, null(), ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE) }
            == WAIT_FAILED
        {
            return Err("restoration message wait failed".into());
        }
        Ok(())
    }
    fn restore(&mut self) -> Result<(), String> {
        self.cancel_capture();
        if let Some(cover) = &self.cover {
            cover.hide();
        }
        if self.restored {
            return Ok(());
        }
        if !self.identity.alive() {
            return Err("source destroyed/reused: original restoration refused".into());
        }
        let h = self.identity.h();
        let mut placement = self.saved.placement;
        // Equivalent native normal state, without requesting foreground activation.
        placement.showCmd = SW_SHOWNOACTIVATE as u32;
        placement.flags |= WPF_ASYNCWINDOWPLACEMENT;
        let deadline = Instant::now() + Duration::from_millis(self.config.handoff_ms);
        let mut failures = Vec::new();
        if let Err(e) = win(
            unsafe { SetWindowPlacement(h, &placement) },
            "restore placement",
        ) {
            failures.push(e);
        }
        while self.identity.alive() && unsafe { IsIconic(h) } != 0 && Instant::now() < deadline {
            if let Err(error) = self.restore_tick(deadline) {
                failures.push(error);
                break;
            }
        }
        // WinForms may replace rcNormalPosition during its first restored transition.
        if self.identity.alive() {
            if let Err(e) = win(
                unsafe { SetWindowPlacement(h, &placement) },
                "restore placement second pass",
            ) {
                failures.push(e);
            }
            if let Err(e) = win(
                unsafe { SetWindowRgn(h, null_mut(), 1) },
                "restore original null region",
            ) {
                failures.push(e);
            }
            if !self.saved.visible {
                unsafe {
                    ShowWindow(h, SW_HIDE);
                }
            }
            while self.identity.alive()
                && outer(h).ok() != Some(self.saved.outer)
                && Instant::now() < deadline
            {
                if let Err(error) = self.restore_tick(deadline) {
                    failures.push(error);
                    break;
                }
            }
            if outer(h).ok() != Some(self.saved.outer)
                || unsafe { IsIconic(h) } != 0
                || unsafe { IsZoomed(h) } != 0
                || (unsafe { IsWindowVisible(h) } != 0) != self.saved.visible
            {
                failures.push("original outer/show/visibility verification failed".into());
            }
            unsafe {
                RemovePropW(h, self.identity.property.as_ptr());
            }
        } else {
            failures.push("identity changed during original restore".into());
        }
        self.restored = true; // Parent owns fixture recovery if restoration fails.
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.restored {
            let _ = self.restore();
        } // Unwind only; normal path logs outcome.
    }
}

pub fn run(config: Config) -> Result<(), String> {
    let _dpi = DpiScope::enter()?;
    CANCELLED.store(false, Ordering::Release);
    if config.cancel_file.as_ref().is_some_and(|p| p.exists()) {
        return Err("cancel file already exists; no mutation".into());
    }
    let mut log = Log::new(config.clone())?;
    log.event(
        "start",
        1,
        json!({"parameters": config, "visualPass": null,
        "markerOrFixtureStateUsedForReadiness": false,
        "budgets": {"rawDibBytes": RAW_BYTES, "frameBytes": FRAME_BYTES, "sceneBytes": SCENE_BYTES},
        "clockCalibration": {"qpcStart": log.qpc_start, "qpcFrequency": log.qpc_frequency,
            "unixStartUs": log.unix_start_us, "units": "qpc-ticks-and-microseconds"}}),
    )?;
    let preparation = (|| {
        let admission = preflight(&config)?;
        log.event("admission", 1, json!({"sourceImagePath": admission.image, "admission": admission.name,
            "protectionMetadata": if admission.affinity.is_ok() {"public-api-wda-none"} else {"unavailable"},
            "affinityQuery": admission.affinity}))?;
        Session::new(config.clone(), admission)
    })();
    let mut session = match preparation {
        Ok(session) => session,
        Err(error) => {
            log.event(
                "degraded",
                1,
                json!({"reason": error, "admitted": false, "visualPass": null}),
            )?;
            log.event(
                "finished",
                1,
                json!({"status": "refused", "targetModified": false}),
            )?;
            write_trial(
                &log,
                json!({"status": "refused", "error": error, "restoreRequired": false, "visualPass": null}),
            )?;
            return Err(error);
        }
    };
    let handler = win(
        unsafe { SetConsoleCtrlHandler(Some(console_cancel), 1) },
        "SetConsoleCtrlHandler",
    );
    let outcome = handler.and_then(|_| session.execute(&mut log));
    let cancelled = session.cancelled();
    session.cancel_capture();
    let restored = session.restore(); // Even logging/rendering/native failure cannot skip cleanup.
    session.poll_result();
    let pending_native = session.capture.is_some() && !session.result_seen;
    session.reject_late(&mut log)?;
    unsafe {
        SetConsoleCtrlHandler(Some(console_cancel), 0);
    }
    let restore_ok = restored.is_ok();
    let restore_error = restored.as_ref().err().cloned();
    let status = if cancelled {
        "cancelled"
    } else if outcome.is_err() || !restore_ok {
        "failed"
    } else if !session.degraded.is_empty() {
        "degraded"
    } else {
        "completed-unverified"
    };
    if let Err(error) = &outcome {
        log.event(
            if cancelled { "cancelled" } else { "degraded" },
            session.gate.generation,
            json!({"reason": error, "visualPass": null}),
        )?;
    }
    log.event(
        "original_restored",
        session.gate.generation,
        json!({"success": restore_ok,
        "error": restore_error, "originalOuter": session.saved.outer,
        "mouseInputBlockRemoved": true, "lateNativeCallMayStillBeRunning": pending_native}),
    )?;
    log.event(
        "finished",
        session.gate.generation,
        json!({"status": status, "visualPass": null}),
    )?;
    write_trial(
        &log,
        json!({"status": status, "visualPass": null, "semanticReady": null,
        "captureWaitUs": session.capture_wait_us, "handoffWaitUs": session.handoff_wait_us,
        "extraPrepareUs": session.extra_prepare_us, "degradedReasons": session.degraded,
        "error": outcome.as_ref().err(), "restorationSucceeded": restore_ok, "restorationError": restore_error,
        "sourceImagePath": session.admission.image, "admission": session.admission.name,
        "protectionMetadata": if session.admission.affinity.is_ok() {"public-api-wda-none"} else {"unavailable"},
        "affinityQuery": session.admission.affinity, "nativeCapturePendingAtExit": pending_native,
        "domFocus": "not observed by native probe; parent must measure separately"}),
    )?;
    restored?;
    outcome?;
    if cancelled || !session.degraded.is_empty() {
        return Err(format!("trial {status}; see trial.json"));
    }
    println!(
        "Trial completed and original restored; VISUAL EFFECTS UNVERIFIED. {}",
        config.output.display()
    );
    Ok(())
}
fn write_trial(log: &Log, result: Value) -> Result<(), String> {
    let value = json!({"schemaVersion": 1, "trialId": log.trial_id, "mode": log.config.mode,
        "pid": log.config.pid, "hwnd": format!("0x{:x}", log.config.hwnd), "parameters": log.config,
        "durationUs": log.start.elapsed().as_micros(),
        "transitionPolicy": "unchanged-by-probe", "transitionOriginalQueried": false,
        "sampling": {"captureReduction": "per-capture-metadata",
            "frozenProxy": "GDI-COLORONCOLOR", "composition": "single-proxy-only-if-capture-identity;otherwise-unknown",
            "liveBaseline": "DWM-managed-unknown",
            "sceneCommit": "BitBlt-no-resampling"}, "clockCalibration": {
            "qpcStart": log.qpc_start, "qpcFrequency": log.qpc_frequency, "unixStartUs": log.unix_start_us},
        "result": result});
    fs::write(
        log.config.output.join("trial.json"),
        serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

/// Memory DC only: complete repaint and stretch without ANY window or screen DC.
pub fn memory_check() -> Result<(), String> {
    let mut source = Dib::new(2, 2, FRAME_BYTES)?;
    source.pixels().copy_from_slice(&[
        0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255,
    ]);
    let mut scene = Dib::new(6, 6, SCENE_BYTES)?;
    scene.render(&source, Rect::new(1, 1, 4, 4)?)?;
    let mut committed = Dib::new(6, 6, SCENE_BYTES)?;
    scene.blit(committed.dc)?;
    win(unsafe { GdiFlush() }, "memory check flush")?;
    let at =
        |pixels: &[u8], x: usize, y: usize| pixels[(y * 6 + x) * 4..(y * 6 + x) * 4 + 3].to_vec();
    let pixels = committed.pixels();
    if at(pixels, 0, 0) != [38, 38, 38]
        || at(pixels, 1, 1) != [0, 0, 255]
        || at(pixels, 4, 4) != [255, 255, 255]
    {
        return Err("memory scene projection/commit failed".into());
    }
    scene.render(&source, Rect::new(3, 3, 2, 2)?)?;
    scene.blit(committed.dc)?;
    win(unsafe { GdiFlush() }, "memory check second flush")?;
    let pixels = committed.pixels();
    if at(pixels, 1, 1) != [38, 38, 38] || at(pixels, 3, 3) != [0, 0, 255] {
        return Err("memory scene retained pixels from preceding animation frame".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn complete_offscreen_scene_and_commit_memory_dc_only() {
        super::memory_check().unwrap();
    }
    #[test]
    fn coloroncolor_pixel_centers_with_padding_and_fractional_ratios() {
        use super::*;
        let mut source = Dib::new(7, 5, FRAME_BYTES).unwrap();
        for y in 0..5usize {
            for x in 0..7usize {
                source.pixels()[(y * 7 + x) * 4..(y * 7 + x) * 4 + 4]
                    .copy_from_slice(&[x as u8, y as u8, 0, 255]);
            }
        }
        for (dw, dh) in [(7, 5), (11, 8), (5, 3), (9, 7)] {
            let mut scene = Dib::new(dw + 5, dh + 6, SCENE_BYTES).unwrap();
            scene
                .render(&source, Rect::new(2, 3, dw, dh).unwrap())
                .unwrap();
            let pixels = scene.pixels();
            assert_eq!(&pixels[..3], &[38, 38, 38]);
            for y in 0..dh {
                for x in 0..dw {
                    let offset = (((y + 3) * (dw + 5) + x + 2) * 4) as usize;
                    let expected = [
                        ((2 * x + 1) * 7 / (2 * dw)) as u8,
                        ((2 * y + 1) * 5 / (2 * dh)) as u8,
                        0,
                    ];
                    assert_eq!(
                        &pixels[offset..offset + 3],
                        &expected,
                        "{dw}x{dh} at {x},{y}"
                    );
                }
            }
        }
    }
}
