//! Column boundary drags. Invisible topmost strips sit on the boundaries between columns
//! (and on screen edges with a hidden column behind them). Dragging one shows a full-screen
//! preview of the resulting split; real windows change only once, on release.
//! Own thread and message loop; the controller only publishes strips and drains drops.
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
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

use crate::{
    layout::edges::{drag_edge, edge_position},
    model::{AppError, ErrorCode, Rect},
};

/// One draggable boundary on a monitor's active page.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub monitor_id: String,
    /// Boundary index on the active page (0 = left of the first column).
    pub index: usize,
    /// Screen strip that grabs the pointer.
    pub hit: Rect,
    /// Screen area the preview covers; column offsets start at `area.x`.
    pub area: Rect,
    pub widths: Vec<u32>,
    /// Row heights per column, for the preview only.
    pub rows: Vec<Vec<u32>>,
    pub scroll: i64,
    pub snap: i64,
    pub scale: f64,
}

pub struct Drop {
    pub monitor_id: String,
    pub index: usize,
    pub delta: i32,
}

type Wake = Box<dyn Fn() + Send>;

struct Shared {
    edges: Mutex<Vec<Edge>>,
    drops: Mutex<Vec<Drop>>,
    wake: Mutex<Option<Wake>>,
    thread: Mutex<u32>,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        edges: Mutex::new(vec![]),
        drops: Mutex::new(vec![]),
        wake: Mutex::new(None),
        thread: Mutex::new(0),
    })
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Publish the strips to show; a no-op when nothing changed.
pub fn configure(edges: Vec<Edge>) {
    let shared = shared();
    let mut current = lock(&shared.edges);
    if *current == edges {
        return;
    }
    *current = edges;
    let thread = *lock(&shared.thread);
    if thread != 0 {
        unsafe {
            PostThreadMessageW(thread, WM_APP, 0, 0);
        }
    }
}

pub fn drain() -> Vec<Drop> {
    std::mem::take(&mut *lock(&shared().drops))
}

struct Drag {
    edge: Edge,
    start: i32,
    delta: i64,
}

struct State {
    strips: Vec<HWND>,
    overlay: HWND,
    drag: Option<Drag>,
    /// Edges the visible strips were built from (the strip index is the edge index).
    shown: Vec<Edge>,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State {
        strips: vec![],
        overlay: null_mut(),
        drag: None,
        shown: vec![],
    }) };
}

const STRIP_CLASS: &str = "e-desktop-splitter";
const OVERLAY_CLASS: &str = "e-desktop-split-preview";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn cursor_x() -> i32 {
    let mut p = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut p);
    }
    p.x
}

/// Rebuild strips from the published edges (deferred while a drag is running).
fn sync() {
    let edges = lock(&shared().edges).clone();
    STATE.with_borrow_mut(|state| {
        if state.drag.is_some() {
            return;
        }
        let class = wide(STRIP_CLASS);
        while state.strips.len() < edges.len() {
            let h = unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
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
            // Alpha 1 keeps the strip invisible but hit-testable (alpha 0 is click-through).
            unsafe {
                SetLayeredWindowAttributes(h, 0, 1, LWA_ALPHA);
                SetWindowLongPtrW(h, GWLP_USERDATA, state.strips.len() as isize);
            }
            state.strips.push(h);
        }
        for (i, &h) in state.strips.iter().enumerate() {
            match edges.get(i) {
                Some(edge) => unsafe {
                    SetWindowPos(
                        h,
                        HWND_TOPMOST,
                        edge.hit.x,
                        edge.hit.y,
                        edge.hit.width as i32,
                        edge.hit.height as i32,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                },
                None => unsafe {
                    ShowWindow(h, SW_HIDE);
                },
            }
        }
        state.shown = edges;
    });
}

fn begin(strip: HWND) {
    let index = unsafe { GetWindowLongPtrW(strip, GWLP_USERDATA) } as usize;
    let started = STATE.with_borrow_mut(|state| {
        let edge = state.shown.get(index)?.clone();
        let area = edge.area;
        state.drag = Some(Drag {
            edge,
            start: cursor_x(),
            delta: 0,
        });
        Some((state.overlay, area))
    });
    let Some((overlay, area)) = started else {
        return;
    };
    unsafe {
        SetCapture(strip);
        SetWindowPos(
            overlay,
            HWND_TOPMOST,
            area.x,
            area.y,
            area.width as i32,
            area.height as i32,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        InvalidateRect(overlay, null(), 0);
    }
}

fn update() {
    let overlay = STATE.with_borrow_mut(|state| {
        let drag = state.drag.as_mut()?;
        let delta = i64::from(cursor_x()) - i64::from(drag.start);
        (delta != drag.delta).then(|| {
            drag.delta = delta;
            state.overlay
        })
    });
    if let Some(overlay) = overlay {
        unsafe {
            InvalidateRect(overlay, null(), 0);
            UpdateWindow(overlay);
        }
    }
}

/// End the drag; `commit` queues the drop for the controller.
fn finish(commit: bool) {
    let Some((drag, overlay)) =
        STATE.with_borrow_mut(|state| state.drag.take().map(|d| (d, state.overlay)))
    else {
        return;
    };
    unsafe {
        ShowWindow(overlay, SW_HIDE);
        ReleaseCapture();
    }
    if commit && drag.delta != 0 {
        lock(&shared().drops).push(Drop {
            monitor_id: drag.edge.monitor_id,
            index: drag.edge.index,
            delta: drag.delta.clamp(i32::MIN.into(), i32::MAX.into()) as i32,
        });
        if let Some(wake) = &*lock(&shared().wake) {
            wake();
        }
    }
    sync(); // Strips published while dragging are applied now.
}

unsafe extern "system" fn strip_proc(h: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_LBUTTONDOWN => {
            begin(h);
            0
        }
        WM_MOUSEMOVE => {
            update();
            0
        }
        WM_LBUTTONUP => {
            update();
            finish(true);
            0
        }
        WM_CAPTURECHANGED => {
            finish(false);
            0
        }
        _ => unsafe { DefWindowProcW(h, message, w, l) },
    }
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

/// The split after the drag: one rectangle per visible window, labelled with its column's share.
fn paint(dc: HDC, size: RECT, drag: &Drag) {
    let edge = &drag.edge;
    let view = edge.area.width;
    let (widths, scroll) = drag_edge(
        &edge.widths,
        view,
        edge.scroll,
        edge.index,
        drag.delta,
        edge.snap,
    );
    fill(dc, size, rgb(20, 20, 22));
    let inset = (4.0 * edge.scale).round() as i32;
    let border = ((2.0 * edge.scale).round() as i32).max(1);
    let face = wide("Segoe UI");
    let font = unsafe {
        CreateFontW(
            -(22.0 * edge.scale).round() as i32,
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            DEFAULT_CHARSET.into(),
            OUT_DEFAULT_PRECIS.into(),
            CLIP_DEFAULT_PRECIS.into(),
            CLEARTYPE_QUALITY.into(),
            0,
            face.as_ptr(),
        )
    };
    let previous = unsafe { SelectObject(dc, font) };
    unsafe {
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, rgb(230, 230, 230));
    }
    let height = i64::from(size.bottom - size.top);
    for (c, &width) in widths.iter().enumerate() {
        let left = edge_position(&widths, scroll, c);
        let right = left + i64::from(width);
        if right <= 0 || left >= i64::from(view) {
            continue;
        }
        let active = c + 1 == edge.index || c == edge.index;
        let weights: Vec<i64> = match edge.rows.get(c) {
            Some(rows) if !rows.is_empty() => rows.iter().map(|&h| i64::from(h.max(1))).collect(),
            _ => vec![1],
        };
        let total: i64 = weights.iter().sum();
        let mut top = 0i64;
        let mut acc = 0i64;
        for (r, weight) in weights.iter().enumerate() {
            acc += weight;
            let bottom = height * acc / total;
            let outer = RECT {
                left: left.max(0) as i32 + inset,
                top: top as i32 + inset,
                right: right.min(i64::from(view)) as i32 - inset,
                bottom: bottom as i32 - inset,
            };
            top = bottom;
            if outer.right <= outer.left || outer.bottom <= outer.top {
                continue;
            }
            let (line, body) = if active {
                (rgb(127, 200, 255), rgb(40, 58, 74))
            } else {
                (rgb(80, 84, 90), rgb(42, 44, 48))
            };
            fill(dc, outer, line);
            let mut inner = RECT {
                left: outer.left + border,
                top: outer.top + border,
                right: outer.right - border,
                bottom: outer.bottom - border,
            };
            fill(dc, inner, body);
            if r == 0 {
                // Share of the screen it covers: a column peeking in counts its visible part.
                let visible = (right.min(i64::from(view)) - left.max(0)) as u64;
                let percent = (visible * 100 + u64::from(view) / 2) / u64::from(view.max(1));
                let mut label: Vec<u16> = format!("{percent}%").encode_utf16().collect();
                unsafe {
                    DrawTextW(
                        dc,
                        label.as_mut_ptr(),
                        label.len() as i32,
                        &mut inner,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );
                }
            }
        }
    }
    unsafe {
        SelectObject(dc, previous);
        DeleteObject(font);
    }
}

unsafe extern "system" fn overlay_proc(h: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_PAINT => unsafe {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(h, &mut ps);
            let mut size: RECT = zeroed();
            GetClientRect(h, &mut size);
            // Double buffer: the preview repaints on every pointer move.
            let memory = CreateCompatibleDC(dc);
            let bitmap = CreateCompatibleBitmap(dc, size.right, size.bottom);
            let old = SelectObject(memory, bitmap);
            STATE.with_borrow(|state| {
                if let Some(drag) = &state.drag {
                    paint(memory, size, drag);
                }
            });
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

/// Owns the splitter thread; dropping it closes every strip.
pub struct Splitter {
    thread: u32,
    handle: Option<JoinHandle<()>>,
}

pub fn start(wake: Wake) -> Result<Splitter, AppError> {
    let failure = |message: String| AppError {
        code: ErrorCode::BackendUnavailable,
        message,
        window_id: None,
    };
    *lock(&shared().wake) = Some(wake);
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("column-splitter".into())
        .spawn(move || unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let mut message: MSG = zeroed();
            PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
            let instance = GetModuleHandleW(null());
            let strip = wide(STRIP_CLASS);
            let overlay = wide(OVERLAY_CLASS);
            let classes: [(&Vec<u16>, WNDPROC, windows_sys::core::PCWSTR); 2] = [
                (&strip, Some(strip_proc), IDC_SIZEWE),
                (&overlay, Some(overlay_proc), IDC_ARROW),
            ];
            for (name, procedure, cursor) in classes {
                let class = WNDCLASSW {
                    lpfnWndProc: procedure,
                    hInstance: instance,
                    hCursor: LoadCursorW(null_mut(), cursor),
                    lpszClassName: name.as_ptr(),
                    ..zeroed()
                };
                RegisterClassW(&class);
            }
            let preview = CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT,
                overlay.as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            if preview.is_null() {
                let _ = tx.send(Err(GetLastError()));
                return;
            }
            SetLayeredWindowAttributes(preview, 0, 240, LWA_ALPHA);
            STATE.with_borrow_mut(|state| state.overlay = preview);
            *lock(&shared().thread) = GetCurrentThreadId();
            let _ = tx.send(Ok(GetCurrentThreadId()));
            sync();
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                if message.hwnd.is_null() && message.message == WM_APP {
                    sync();
                    continue;
                }
                DispatchMessageW(&message);
            }
            STATE.with_borrow_mut(|state| {
                for &h in state.strips.iter().chain([&state.overlay]) {
                    DestroyWindow(h);
                }
            });
        })
        .map_err(|e| failure(e.to_string()))?;
    match rx.recv() {
        Ok(Ok(thread)) => Ok(Splitter {
            thread,
            handle: Some(handle),
        }),
        Ok(Err(code)) => Err(failure(format!(
            "列分界线窗口创建失败（Win32 {code}），拖动分界线不可用。"
        ))),
        Err(_) => Err(failure("列分界线线程意外退出。".into())),
    }
}

impl std::ops::Drop for Splitter {
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
