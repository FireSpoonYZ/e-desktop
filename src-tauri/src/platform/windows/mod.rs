//! Win32 backend. Poll on the owning thread; no hooks or desktop mutation at construction.
pub mod preview;
use crate::model::*;
use std::{
    collections::HashMap,
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
            capabilities: Capabilities { enumerate: true, placement: true, focus: true, close: true, minimize: true, clipping: true, ..Capabilities::default() },
            message: "Win32 polling backend. Elevated/protected windows excluded; foreground activation may be denied. Layered/RTL windows cannot be clipped. DWM preview API requires overview host integration; no pointer-focus implementation.".into() }
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
            let mut r: RECT = unsafe { zeroed() };
            if unsafe { GetWindowRect(h, &mut r) } == 0 {
                continue;
            }
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
                    },
                );
                id
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
            let minimized = unsafe { IsIconic(h) } != 0;
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
        if minimized || matches!(clipping, Some(None)) {
            if unsafe { IsIconic(h) } == 0 {
                self.show(h, SW_SHOWMINNOACTIVE, true, id)?;
                self.entries.get_mut(id).unwrap().minimized = true;
            }
            return Ok(());
        }
        // Mask BEFORE moving/restoring, so a cross-monitor move never paints outside its clip.
        if clipping.is_some() {
            set_region(h, Some(fresh_region()?))?;
            self.entries
                .get_mut(id)
                .unwrap()
                .saved
                .as_mut()
                .unwrap()
                .clipped = true;
        }
        if unsafe { IsIconic(h) } != 0 || unsafe { IsZoomed(h) } != 0 {
            self.show(h, SW_SHOWNOACTIVATE, false, id)?;
        }
        if unsafe {
            SetWindowPos(
                h,
                null_mut(),
                target.x,
                target.y,
                target.width as i32,
                target.height as i32,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
            )
        } == 0
        {
            return Err(failed("SetWindowPos", Some(id)));
        }
        let mut actual: RECT = unsafe { zeroed() };
        if unsafe { GetWindowRect(h, &mut actual) } == 0 {
            return Err(failed("GetWindowRect", Some(id)));
        }
        if let Some(Some(c)) = clipping {
            let c = intersect(c, actual).ok_or_else(|| {
                error(
                    ErrorCode::OperationDenied,
                    "Window refused placement within clip",
                    Some(id),
                )
            })?;
            let local = local_clip(c, actual)?;
            let region =
                Region(
                    unsafe { CreateRectRgn(local.left, local.top, local.right, local.bottom) }
                        as usize,
                );
            if region.0 == 0 {
                return Err(failed("CreateRectRgn", Some(id)));
            }
            if let Some(original) = &self.entries[id].saved.as_ref().unwrap().region {
                if unsafe {
                    CombineRgn(region.handle(), region.handle(), original.handle(), RGN_AND)
                } == ERROR
                {
                    return Err(failed("CombineRgn", Some(id)));
                }
            }
            set_region(h, Some(region))?;
        } else if self.entries[id].saved.as_ref().unwrap().clipped {
            let original = self.entries[id]
                .saved
                .as_ref()
                .unwrap()
                .region
                .as_ref()
                .map(copy_region)
                .transpose()?;
            set_region(h, original)?;
            self.entries
                .get_mut(id)
                .unwrap()
                .saved
                .as_mut()
                .unwrap()
                .clipped = false;
        }
        self.entries.get_mut(id).unwrap().minimized = false;
        if rect(actual) != target {
            return Err(error(
                ErrorCode::OperationDenied,
                "Window constrained requested size/position; refresh required",
                Some(id),
            ));
        }
        Ok(())
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
        // Restore geometry while still masked, then restore the original region.
        let placement_error = (unsafe { SetWindowPlacement(h, &saved.placement) } == 0)
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
        let e = self.entries.get_mut(id).unwrap();
        e.saved = None;
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
