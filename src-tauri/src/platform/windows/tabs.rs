//! lane: tabbed. Tab indicators: one small native strip per tabbed column shown on screen, in
//! the space the layout reserves above that column's window. Segments show how many tabs the
//! column has and which one is shown; a click focuses that tab. The strips are not topmost,
//! never activate and cover no window content, so they cannot take input meant for a window or
//! steal focus. Own thread and message loop; the controller publishes the bars to show.
use std::{
    cell::RefCell,
    mem::zeroed,
    ptr::{null, null_mut},
    sync::{Mutex, OnceLock, mpsc},
    thread::JoinHandle,
};

use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{HiDpi::*, WindowsAndMessaging::*},
};

use crate::{
    layout::tabbed::{TabBar, tab_at},
    model::{AppError, Command, ErrorCode},
};

/// Hands a clicked tab's focus command to the controller.
type Submit = Box<dyn Fn(Command) + Send>;

struct Shared {
    bars: Mutex<Vec<TabBar>>,
    submit: Mutex<Option<Submit>>,
    thread: Mutex<u32>,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        bars: Mutex::new(vec![]),
        submit: Mutex::new(None),
        thread: Mutex::new(0),
    })
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

const SYNC: u32 = WM_APP;
const CLASS: &str = "e-desktop-tab-indicator";

struct State {
    windows: Vec<HWND>,
    /// Bars the visible windows show (the window index is the bar index).
    shown: Vec<TabBar>,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State { windows: vec![], shown: vec![] }) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Publish the indicators to show; a no-op when nothing changed.
pub fn configure(bars: Vec<TabBar>) {
    let mut current = lock(&shared().bars);
    if *current != bars {
        *current = bars;
        drop(current);
        let thread = *lock(&shared().thread);
        if thread != 0 {
            unsafe {
                PostThreadMessageW(thread, SYNC, 0, 0);
            }
        }
    }
}

/// Native calls run outside the state borrow: they send messages to `tab_proc`.
fn sync() {
    let bars = lock(&shared().bars).clone();
    let class = wide(CLASS);
    while STATE.with_borrow(|state| state.windows.len()) < bars.len() {
        let h = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                GetModuleHandleW(null()),
                null(),
            )
        };
        if h.is_null() {
            break;
        }
        unsafe {
            SetWindowLongPtrW(
                h,
                GWLP_USERDATA,
                STATE.with_borrow(|s| s.windows.len()) as isize,
            );
        }
        STATE.with_borrow_mut(|state| state.windows.push(h));
    }
    let windows = STATE.with_borrow_mut(|state| {
        state.shown = bars.clone();
        state.windows.clone()
    });
    for (i, h) in windows.into_iter().enumerate() {
        match bars.get(i) {
            Some(bar) => unsafe {
                let v = bar.visible;
                SetWindowPos(
                    h,
                    null_mut(),
                    v.x,
                    v.y,
                    v.width as i32,
                    v.height as i32,
                    SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
                );
                InvalidateRect(h, null(), 0);
            },
            None => unsafe {
                ShowWindow(h, SW_HIDE);
            },
        }
    }
}

fn bar_of(h: HWND) -> Option<TabBar> {
    let index = unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) } as usize;
    STATE.with_borrow(|state| state.shown.get(index).cloned())
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16)
}

fn fill(dc: HDC, r: RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, &r, brush);
        DeleteObject(brush);
    }
}

/// One segment per tab across the whole strip (client coordinates of the visible part).
fn paint(dc: HDC, size: RECT, bar: &TabBar) {
    fill(dc, size, rgb(25, 27, 28));
    let px = |v: f64| ((v * bar.scale).round() as i64).max(1);
    let (gap, pad) = (px(2.0), px(3.0));
    let count = bar.tabs.len() as i64;
    let width = i64::from(bar.rect.width);
    let origin = i64::from(bar.rect.x) - i64::from(bar.visible.x);
    for i in 0..count {
        let left = origin + width * i / count + if i > 0 { gap / 2 } else { 0 };
        let right =
            origin + width * (i + 1) / count - if i + 1 < count { gap - gap / 2 } else { 0 };
        let color = if i as usize == bar.active {
            rgb(127, 200, 255)
        } else {
            rgb(80, 84, 90)
        };
        let segment = RECT {
            left: left.clamp(0, size.right.into()) as i32,
            top: pad.min(i64::from(size.bottom) / 2) as i32,
            right: right.clamp(0, size.right.into()) as i32,
            bottom: (i64::from(size.bottom) - pad).max(i64::from(size.bottom) / 2 + 1) as i32,
        };
        if segment.right > segment.left {
            fill(dc, segment, color);
        }
    }
}

unsafe extern "system" fn tab_proc(h: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_LBUTTONDOWN => {
            let x = (l & 0xffff) as u16 as i16 as i32;
            if let Some(bar) = bar_of(h) {
                if let Some(window_id) = tab_at(&bar, bar.visible.x.saturating_add(x))
                    .and_then(|tab| bar.tabs.get(tab).cloned())
                {
                    if let Some(submit) = &*lock(&shared().submit) {
                        submit(Command::FocusWindow { window_id });
                    }
                }
            }
            0
        }
        WM_PAINT => unsafe {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(h, &mut ps);
            let mut size: RECT = zeroed();
            GetClientRect(h, &mut size);
            let memory = CreateCompatibleDC(dc);
            let bitmap = CreateCompatibleBitmap(dc, size.right, size.bottom);
            let old = SelectObject(memory, bitmap);
            match bar_of(h) {
                Some(bar) => paint(memory, size, &bar),
                None => fill(memory, size, rgb(25, 27, 28)),
            }
            BitBlt(dc, 0, 0, size.right, size.bottom, memory, 0, 0, SRCCOPY);
            SelectObject(memory, old);
            DeleteObject(bitmap);
            DeleteDC(memory);
            EndPaint(h, &ps);
            0
        },
        _ => unsafe { DefWindowProcW(h, message, w, l) },
    }
}

/// Owns the indicator thread; dropping it closes every indicator.
pub struct TabIndicators {
    thread: u32,
    handle: Option<JoinHandle<()>>,
}

pub fn start(submit: Submit) -> Result<TabIndicators, AppError> {
    let failure = |message: String| AppError {
        code: ErrorCode::BackendUnavailable,
        message,
        window_id: None,
    };
    *lock(&shared().submit) = Some(submit);
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("tab-indicators".into())
        .spawn(move || unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let mut message: MSG = zeroed();
            PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
            let class = wide(CLASS);
            let registered = RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(tab_proc),
                hInstance: GetModuleHandleW(null()),
                hCursor: LoadCursorW(null_mut(), IDC_HAND),
                lpszClassName: class.as_ptr(),
                ..zeroed()
            });
            if registered == 0 {
                let _ = tx.send(Err(GetLastError()));
                return;
            }
            *lock(&shared().thread) = GetCurrentThreadId();
            let _ = tx.send(Ok(GetCurrentThreadId()));
            sync();
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                if message.hwnd.is_null() {
                    if message.message == SYNC {
                        sync();
                    }
                    continue;
                }
                DispatchMessageW(&message);
            }
            for h in STATE.with_borrow_mut(|state| std::mem::take(&mut state.windows)) {
                DestroyWindow(h);
            }
        })
        .map_err(|e| failure(e.to_string()))?;
    match rx.recv() {
        Ok(Ok(thread)) => Ok(TabIndicators {
            thread,
            handle: Some(handle),
        }),
        Ok(Err(code)) => Err(failure(format!(
            "标签指示器窗口类注册失败（Win32 {code}），标签列不显示指示器。"
        ))),
        Err(_) => Err(failure("标签指示器线程意外退出。".into())),
    }
}

impl Drop for TabIndicators {
    fn drop(&mut self) {
        *lock(&shared().thread) = 0;
        unsafe {
            PostThreadMessageW(self.thread, WM_QUIT, 0, 0);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
