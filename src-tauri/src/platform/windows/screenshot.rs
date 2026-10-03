//! lane: rules-spawn-screenshot. Screenshots: GDI copies of the composed screen, PrintWindow
//! for clipped or off-screen windows, a native frozen-screen region picker, WIC PNG files and
//! a CF_DIB clipboard copy. Everything after target selection runs on its own thread.
use super::snapshot::Dib;
use super::*;
use crate::screenshot::{Busy, Image, LocalTime, expand_path};
use std::{os::windows::ffi::OsStrExt, path::Path, time::Duration};
use windows::{
    Win32::{
        Foundation::GENERIC_WRITE,
        Graphics::Imaging::*,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
    },
    core::PCWSTR,
};
use windows_sys::Win32::{
    Storage::Xps::PrintWindow,
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        LibraryLoader::GetModuleHandleW,
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        SystemInformation::GetLocalTime,
    },
    UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE, VK_RETURN, VK_SPACE},
};

/// Largest capture: three 8K displays side by side fit.
const CAPTURE_BYTES: usize = 1 << 30;
const CF_DIB: u32 = 8;
/// SDK PW_RENDERFULLCONTENT (WinUser.h): DirectComposition content too.
const PW_RENDERFULLCONTENT: u32 = 2;
/// Selection outline, the default focus border color #7fc8ff as COLORREF.
const OUTLINE: u32 = 0x00ff_c87f;
const PICKER_CLASS: &str = "e-desktop.screenshot";

fn shot_error(message: impl Into<String>) -> AppError {
    error(ErrorCode::OperationDenied, message, None)
}

/// What to capture, resolved on the controller thread.
pub enum Target {
    Region,
    Screen(Rect),
    Window(WindowTarget),
}

pub struct WindowTarget {
    hwnd: usize,
    /// Invisible border of a managed window; DWM cannot report it while we clip the window.
    pad: Option<[i32; 4]>,
}

impl Backend {
    /// The native foreground window; the layout focus while one of our surfaces is in front.
    pub fn screenshot_window(&self, focused: Option<&str>) -> Result<WindowTarget, AppError> {
        let fg = unsafe { GetForegroundWindow() };
        let mut pid = 0;
        let ours = fg.is_null()
            || unsafe { GetWindowThreadProcessId(fg, &mut pid) } == 0
            || pid == unsafe { GetCurrentProcessId() };
        let hwnd = if ours {
            focused
                .and_then(|id| self.entries.get(id))
                .filter(|e| self.alive(e))
                .map(|e| e.hwnd)
                .ok_or_else(|| shot_error("没有可截取的焦点窗口。"))?
        } else {
            unsafe { GetAncestor(fg, GA_ROOT) as usize }
        };
        let pad = self
            .entries
            .values()
            .find(|e| e.hwnd == hwnd)
            .and_then(|e| e.placed_pad);
        Ok(WindowTarget { hwnd, pad })
    }
}

/// Capture on a new thread, then save `path` (a template, None: clipboard only) and copy
/// to the clipboard. `settle` waits for our own just-dismissed surfaces to leave the screen.
pub fn start(
    target: Target,
    path: Option<String>,
    settle: bool,
    report: impl Fn(AppError) + Send + 'static,
) -> Result<(), AppError> {
    let busy = Busy::begin().ok_or_else(|| shot_error("上一次截图尚未完成。"))?;
    std::thread::Builder::new()
        .name("screenshot".into())
        .spawn(move || {
            let _busy = busy;
            if settle {
                std::thread::sleep(Duration::from_millis(150));
            }
            let image = match target {
                Target::Region => pick(),
                Target::Screen(rect) => capture_screen(rect).map(Some),
                Target::Window(window) => capture_window(&window).map(Some),
            };
            let image = match image {
                Ok(Some(image)) => image,
                Ok(None) => return,
                Err(issue) => return report(issue),
            };
            if let Some(template) = path {
                let path = expand_path(&template, local_time(), |name| std::env::var(name).ok());
                if let Err(issue) = save_png(&image, Path::new(&path)) {
                    report(issue);
                }
            }
            if let Err(issue) = copy_to_clipboard(&image) {
                report(issue);
            }
        })
        .map_err(|e| shot_error(format!("无法启动截图线程：{e}")))?;
    Ok(())
}

fn local_time() -> LocalTime {
    let mut t: SYSTEMTIME = unsafe { zeroed() };
    unsafe { GetLocalTime(&mut t) };
    LocalTime {
        year: t.wYear,
        month: t.wMonth,
        day: t.wDay,
        hour: t.wHour,
        minute: t.wMinute,
        second: t.wSecond,
    }
}

fn size_of_rect(r: RECT) -> SIZE {
    SIZE {
        cx: r.right - r.left,
        cy: r.bottom - r.top,
    }
}

fn new_dib(size: SIZE) -> Result<Dib, AppError> {
    Dib::new(size, CAPTURE_BYTES)
        .ok_or_else(|| shot_error(format!("无法为 {}×{} 的截图分配位图。", size.cx, size.cy)))
}

/// Copy composed screen pixels (layered windows included) at screen rectangle `r`.
fn grab_screen(dib: &Dib, r: RECT) -> Result<(), AppError> {
    let size = size_of_rect(r);
    let screen = unsafe { GetDC(null_mut()) };
    if screen.is_null() {
        return Err(failed("GetDC (screenshot)", None));
    }
    let copied = unsafe {
        BitBlt(
            dib.dc,
            0,
            0,
            size.cx,
            size.cy,
            screen,
            r.left,
            r.top,
            SRCCOPY | CAPTUREBLT,
        )
    };
    unsafe { ReleaseDC(null_mut(), screen) };
    if copied == 0 {
        return Err(failed("BitBlt (screenshot)", None));
    }
    Ok(())
}

/// GDI leaves alpha undefined; PNG and clipboard consumers must see opaque pixels.
fn image_of(dib: &mut Dib, size: SIZE) -> Image {
    unsafe { GdiFlush() };
    let mut pixels = dib.pixels().to_vec();
    for alpha in pixels.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
    Image {
        width: size.cx as u32,
        height: size.cy as u32,
        pixels,
    }
}

fn capture_screen(rect: Rect) -> Result<Image, AppError> {
    let _dpi = DpiScope::enter()?;
    let r = native(rect)?;
    let mut dib = new_dib(size_of_rect(r))?;
    grab_screen(&dib, r)?;
    Ok(image_of(&mut dib, size_of_rect(r)))
}

fn monitor_rects() -> Vec<RECT> {
    let mut monitors = Vec::<usize>::new();
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(collect_monitor),
            &mut monitors as *mut _ as LPARAM,
        );
    }
    monitors
        .into_iter()
        .filter_map(|h| {
            let mut info: MONITORINFO = unsafe { zeroed() };
            info.cbSize = size_of::<MONITORINFO>() as u32;
            (unsafe { GetMonitorInfoW(h as HMONITOR, &mut info) } != 0).then_some(info.rcMonitor)
        })
        .collect()
}

fn area(r: RECT) -> i64 {
    (i64::from(r.right) - i64::from(r.left)) * (i64::from(r.bottom) - i64::from(r.top))
}

/// The visible frame is on screen without a window region: copy those screen pixels, the
/// way it looks. Otherwise (clipped by us, partly off screen) ask the window to render itself.
fn capture_window(target: &WindowTarget) -> Result<Image, AppError> {
    let _dpi = DpiScope::enter()?;
    let h = target.hwnd as HWND;
    if unsafe { IsWindow(h) } == 0 {
        return Err(error(ErrorCode::WindowGone, "要截取的窗口已关闭。", None));
    }
    if unsafe { IsIconic(h) } != 0 {
        return Err(shot_error("窗口已最小化，无法截图。"));
    }
    let outer = outer_frame(h).ok_or_else(|| failed("GetWindowRect (screenshot)", None))?;
    let mut region: RECT = unsafe { zeroed() };
    let clipped = unsafe { GetWindowRgnBox(h, &mut region) } != ERROR;
    let visible = match (clipped, target.pad) {
        (true, Some(pad)) => inset(outer, pad),
        (true, None) => outer,
        (false, _) => dwm_frame(h).unwrap_or(outer),
    };
    if area(visible) <= 0 {
        return Err(shot_error("窗口没有可截取的区域。"));
    }
    let on_screen: i64 = monitor_rects()
        .into_iter()
        .filter_map(|m| intersect(m, visible))
        .map(area)
        .sum();
    if !clipped && on_screen == area(visible) {
        let mut dib = new_dib(size_of_rect(visible))?;
        grab_screen(&dib, visible)?;
        return Ok(image_of(&mut dib, size_of_rect(visible)));
    }
    let size = size_of_rect(outer);
    let mut dib = new_dib(size)?;
    if unsafe { PrintWindow(h, dib.dc, PW_RENDERFULLCONTENT) } == 0 {
        return Err(failed("PrintWindow (screenshot)", None));
    }
    image_of(&mut dib, size)
        .crop(
            i64::from(visible.left) - i64::from(outer.left),
            i64::from(visible.top) - i64::from(outer.top),
            size_of_rect(visible).cx as u32,
            size_of_rect(visible).cy as u32,
        )
        .ok_or_else(|| shot_error("窗口没有可截取的区域。"))
}

struct Picker {
    bright: Dib,
    dim: Dib,
    size: SIZE,
    origin: POINT,
    outline: HBRUSH,
    anchor: Option<POINT>,
    /// Client coordinates; right/bottom exclusive.
    selection: Option<RECT>,
    /// Some(None): cancelled.
    done: Option<Option<RECT>>,
}

impl Picker {
    fn select(&mut self, h: HWND, selection: Option<RECT>) {
        for r in [self.selection, selection].into_iter().flatten() {
            let dirty = RECT {
                left: r.left - 3,
                top: r.top - 3,
                right: r.right + 3,
                bottom: r.bottom + 3,
            };
            unsafe { InvalidateRect(h, &dirty, 0) };
        }
        self.selection = selection;
    }

    fn finish(&mut self, h: HWND, result: Option<RECT>) {
        self.done = Some(result);
        unsafe { DestroyWindow(h) };
    }

    /// Enter without a drag takes the monitor under the pointer.
    fn monitor_under_pointer(&self) -> Option<RECT> {
        let mut p = POINT { x: 0, y: 0 };
        let mut info: MONITORINFO = unsafe { zeroed() };
        info.cbSize = size_of::<MONITORINFO>() as u32;
        unsafe {
            (GetCursorPos(&mut p) != 0
                && GetMonitorInfoW(MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST), &mut info) != 0)
                .then(|| RECT {
                    left: info.rcMonitor.left - self.origin.x,
                    top: info.rcMonitor.top - self.origin.y,
                    right: info.rcMonitor.right - self.origin.x,
                    bottom: info.rcMonitor.bottom - self.origin.y,
                })
        }
    }

    fn point(&self, l: LPARAM) -> POINT {
        POINT {
            x: i32::from(l as u16 as i16).clamp(0, self.size.cx - 1),
            y: i32::from((l >> 16) as u16 as i16).clamp(0, self.size.cy - 1),
        }
    }

    fn paint(&self, h: HWND) {
        let mut ps: PAINTSTRUCT = unsafe { zeroed() };
        let dc = unsafe { BeginPaint(h, &mut ps) };
        let r = ps.rcPaint;
        let size = size_of_rect(r);
        if !dc.is_null() && size.cx > 0 && size.cy > 0 {
            unsafe {
                // Compose off screen: the dim frame, the bright selection, then its outline.
                let memory = CreateCompatibleDC(dc);
                let bitmap = CreateCompatibleBitmap(dc, size.cx, size.cy);
                let previous = SelectObject(memory, bitmap);
                BitBlt(
                    memory,
                    0,
                    0,
                    size.cx,
                    size.cy,
                    self.dim.dc,
                    r.left,
                    r.top,
                    SRCCOPY,
                );
                if let Some(selection) = self.selection {
                    if let Some(i) = intersect(selection, r) {
                        let s = size_of_rect(i);
                        BitBlt(
                            memory,
                            i.left - r.left,
                            i.top - r.top,
                            s.cx,
                            s.cy,
                            self.bright.dc,
                            i.left,
                            i.top,
                            SRCCOPY,
                        );
                    }
                    for grow in 1..=2 {
                        let frame = RECT {
                            left: selection.left - r.left - grow,
                            top: selection.top - r.top - grow,
                            right: selection.right - r.left + grow,
                            bottom: selection.bottom - r.top + grow,
                        };
                        FrameRect(memory, &frame, self.outline);
                    }
                }
                BitBlt(dc, r.left, r.top, size.cx, size.cy, memory, 0, 0, SRCCOPY);
                SelectObject(memory, previous);
                DeleteObject(bitmap);
                DeleteDC(memory);
            }
        }
        unsafe { EndPaint(h, &ps) };
    }
}

unsafe extern "system" fn picker_proc(h: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let picker = unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) } as *mut Picker;
    if picker.is_null() {
        return unsafe { DefWindowProcW(h, message, w, l) };
    }
    // Only this thread's message loop reaches here while `pick` owns the Picker.
    let picker = unsafe { &mut *picker };
    match message {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            picker.paint(h);
            0
        }
        WM_LBUTTONDOWN => {
            unsafe { SetCapture(h) };
            picker.anchor = Some(picker.point(l));
            picker.select(h, None);
            0
        }
        WM_MOUSEMOVE => {
            if let Some(a) = picker.anchor {
                let p = picker.point(l);
                let selection = RECT {
                    left: a.x.min(p.x),
                    top: a.y.min(p.y),
                    right: a.x.max(p.x) + 1,
                    bottom: a.y.max(p.y) + 1,
                };
                picker.select(h, Some(selection));
            }
            0
        }
        WM_LBUTTONUP => {
            unsafe { ReleaseCapture() };
            picker.anchor = None;
            0
        }
        WM_RBUTTONUP => {
            picker.finish(h, None);
            0
        }
        WM_KEYDOWN => {
            match w as u16 {
                VK_ESCAPE => picker.finish(h, None),
                VK_RETURN | VK_SPACE => {
                    let selection = picker.selection.or_else(|| picker.monitor_under_pointer());
                    picker.finish(h, selection);
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            picker.finish(h, None);
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(h, message, w, l) },
    }
}

/// Freeze the whole virtual screen, let the user drag a rectangle (Enter/Space confirms,
/// Esc or right click cancels) and cut it from the frozen frame. None when cancelled.
fn pick() -> Result<Option<Image>, AppError> {
    let _dpi = DpiScope::enter()?;
    let screen = unsafe {
        RECT {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            right: GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN),
            bottom: GetSystemMetrics(SM_YVIRTUALSCREEN) + GetSystemMetrics(SM_CYVIRTUALSCREEN),
        }
    };
    let size = size_of_rect(screen);
    let mut bright = new_dib(size)?;
    grab_screen(&bright, screen)?;
    unsafe { GdiFlush() };
    let mut dim = new_dib(size)?;
    for (target, source) in dim.pixels().iter_mut().zip(bright.pixels().iter()) {
        *target = source / 2;
    }
    let outline = unsafe { CreateSolidBrush(OUTLINE) };
    if outline.is_null() {
        return Err(failed("CreateSolidBrush (screenshot)", None));
    }
    let mut picker = Picker {
        bright,
        dim,
        size,
        origin: POINT {
            x: screen.left,
            y: screen.top,
        },
        outline,
        anchor: None,
        selection: None,
        done: None,
    };
    let result = run_picker(&mut picker, screen);
    unsafe { DeleteObject(outline) };
    let Picker {
        mut bright,
        dim,
        done,
        ..
    } = picker;
    drop(dim);
    let selection = match (result, done) {
        (Err(issue), _) => return Err(issue),
        (Ok(()), Some(Some(selection))) => selection,
        _ => return Ok(None),
    };
    let s = size_of_rect(selection);
    // The frozen frame the user saw, not a fresh capture.
    Ok(image_of(&mut bright, size).crop(
        i64::from(selection.left),
        i64::from(selection.top),
        s.cx.max(0) as u32,
        s.cy.max(0) as u32,
    ))
}

fn run_picker(picker: &mut Picker, screen: RECT) -> Result<(), AppError> {
    let class = wide(PICKER_CLASS);
    let instance = unsafe { GetModuleHandleW(null()) };
    let registered = WNDCLASSW {
        lpfnWndProc: Some(picker_proc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        hCursor: unsafe { LoadCursorW(null_mut(), IDC_CROSS) },
        ..unsafe { zeroed() }
    };
    if unsafe { RegisterClassW(&registered) } == 0
        && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
    {
        return Err(failed("RegisterClassW (screenshot)", None));
    }
    let h = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
            class.as_ptr(),
            null(),
            WS_POPUP,
            screen.left,
            screen.top,
            picker.size.cx,
            picker.size.cy,
            null_mut(),
            null_mut(),
            instance,
            null(),
        )
    };
    if h.is_null() {
        return Err(failed("CreateWindowExW (screenshot)", None));
    }
    set_transitions(h, true);
    unsafe {
        SetWindowLongPtrW(h, GWLP_USERDATA, picker as *mut Picker as isize);
        ShowWindow(h, SW_SHOW);
        SetForegroundWindow(h);
        let mut message: MSG = zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

/// Single-threaded COM for this call; balanced unless another apartment already owns the thread.
struct Com(bool);
impl Com {
    fn enter() -> Self {
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn save_png(image: &Image, path: &Path) -> Result<(), AppError> {
    let failure =
        |e: &dyn std::fmt::Display| shot_error(format!("截图保存到 {} 失败：{e}", path.display()));
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| failure(&e))?;
    }
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let _com = Com::enter();
    let encode = || -> windows::core::Result<()> {
        unsafe {
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let stream = factory.CreateStream()?;
            stream.InitializeFromFilename(PCWSTR(file.as_ptr()), GENERIC_WRITE.0)?;
            let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, null())?;
            encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let (mut frame, mut options) = (None, None);
            encoder.CreateNewFrame(&mut frame, &mut options)?;
            let frame: IWICBitmapFrameEncode = frame.ok_or(windows::core::Error::empty())?;
            frame.Initialize(options.as_ref())?;
            frame.SetSize(image.width, image.height)?;
            let mut format = GUID_WICPixelFormat32bppBGRA;
            frame.SetPixelFormat(&mut format)?;
            if format != GUID_WICPixelFormat32bppBGRA {
                return Err(windows::core::Error::empty());
            }
            frame.WritePixels(image.height, image.width * 4, &image.pixels)?;
            frame.Commit()?;
            encoder.Commit()
        }
    };
    encode().map_err(|e| failure(&e))
}

/// CF_DIB: bottom-up 32-bit BI_RGB rows after the header.
fn dib_bytes(image: &Image) -> Vec<u8> {
    let header = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: image.width as i32,
        biHeight: image.height as i32,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB,
        biSizeImage: image.pixels.len() as u32,
        ..unsafe { zeroed() }
    };
    let mut bytes = unsafe {
        std::slice::from_raw_parts(
            &header as *const BITMAPINFOHEADER as *const u8,
            size_of::<BITMAPINFOHEADER>(),
        )
    }
    .to_vec();
    for row in image.pixels.chunks_exact(image.width as usize * 4).rev() {
        bytes.extend_from_slice(row);
    }
    bytes
}

fn copy_to_clipboard(image: &Image) -> Result<(), AppError> {
    let bytes = dib_bytes(image);
    // SetClipboardData fails after OpenClipboard(NULL); a message-only window owns the data.
    let class = wide("STATIC");
    let owner = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            null_mut(),
            null(),
        )
    };
    if owner.is_null() {
        return Err(failed("CreateWindowExW (clipboard)", None));
    }
    // Another application may hold the clipboard for a moment.
    let opened = (0..10).any(|attempt| {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(30));
        }
        unsafe { OpenClipboard(owner) != 0 }
    });
    let result = if !opened {
        Err(failed("OpenClipboard (screenshot)", None))
    } else {
        let result = unsafe {
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
            let target = if memory.is_null() {
                null_mut()
            } else {
                GlobalLock(memory) as *mut u8
            };
            if target.is_null() {
                if !memory.is_null() {
                    GlobalFree(memory);
                }
                Err(failed("GlobalAlloc (clipboard)", None))
            } else {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
                GlobalUnlock(memory);
                EmptyClipboard();
                // On success the system owns the memory.
                if SetClipboardData(CF_DIB, memory).is_null() {
                    GlobalFree(memory);
                    Err(failed("SetClipboardData (screenshot)", None))
                } else {
                    Ok(())
                }
            }
        };
        unsafe { CloseClipboard() };
        result
    };
    unsafe { DestroyWindow(owner) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn image() -> Image {
        Image {
            width: 3,
            height: 2,
            pixels: (0..24).collect(),
        }
    }

    #[test]
    fn png_is_written_with_the_image_size() {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/screenshot-tests")
            .join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&directory);
        let path = directory.join("nested dir").join("shot.png");
        save_png(&image(), &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        // IHDR: big-endian width and height right after the chunk type.
        assert_eq!(&bytes[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 3);
        assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 2);
        std::fs::remove_dir_all(directory).unwrap();
        assert!(save_png(&image(), Path::new("Z:\\:invalid\\shot.png")).is_err());
    }

    #[test]
    fn clipboard_dib_is_bottom_up_after_the_header() {
        let bytes = dib_bytes(&image());
        let header = size_of::<BITMAPINFOHEADER>();
        assert_eq!(bytes.len(), header + 24);
        assert_eq!(i32::from_le_bytes(bytes[8..12].try_into().unwrap()), 2);
        assert_eq!(
            &bytes[header..header + 12],
            &(12..24).collect::<Vec<u8>>()[..]
        );
        assert_eq!(&bytes[header + 12..], &(0..12).collect::<Vec<u8>>()[..]);
    }
}
