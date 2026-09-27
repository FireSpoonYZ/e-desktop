//! Win32 backend. Poll on the owning thread; no hooks or desktop mutation at construction.
pub mod hook;
pub mod preview;
pub mod splitter;
use crate::model::*;
use std::{
    collections::{HashMap, HashSet},
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    System::Threading::*,
    UI::{HiDpi::*, WindowsAndMessaging::*},
};

struct DpiScope(DPI_AWARENESS_CONTEXT);
impl DpiScope {
    fn enter() -> Result<Self, AppError> {
        let previous =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if previous.is_null() {
            Err(failed("SetThreadDpiAwarenessContext", None))
        } else {
            Ok(Self(previous))
        }
    }
}
impl Drop for DpiScope {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}
// GDI region handles are process-wide; Backend's &mut methods serialize ownership.
struct Region(usize);
impl Region {
    fn handle(&self) -> HRGN {
        self.0 as HRGN
    }
}
impl Drop for Region {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.handle());
        }
    }
}
struct Saved {
    placement: WINDOWPLACEMENT,
    region: Option<Region>,
    clipped: bool,
}
struct Entry {
    hwnd: usize,
    pid: u32,
    cookie: usize,
    saved: Option<Saved>,
    minimized: bool,
    /// Last border color (`None` = not forced) and corner preference we applied.
    decor: Option<(Option<u32>, i32)>,
    /// Invisible resize border (left, top, right, bottom) per window DPI, measured while
    /// unclipped; it differs between monitors with different scaling.
    pads: Vec<(u32, [i32; 4])>,
    /// Border padding the last placement used; a managed window reads back with it.
    placed_pad: Option<[i32; 4]>,
    /// Our clip region's box in window coordinates; None while the original region applies.
    region_box: Option<RECT>,
}
pub struct Backend {
    previews: preview::Previews,
    entries: HashMap<String, Entry>,
    property: Vec<u16>,
    next: usize,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn error(code: ErrorCode, message: impl Into<String>, id: Option<&str>) -> AppError {
    AppError {
        code,
        message: message.into(),
        window_id: id.map(str::to_owned),
    }
}
fn failed(op: &str, id: Option<&str>) -> AppError {
    error(
        ErrorCode::OperationDenied,
        format!("{op} failed (Win32 {}).", unsafe { GetLastError() }),
        id,
    )
}
fn rect(r: RECT) -> Rect {
    Rect {
        x: r.left,
        y: r.top,
        width: (i64::from(r.right) - i64::from(r.left)).max(0) as u32,
        height: (i64::from(r.bottom) - i64::from(r.top)).max(0) as u32,
    }
}
fn native(r: Rect) -> Result<RECT, AppError> {
    let right = i64::from(r.x) + i64::from(r.width);
    let bottom = i64::from(r.y) + i64::from(r.height);
    if r.width == 0
        || r.height == 0
        || r.width > i32::MAX as u32
        || r.height > i32::MAX as u32
        || right > i64::from(i32::MAX)
        || bottom > i64::from(i32::MAX)
    {
        return Err(error(
            ErrorCode::InvalidCommand,
            "Invalid physical rectangle",
            None,
        ));
    }
    Ok(RECT {
        left: r.x,
        top: r.y,
        right: right as i32,
        bottom: bottom as i32,
    })
}
fn intersect(a: RECT, b: RECT) -> Option<RECT> {
    let r = RECT {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    };
    (r.left < r.right && r.top < r.bottom).then_some(r)
}
fn same_rect(a: RECT, b: RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}
/// Invisible resize border: how far `outer` (GetWindowRect) extends past the visible frame.
fn frame_pad(outer: RECT, visible: RECT) -> [i32; 4] {
    [
        visible.left.saturating_sub(outer.left),
        visible.top.saturating_sub(outer.top),
        outer.right.saturating_sub(visible.right),
        outer.bottom.saturating_sub(visible.bottom),
    ]
}
fn outset(visible: RECT, pad: [i32; 4]) -> RECT {
    RECT {
        left: visible.left.saturating_sub(pad[0]),
        top: visible.top.saturating_sub(pad[1]),
        right: visible.right.saturating_add(pad[2]),
        bottom: visible.bottom.saturating_add(pad[3]),
    }
}
fn inset(outer: RECT, pad: [i32; 4]) -> RECT {
    outset(outer, pad.map(|p| -p))
}
fn outer_frame(h: HWND) -> Option<RECT> {
    let mut r: RECT = unsafe { zeroed() };
    (unsafe { GetWindowRect(h, &mut r) } != 0).then_some(r)
}
/// DWM visible frame. Unreliable while a window region is set (DWM then reports the outer
/// rectangle, and keeps doing so briefly after the region is removed).
fn dwm_frame(h: HWND) -> Option<RECT> {
    let mut visible: RECT = unsafe { zeroed() };
    let hr = unsafe {
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            &mut visible as *mut _ as _,
            size_of::<RECT>() as u32,
        )
    };
    (hr >= 0).then_some(visible)
}
/// Minimize/restore animations would play every time a column scrolls off or back on screen.
fn set_transitions(hwnd: HWND, disabled: bool) {
    let value = i32::from(disabled);
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_TRANSITIONS_FORCEDISABLED as u32,
            &value as *const _ as _,
            size_of::<i32>() as u32,
        );
    }
}
/// WINDOWPLACEMENT uses workspace coordinates: screen minus the monitor's work-area offset.
fn workspace(r: RECT) -> RECT {
    let mut info: MONITORINFO = unsafe { zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    let monitor = unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST) };
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return r;
    }
    let dx = info.rcWork.left - info.rcMonitor.left;
    let dy = info.rcWork.top - info.rcMonitor.top;
    RECT {
        left: r.left - dx,
        top: r.top - dy,
        right: r.right - dx,
        bottom: r.bottom - dy,
    }
}
fn reset_decor(hwnd: HWND) {
    let color = DWMWA_COLOR_DEFAULT;
    let corner = DWMWCP_DEFAULT;
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR as u32,
            &color as *const _ as _,
            size_of::<u32>() as u32,
        );
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            &corner as *const _ as _,
            size_of::<i32>() as u32,
        );
    }
}
fn local_clip(clip: RECT, window: RECT) -> Result<RECT, AppError> {
    let offset = |value: i32, origin: i32| {
        i32::try_from(i64::from(value) - i64::from(origin)).map_err(|_| {
            error(
                ErrorCode::OperationDenied,
                "Window region exceeds Win32 coordinate range",
                None,
            )
        })
    };
    Ok(RECT {
        left: offset(clip.left, window.left)?,
        top: offset(clip.top, window.top)?,
        right: offset(clip.right, window.left)?,
        bottom: offset(clip.bottom, window.top)?,
    })
}
fn monitor_id(h: HMONITOR) -> String {
    format!("win-monitor-{:x}", h as usize)
}
fn fresh_region() -> Result<Region, AppError> {
    let r = unsafe { CreateRectRgn(0, 0, 0, 0) };
    if r.is_null() {
        Err(failed("CreateRectRgn", None))
    } else {
        Ok(Region(r as usize))
    }
}
fn copy_region(source: &Region) -> Result<Region, AppError> {
    let r = fresh_region()?;
    if unsafe { CombineRgn(r.handle(), source.handle(), null_mut(), RGN_COPY) } == ERROR {
        return Err(failed("CombineRgn", None));
    }
    Ok(r)
}
fn set_region(hwnd: HWND, region: Option<Region>) -> Result<(), AppError> {
    let handle = region.as_ref().map_or(null_mut(), Region::handle);
    if unsafe { SetWindowRgn(hwnd, handle, 1) } == 0 {
        return Err(failed("SetWindowRgn", None));
    }
    // On success ownership transfers to USER, including when the window later dies.
    if let Some(r) = region {
        std::mem::forget(r);
    }
    Ok(())
}
unsafe extern "system" fn collect_window(hwnd: HWND, data: LPARAM) -> i32 {
    unsafe {
        (*(data as *mut Vec<usize>)).push(hwnd as usize);
    }
    1
}
unsafe extern "system" fn collect_monitor(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
    unsafe {
        (*(data as *mut Vec<usize>)).push(h as usize);
    }
    1
}
impl Backend {
    pub fn new() -> Result<Self, AppError> {
        // Unique name also prevents a second backend instance adopting our cookies.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| error(ErrorCode::BackendUnavailable, e.to_string(), None))?
            .as_nanos();
        Ok(Self {
            previews: preview::Previews::default(),
            entries: HashMap::new(),
            property: wide(&format!(
                "e-desktop.{}.{}",
                unsafe { GetCurrentProcessId() },
                nonce
            )),
            next: 1,
        })
    }
    pub fn status(&self) -> BackendStatus {
        BackendStatus { kind: BackendKind::Windows, availability: BackendAvailability::Ready,
            capabilities: Capabilities { enumerate: true, placement: true, focus: true, close: true, minimize: true, clipping: true, focus_follows_pointer: true, ..Capabilities::default() },
            message: "Win32 polling backend. Elevated/protected windows excluded; foreground activation may be denied. Layered/RTL windows cannot be clipped. Rectangles are visible DWM frames. Focus border color and corner preference apply on Windows 11. DWM live overview previews for visible sources; low-level mouse hook for pointer focus, modifier drags and hot corners.".into() }
    }
    /// Top-level windows the pointer hook may grab.
    pub fn pointer_targets(&self) -> HashSet<usize> {
        self.entries.values().map(|e| e.hwnd).collect()
    }
    pub fn window_for(&self, hwnd: usize) -> Option<String> {
        self.entries
            .iter()
            .find(|(_, e)| e.hwnd == hwnd && self.alive(e))
            .map(|(id, _)| id.clone())
    }
    pub fn window_at(&self, x: i32, y: i32) -> Option<String> {
        let _dpi = DpiScope::enter().ok()?;
        let h = unsafe { GetAncestor(WindowFromPoint(POINT { x, y }), GA_ROOT) };
        self.window_for(h as usize)
    }
    /// Pointer focus must not dismiss menus, dialogs, Start/search or other unmanaged popups.
    pub fn pointer_focus_allowed(&self) -> bool {
        let fg = unsafe { GetForegroundWindow() };
        if fg.is_null() {
            return true;
        }
        let mut pid = 0;
        let thread = unsafe { GetWindowThreadProcessId(fg, &mut pid) };
        let mut info: GUITHREADINFO = unsafe { zeroed() };
        info.cbSize = size_of::<GUITHREADINFO>() as u32;
        if unsafe { GetGUIThreadInfo(thread, &mut info) } != 0
            && info.flags
                & (GUI_INMENUMODE | GUI_POPUPMENUMODE | GUI_SYSTEMMENUMODE | GUI_INMOVESIZE)
                != 0
        {
            return false;
        }
        if pid == unsafe { GetCurrentProcessId() } || self.window_for(fg as usize).is_some() {
            return true;
        }
        let mut class = [0u16; 64];
        let n = unsafe { GetClassNameW(fg, class.as_mut_ptr(), class.len() as i32) };
        matches!(
            String::from_utf16_lossy(&class[..n.max(0) as usize]).as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        )
    }
    /// A managed window is in the system move/size loop (its own border or title bar is
    /// being dragged). Placing windows now would fight the loop.
    pub fn move_size_window(&self) -> Option<String> {
        let fg = unsafe { GetForegroundWindow() };
        if fg.is_null() {
            return None;
        }
        let thread = unsafe { GetWindowThreadProcessId(fg, null_mut()) };
        let mut info: GUITHREADINFO = unsafe { zeroed() };
        info.cbSize = size_of::<GUITHREADINFO>() as u32;
        if unsafe { GetGUIThreadInfo(thread, &mut info) } == 0 || info.flags & GUI_INMOVESIZE == 0 {
            return None;
        }
        self.window_for(unsafe { GetAncestor(info.hwndMoveSize, GA_ROOT) } as usize)
    }
    /// Center the pointer in `target` unless it is already inside.
    pub fn warp_pointer(&self, target: Rect) {
        let Ok(_dpi) = DpiScope::enter() else { return };
        let mut p = POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut p) } != 0
            && p.x as i64 >= target.x as i64
            && p.y as i64 >= target.y as i64
            && (p.x as i64) < target.x as i64 + target.width as i64
            && (p.y as i64) < target.y as i64 + target.height as i64
        {
            return;
        }
        unsafe {
            SetCursorPos(
                (target.x as i64 + target.width as i64 / 2) as i32,
                (target.y as i64 + target.height as i64 / 2) as i32,
            );
        }
    }
    fn alive(&self, e: &Entry) -> bool {
        let h = e.hwnd as HWND;
        let mut pid = 0;
        unsafe {
            IsWindow(h) != 0
                && GetWindowThreadProcessId(h, &mut pid) != 0
                && pid == e.pid
                && GetPropW(h, self.property.as_ptr()) as usize == e.cookie
        }
    }
    pub fn enumerate(&mut self) -> Result<SystemSnapshot, AppError> {
        self.prune_previews();
        let _dpi = DpiScope::enter()?;
        let mut result = SystemSnapshot::default();
        let mut monitors = Vec::<usize>::new();
        if unsafe {
            EnumDisplayMonitors(
                null_mut(),
                null(),
                Some(collect_monitor),
                &mut monitors as *mut _ as LPARAM,
            )
        } == 0
        {
            return Err(failed("EnumDisplayMonitors", None));
        }
        for h in monitors {
            let mut info: MONITORINFOEXW = unsafe { zeroed() };
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if unsafe { GetMonitorInfoW(h as HMONITOR, &mut info.monitorInfo) } == 0 {
                return Err(failed("GetMonitorInfoW", None));
            }
            let (mut x, mut y) = (96, 96);
            let hr = unsafe { GetDpiForMonitor(h as HMONITOR, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
            if hr < 0 {
                return Err(error(
                    ErrorCode::BackendUnavailable,
                    format!("GetDpiForMonitor failed: {hr:#x}"),
                    None,
                ));
            }
            result.monitors.push(Monitor {
                id: monitor_id(h as HMONITOR),
                name: String::from_utf16_lossy(
                    &info.szDevice[..info
                        .szDevice
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(info.szDevice.len())],
                ),
                bounds: rect(info.monitorInfo.rcMonitor),
                work_area: rect(info.monitorInfo.rcWork),
                scale_factor: f64::from(x) / 96.0,
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        let stale: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, e)| !self.alive(e))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.entries.remove(&id);
        }
        let mut handles = Vec::<usize>::new();
        if unsafe { EnumWindows(Some(collect_window), &mut handles as *mut _ as LPARAM) } == 0 {
            return Err(failed("EnumWindows", None));
        }
        for raw in handles {
            let h = raw as HWND;
            let existing = self
                .entries
                .iter()
                .find(|(_, e)| e.hwnd == raw)
                .map(|(id, _)| id.clone());
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(h, &mut pid);
            }
            let style = unsafe { GetWindowLongPtrW(h, GWL_STYLE) } as u32;
            let ex = unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32;
            let managed = existing
                .as_ref()
                .is_some_and(|id| self.entries[id].saved.is_some());
            if !managed {
                let mut cloaked: u32 = 0;
                let hr = unsafe {
                    DwmGetWindowAttribute(
                        h,
                        DWMWA_CLOAKED as u32,
                        &mut cloaked as *mut _ as _,
                        size_of::<u32>() as u32,
                    )
                };
                let mut class = [0u16; 256];
                let n = unsafe { GetClassNameW(h, class.as_mut_ptr(), class.len() as i32) };
                let class = String::from_utf16_lossy(&class[..n.max(0) as usize]);
                if pid == unsafe { GetCurrentProcessId() }
                    || pid == 0
                    || unsafe { IsWindowVisible(h) } == 0
                    || style & WS_CHILD != 0
                    || ex & (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) != 0
                    || !unsafe { GetWindow(h, GW_OWNER) }.is_null()
                    || hr < 0
                    || cloaked != 0
                    || matches!(
                        class.as_str(),
                        "Progman"
                            | "WorkerW"
                            | "Shell_TrayWnd"
                            | "Shell_SecondaryTrayWnd"
                            | "Windows.UI.Core.CoreWindow"
                    )
                {
                    continue;
                }
            }
            // A managed window the app hid (closed to the tray) leaves the layout; it joins
            // again as a new window when shown.
            if unsafe { IsWindowVisible(h) } == 0 {
                continue;
            }
            let minimized = unsafe { IsIconic(h) } != 0;
            let Some(outer) = outer_frame(h) else {
                continue;
            };
            let id = if let Some(id) = existing {
                id
            } else {
                let cookie = self.next;
                self.next = self.next.checked_add(1).ok_or_else(|| {
                    error(ErrorCode::BackendUnavailable, "Window ID exhausted", None)
                })?;
                // A property is removed by Windows on HWND destruction, even on same-PID reuse.
                if unsafe { SetPropW(h, self.property.as_ptr(), cookie as HANDLE) } == 0 {
                    let failure = unsafe { GetLastError() };
                    if failure == ERROR_ACCESS_DENIED || unsafe { IsWindow(h) } == 0 {
                        continue;
                    }
                    return Err(error(
                        ErrorCode::BackendUnavailable,
                        format!("SetPropW lifetime registration failed (Win32 {failure})"),
                        None,
                    ));
                }
                let id = format!("win-{cookie}");
                self.entries.insert(
                    id.clone(),
                    Entry {
                        hwnd: raw,
                        pid,
                        cookie,
                        saved: None,
                        minimized: false,
                        decor: None,
                        pads: vec![],
                        placed_pad: None,
                        region_box: None,
                    },
                );
                id
            };
            // Report the visible frame with the same border padding placement used, so a
            // placed window reads back exactly as planned (region or not).
            let entry = &self.entries[&id];
            let dpi = unsafe { GetDpiForWindow(h) };
            let known = entry.pads.iter().find(|(d, _)| *d == dpi).map(|(_, p)| *p);
            let r = match (entry.saved.as_ref().and(entry.placed_pad), known) {
                _ if minimized => outer,
                (Some(pad), _) | (None, Some(pad)) => inset(outer, pad),
                (None, None) => dwm_frame(h).unwrap_or(outer),
            };
            let mut title = vec![0u16; 32768];
            let n = unsafe { GetWindowTextW(h, title.as_mut_ptr(), title.len() as i32) }.max(0)
                as usize;
            let mut app = format!("PID {pid}");
            let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
            if !process.is_null() {
                let mut path = vec![0u16; 32768];
                let mut len = path.len() as u32;
                if unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) }
                    != 0
                {
                    let path = String::from_utf16_lossy(&path[..len as usize]);
                    app = path.rsplit('\\').next().unwrap_or(&path).to_owned();
                }
                unsafe {
                    CloseHandle(process);
                }
            }
            result.windows.push(NativeWindow {
                id: id.clone(),
                title: String::from_utf16_lossy(&title[..n]),
                app_name: app,
                process_id: pid,
                monitor_id: monitor_id(unsafe { MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST) }),
                rect: rect(r),
                minimized,
                minimized_by_manager: self.entries[&id].minimized && minimized,
                resizable: style & WS_THICKFRAME != 0,
            });
            if unsafe { GetForegroundWindow() } == h {
                result.focused_window = Some(id);
            }
        }
        Ok(result)
    }
    fn entry(&self, id: &str) -> Result<&Entry, AppError> {
        self.entries
            .get(id)
            .filter(|e| self.alive(e))
            .ok_or_else(|| {
                error(
                    ErrorCode::WindowGone,
                    "Window lifetime ended; refresh before retrying",
                    Some(id),
                )
            })
    }
    fn save(&mut self, id: &str) -> Result<(), AppError> {
        let e = self.entry(id)?;
        if e.saved.is_some() {
            return Ok(());
        }
        let h = e.hwnd as HWND;
        let mut placement: WINDOWPLACEMENT = unsafe { zeroed() };
        placement.length = size_of::<WINDOWPLACEMENT>() as u32;
        if unsafe { GetWindowPlacement(h, &mut placement) } == 0 {
            return Err(failed("GetWindowPlacement", Some(id)));
        }
        let region = fresh_region()?;
        // GetWindowRgn returns ERROR for an ordinary rectangular window without a region.
        let region = if unsafe { GetWindowRgn(h, region.handle()) } == ERROR {
            None
        } else {
            Some(region)
        };
        self.entries.get_mut(id).unwrap().saved = Some(Saved {
            placement,
            region,
            clipped: false,
        });
        set_transitions(h, true);
        Ok(())
    }
    /// Replace our clip with `local` (window coordinates; None = nothing visible), always
    /// intersected with the application's own original region.
    fn set_clip(&mut self, id: &str, h: HWND, local: Option<RECT>) -> Result<(), AppError> {
        let region = match local {
            Some(r) => Region(unsafe { CreateRectRgn(r.left, r.top, r.right, r.bottom) } as usize),
            None => fresh_region()?,
        };
        if region.0 == 0 {
            return Err(failed("CreateRectRgn", Some(id)));
        }
        let entry = self.entries.get_mut(id).unwrap();
        let saved = entry.saved.as_mut().unwrap();
        if let Some(original) = &saved.region {
            if unsafe { CombineRgn(region.handle(), region.handle(), original.handle(), RGN_AND) }
                == ERROR
            {
                return Err(failed("CombineRgn", Some(id)));
            }
        }
        set_region(h, Some(region))?;
        saved.clipped = true;
        entry.region_box = local;
        Ok(())
    }
    /// Put the application's original region back.
    fn unclip(&mut self, id: &str, h: HWND) -> Result<(), AppError> {
        let entry = self.entries.get_mut(id).unwrap();
        let saved = entry.saved.as_mut().unwrap();
        let original = saved.region.as_ref().map(copy_region).transpose()?;
        set_region(h, original)?;
        saved.clipped = false;
        entry.region_box = None;
        Ok(())
    }
    pub fn apply(&mut self, actions: &[NativeAction]) -> Result<(), AppError> {
        let _dpi = DpiScope::enter()?;
        for action in actions {
            match action {
                NativeAction::Placement {
                    window_id,
                    rect,
                    clip,
                    minimized,
                } => {
                    if let Err(mut failure) = self.place(window_id, *rect, *clip, *minimized) {
                        failure.window_id = Some(window_id.clone());
                        // In particular, never leave our temporary empty region on a failed move.
                        if let Err(restore) = self.restore_one(window_id) {
                            failure.message.push_str(&format!(
                                " Restoration also failed: {}",
                                restore.message
                            ));
                        }
                        return Err(failure);
                    }
                }
                NativeAction::Focus { window_id } => {
                    let h = self.entry(window_id)?.hwnd as HWND;
                    self.save(window_id)?;
                    if unsafe { IsIconic(h) } != 0 {
                        self.show(h, SW_SHOWNOACTIVATE, false, window_id)?;
                    }
                    if unsafe { SetForegroundWindow(h) } == 0 {
                        return Err(error(
                            ErrorCode::OperationDenied,
                            "Windows denied foreground activation",
                            Some(window_id),
                        ));
                    }
                    self.entries.get_mut(window_id).unwrap().minimized = false;
                }
                NativeAction::Close { window_id } => {
                    let h = self.entry(window_id)?.hwnd as HWND;
                    if unsafe { PostMessageW(h, WM_CLOSE, 0, 0) } == 0 {
                        return Err(failed("PostMessage(WM_CLOSE)", Some(window_id)));
                    }
                }
                NativeAction::Restore { window_id } => self.restore_one(window_id)?,
            }
        }
        Ok(())
    }
    fn show(&self, h: HWND, command: i32, iconic: bool, id: &str) -> Result<(), AppError> {
        // ShowWindow's return is previous visibility, NOT success.
        unsafe {
            ShowWindow(h, command);
        }
        if (unsafe { IsIconic(h) } != 0) != iconic || unsafe { IsWindow(h) } == 0 {
            return Err(error(
                ErrorCode::OperationDenied,
                "Window refused show/minimize state",
                Some(id),
            ));
        }
        Ok(())
    }
    /// Border padding at the window's current DPI, refreshed while DWM can report it
    /// (visible, without our region). A zero reading never replaces a known padding
    /// (DWM lags behind region removal).
    fn measure_pad(&mut self, id: &str, h: HWND) -> Option<[i32; 4]> {
        let dpi = unsafe { GetDpiForWindow(h) };
        let e = self.entries.get_mut(id)?;
        let known = e.pads.iter().find(|(d, _)| *d == dpi).map(|(_, p)| *p);
        if unsafe { IsIconic(h) } != 0 || e.saved.as_ref().is_some_and(|s| s.clipped) {
            return known;
        }
        let (Some(outer), Some(visible)) = (outer_frame(h), dwm_frame(h)) else {
            return known;
        };
        let pad = frame_pad(outer, visible);
        if !pad.iter().all(|p| (0..=32).contains(p)) || (pad == [0; 4] && known.is_some()) {
            return known;
        }
        e.pads.retain(|(d, _)| *d != dpi);
        e.pads.push((dpi, pad));
        Some(pad)
    }
    fn place(
        &mut self,
        id: &str,
        target: Rect,
        clip: Option<Rect>,
        minimized: bool,
    ) -> Result<(), AppError> {
        let r = native(target)?;
        let h = self.entry(id)?.hwnd as HWND;
        let clipping = if let Some(c) = clip {
            let c = native(c)?;
            let monitor = unsafe { MonitorFromRect(&c, MONITOR_DEFAULTTONEAREST) };
            let mut info: MONITORINFO = unsafe { zeroed() };
            info.cbSize = size_of::<MONITORINFO>() as u32;
            if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
                return Err(failed("GetMonitorInfoW", Some(id)));
            }
            Some(intersect(r, c).and_then(|v| intersect(v, info.rcWork)))
        } else {
            None
        };
        if clipping.is_some()
            && unsafe { GetWindowLongPtrW(h, GWL_EXSTYLE) } as u32
                & (WS_EX_LAYERED | WS_EX_LAYOUTRTL)
                != 0
        {
            return Err(error(
                ErrorCode::OperationDenied,
                "Safe region clipping is unsupported for layered/RTL windows",
                Some(id),
            ));
        }
        self.save(id)?;
        let mut pad = self
            .measure_pad(id, h)
            .or(self.entries[id].placed_pad)
            .unwrap_or_default();
        if minimized || matches!(clipping, Some(None)) {
            if unsafe { IsIconic(h) } == 0 {
                self.show(h, SW_SHOWMINNOACTIVE, true, id)?;
                self.entries.get_mut(id).unwrap().minimized = true;
            }
            return Ok(());
        }
        let iconic = unsafe { IsIconic(h) } != 0;
        // Mask BEFORE moving/restoring with the part visible both before and after the move,
        // so a GDI window never paints outside either clip (an empty mask would blink it).
        if let Some(Some(c)) = clipping {
            let next = local_clip(c, outset(r, pad))?;
            let now = self.entries[id].region_box.or_else(|| {
                let o = outer_frame(h)?;
                Some(RECT {
                    left: 0,
                    top: 0,
                    right: o.right - o.left,
                    bottom: o.bottom - o.top,
                })
            });
            let mask = if iconic {
                Some(next)
            } else {
                now.and_then(|now| intersect(now, next))
            };
            self.set_clip(id, h, mask)?;
        }
        if iconic {
            // Restore straight into the target slot, not first at the old normal position.
            let mut placement: WINDOWPLACEMENT = unsafe { zeroed() };
            placement.length = size_of::<WINDOWPLACEMENT>() as u32;
            if unsafe { GetWindowPlacement(h, &mut placement) } == 0 {
                return Err(failed("GetWindowPlacement", Some(id)));
            }
            placement.flags = 0;
            placement.showCmd = SW_SHOWNOACTIVATE as u32;
            placement.rcNormalPosition = workspace(outset(r, pad));
            unsafe {
                SetWindowPlacement(h, &placement);
            }
            if unsafe { IsIconic(h) } != 0 {
                self.show(h, SW_SHOWNOACTIVATE, false, id)?;
            }
        } else if unsafe { IsZoomed(h) } != 0 {
            self.show(h, SW_SHOWNOACTIVATE, false, id)?;
        }
        // Window regions do not clip DirectComposition content (Chromium, Electron, WinUI,
        // Terminal): the cut-off part would still show on a neighbouring monitor. Keep clipped
        // windows at the bottom, below that monitor's own windows, which then cover it.
        let (after, order) = if clipping.is_some() {
            (HWND_BOTTOM, 0)
        } else {
            (null_mut(), SWP_NOZORDER)
        };
        // `r` is the visible target; SetWindowPos takes the outer frame, which includes the
        // invisible resize border.
        let position = |pad: [i32; 4]| {
            let outer = outset(r, pad);
            (unsafe {
                SetWindowPos(
                    h,
                    after,
                    outer.left,
                    outer.top,
                    outer.right - outer.left,
                    outer.bottom - outer.top,
                    SWP_NOACTIVATE | SWP_NOOWNERZORDER | order,
                )
            } != 0)
                .then_some(outer)
                .ok_or_else(|| failed("SetWindowPos", Some(id)))
        };
        let mut outer = position(pad)?;
        // Crossing into a monitor with different scaling makes the app resize itself
        // (WM_DPICHANGED) and changes the invisible border. A column peeking in at the screen
        // edge does this when most of it lies on the neighbouring monitor. Place once more
        // with the new DPI's border: the window is already there, so the size now holds.
        let resized = outer_frame(h).is_some_and(|actual| !same_rect(actual, outer));
        // A first clipped move to a new DPI has no cached border there. Regions make DWM
        // report the outer frame, so restore the original region and synchronize composition
        // before measuring. Never turn an unavailable measurement into a zero-width border.
        if self.measure_pad(id, h).is_none()
            && self.entries[id].saved.as_ref().is_some_and(|s| s.clipped)
        {
            self.unclip(id, h)?;
            unsafe {
                DwmFlush();
            }
        }
        let new_pad = self.measure_pad(id, h).unwrap_or(pad);
        if resized || new_pad != pad {
            pad = new_pad;
            outer = position(pad)?;
        }
        self.entries.get_mut(id).unwrap().placed_pad = Some(pad);
        let actual = outer_frame(h).ok_or_else(|| failed("GetWindowRect", Some(id)))?;
        if let Some(Some(c)) = clipping {
            let c = intersect(c, actual).ok_or_else(|| {
                error(
                    ErrorCode::OperationDenied,
                    "Window refused placement within clip",
                    Some(id),
                )
            })?;
            // Region coordinates are relative to the outer frame, not the visible bounds.
            let local = local_clip(c, actual)?;
            if self.entries[id].region_box.is_none_or(|b| !same_rect(b, local)) {
                self.set_clip(id, h, Some(local))?;
            }
        } else if self.entries[id].saved.as_ref().unwrap().clipped {
            self.unclip(id, h)?;
        }
        self.entries.get_mut(id).unwrap().minimized = false;
        if !same_rect(actual, outer) {
            return Err(error(
                ErrorCode::OperationDenied,
                "Window constrained requested size/position; refresh required",
                Some(id),
            ));
        }
        Ok(())
    }
    /// Focus border and corners for managed windows. Unchanged windows are not touched.
    pub fn set_decorations(
        &mut self,
        focused: Option<&str>,
        border: Option<u32>,
        corners: Option<i32>,
    ) {
        let corner = corners.unwrap_or(DWMWCP_DEFAULT);
        let ids: Vec<_> = self.entries.keys().cloned().collect();
        for id in ids {
            let Some(entry) = self.entries.get(&id) else {
                continue;
            };
            if entry.saved.is_none() || !self.alive(entry) {
                continue;
            }
            let desired = border.map(|color| {
                if focused == Some(id.as_str()) {
                    color
                } else {
                    DWMWA_COLOR_DEFAULT
                }
            });
            let previous = entry.decor;
            let (border_changed, corner_changed) = match previous {
                Some((b, c)) => (b != desired, c != corner),
                None => (desired.is_some(), corner != DWMWCP_DEFAULT),
            };
            if !border_changed && !corner_changed {
                continue;
            }
            let hwnd = entry.hwnd as HWND;
            if border_changed {
                let color = desired.unwrap_or(DWMWA_COLOR_DEFAULT);
                unsafe {
                    DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_BORDER_COLOR as u32,
                        &color as *const _ as _,
                        size_of::<u32>() as u32,
                    );
                }
            }
            if corner_changed {
                unsafe {
                    DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                        &corner as *const _ as _,
                        size_of::<i32>() as u32,
                    );
                }
            }
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.decor = if desired.is_none() && corner == DWMWCP_DEFAULT {
                    None
                } else {
                    Some((desired, corner))
                };
            }
        }
    }
    fn restore_one(&mut self, id: &str) -> Result<(), AppError> {
        let Some(e) = self.entries.get(id) else {
            return Ok(());
        };
        if !self.alive(e) {
            self.entries.remove(id);
            return Ok(());
        }
        let Some(saved) = &e.saved else {
            return Ok(());
        };
        let h = e.hwnd as HWND;
        let mut placement = saved.placement;
        // A window the app hid (tray) keeps its original geometry but stays hidden.
        if unsafe { IsWindowVisible(h) } == 0 {
            placement.showCmd = SW_HIDE as u32;
        }
        // Restore geometry while still masked, then restore the original region.
        let placement_error = (unsafe { SetWindowPlacement(h, &placement) } == 0)
            .then(|| failed("SetWindowPlacement", Some(id)));
        // Region recovery must still run if placement failed (the current region may be empty).
        let region_result = if saved.clipped {
            saved
                .region
                .as_ref()
                .map(copy_region)
                .transpose()
                .and_then(|r| set_region(h, r))
        } else {
            Ok(())
        };
        if let Some(mut failure) = placement_error {
            if let Err(region) = region_result {
                failure.message.push_str(&format!(
                    " Region restoration also failed: {}",
                    region.message
                ));
            }
            return Err(failure);
        }
        region_result?;
        set_transitions(h, false);
        let e = self.entries.get_mut(id).unwrap();
        e.region_box = None;
        if e.decor.is_some() {
            reset_decor(h);
            e.decor = None;
        }
        e.saved = None;
        e.placed_pad = None;
        e.minimized = false;
        Ok(())
    }
    pub fn restore(&mut self) -> Result<(), AppError> {
        self.clear_previews();
        let _dpi = DpiScope::enter()?;
        let ids: Vec<_> = self.entries.keys().cloned().collect();
        let mut failures = Vec::new();
        for id in ids {
            if let Err(e) = self.restore_one(&id) {
                failures.push(e.message);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(error(ErrorCode::OperationDenied, failures.join("; "), None))
        }
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            eprintln!("Windows backend drop restoration: {e}");
        }
        for e in self.entries.values() {
            if self.alive(e) {
                unsafe {
                    RemovePropW(e.hwnd as HWND, self.property.as_ptr());
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_rectangles_and_clip_edges() {
        fn assert_send<T: Send>() {}
        assert_send::<Backend>();
        let a = native(Rect {
            x: -1920,
            y: -100,
            width: 1920,
            height: 1080,
        })
        .unwrap();
        let b = native(Rect {
            x: -100,
            y: 0,
            width: 200,
            height: 100,
        })
        .unwrap();
        let clipped = intersect(a, b).unwrap();
        assert_eq!(
            rect(clipped),
            Rect {
                x: -100,
                y: 0,
                width: 100,
                height: 100
            }
        );
        assert_eq!(
            rect(local_clip(clipped, a).unwrap()),
            Rect {
                x: 1820,
                y: 100,
                width: 100,
                height: 100
            }
        );
        assert!(
            local_clip(
                RECT {
                    left: i32::MAX,
                    top: 0,
                    right: i32::MAX,
                    bottom: 1
                },
                RECT {
                    left: i32::MIN,
                    top: 0,
                    right: 0,
                    bottom: 1
                }
            )
            .is_err()
        );
        assert!(
            intersect(
                a,
                native(Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10
                })
                .unwrap()
            )
            .is_none()
        );
        assert!(
            native(Rect {
                x: i32::MAX,
                y: 0,
                width: 1,
                height: 1
            })
            .is_err()
        );
        assert!(native(Rect::default()).is_err());
        let outer = RECT {
            left: 0,
            top: 0,
            right: 114,
            bottom: 114,
        };
        let visible = RECT {
            left: 7,
            top: 0,
            right: 107,
            bottom: 107,
        };
        let pad = frame_pad(outer, visible);
        assert_eq!(pad, [7, 0, 7, 7]);
        let target = RECT {
            left: 100,
            top: 50,
            right: 500,
            bottom: 400,
        };
        let placed = outset(target, pad);
        assert!(same_rect(
            placed,
            RECT {
                left: 93,
                top: 50,
                right: 507,
                bottom: 407,
            }
        ));
        assert!(
            native(Rect {
                x: i32::MIN,
                y: 0,
                width: u32::MAX,
                height: 1
            })
            .is_err()
        );
    }
}
