//! Pointer drags with a live preview. Invisible topmost strips sit on the boundaries between
//! columns (and on screen edges with a hidden column behind them) and between stacked
//! windows. Dragging a strip, a tiled window with the drag modifier, or a tiled window by its
//! own title bar covers the target monitor with a preview of the resulting layout: the
//! engine's result for the very command a release applies. Real windows change only once,
//! on release. Own thread and message loop; the controller publishes strips and the layout,
//! starts/ends window drags and drains the resulting commands.
use std::{
    cell::RefCell,
    collections::VecDeque,
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
    layout::{
        Engine,
        edges::edge_position,
        expand_gap, half_gap,
        scene::{Mark, Scene},
    },
    model::{AppError, Command, ErrorCode, Rect, WindowId},
};

/// One draggable boundary on a monitor's active page.
#[derive(Debug, Clone, PartialEq)]
pub struct Strip {
    pub monitor_id: String,
    /// Screen strip that grabs the pointer.
    pub hit: Rect,
    /// Column index for a boundary between stacked rows; None for a column boundary.
    pub column: Option<usize>,
    /// Between columns (or rows) `edge - 1` and `edge`.
    pub edge: usize,
}

/// A finished drag: the command to apply.
pub struct Drop {
    pub command: Command,
    /// A window carried by its own title bar is no longer where it was last placed.
    pub carried: Option<WindowId>,
}

type Wake = Box<dyn Fn() + Send>;

enum Request {
    Begin {
        window_id: WindowId,
        /// The window is in the system move loop (title bar drag), not a modifier drag.
        native: Option<usize>,
    },
    End {
        point: Option<(i32, i32)>,
        commit: bool,
    },
}

struct Shared {
    strips: Mutex<Vec<Strip>>,
    layout: Mutex<Option<Engine>>,
    requests: Mutex<VecDeque<Request>>,
    drops: Mutex<Vec<Drop>>,
    wake: Mutex<Option<Wake>>,
    thread: Mutex<u32>,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared {
        strips: Mutex::new(vec![]),
        layout: Mutex::new(None),
        requests: Mutex::new(VecDeque::new()),
        drops: Mutex::new(vec![]),
        wake: Mutex::new(None),
        thread: Mutex::new(0),
    })
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

const SYNC: u32 = WM_APP;
const REQUEST: u32 = WM_APP + 1;

fn post(message: u32) {
    let thread = *lock(&shared().thread);
    if thread != 0 {
        unsafe {
            PostThreadMessageW(thread, message, 0, 0);
        }
    }
}

/// Strips on the column boundaries and between stacked windows of every active page, none
/// while paused or where a layout fullscreen window covers the page.
pub fn strips(engine: &Engine) -> Vec<Strip> {
    let snapshot = engine.snapshot();
    let mut strips = Vec::new();
    for monitor in snapshot.monitors.iter().filter(|_| snapshot.enabled) {
        let Some(page) = monitor.pages.iter().find(|p| p.id == monitor.active_page) else {
            continue;
        };
        let fullscreen = snapshot.windows.iter().any(|w| {
            w.fullscreen
                && page
                    .columns
                    .iter()
                    .any(|c| c.windows.contains(&w.native.id))
        });
        if fullscreen || page.columns.is_empty() {
            continue;
        }
        let scale = monitor.monitor.scale_factor;
        let inner = monitor.viewport;
        let outer = expand_gap(inner, half_gap(snapshot.gaps, scale));
        let widths: Vec<u32> = page.columns.iter().map(|c| c.width).collect();
        let view = i64::from(inner.width);
        let x = i64::from(page.viewport_x);
        // As wide as the native resize border on both sides of the boundary.
        let size = ((12.0 * scale).round() as i64).max(8);
        let (left, right) = (
            i64::from(outer.x),
            i64::from(outer.x) + i64::from(outer.width),
        );
        for edge in 0..=widths.len() {
            let pos = edge_position(&widths, x, edge);
            // Shared boundaries, plus screen edges hiding a column behind them.
            let shown = (pos > 0 && pos < view)
                || (pos == 0 && edge > 0)
                || (pos == view && edge < widths.len());
            if !shown {
                continue;
            }
            let start = (i64::from(inner.x) + pos - size / 2).clamp(left, right - size);
            strips.push(Strip {
                monitor_id: monitor.monitor.id.clone(),
                hit: Rect {
                    x: start as i32,
                    y: outer.y,
                    width: size as u32,
                    height: outer.height,
                },
                column: None,
                edge,
            });
        }
        // Boundaries between stacked windows, across the visible part of their column and
        // clear of the column boundary strips.
        for (c, column) in page.columns.iter().enumerate() {
            let from = (i64::from(inner.x) + edge_position(&widths, x, c) + size / 2).max(left);
            let to = (i64::from(inner.x) + edge_position(&widths, x, c + 1) - size / 2).min(right);
            if column.windows.len() < 2 || to <= from {
                continue;
            }
            let mut y = i64::from(inner.y);
            let heights = engine.column_heights(column, inner.height);
            for (row, height) in heights.into_iter().enumerate().take(column.windows.len() - 1) {
                y += i64::from(height);
                strips.push(Strip {
                    monitor_id: monitor.monitor.id.clone(),
                    hit: Rect {
                        x: from as i32,
                        y: (y - size / 2) as i32,
                        width: (to - from) as u32,
                        height: size as u32,
                    },
                    column: Some(c),
                    edge: row + 1,
                });
            }
        }
    }
    strips
}

/// Publish the strips to show; a no-op when nothing changed.
pub fn configure(strips: Vec<Strip>) {
    let mut current = lock(&shared().strips);
    if *current != strips {
        *current = strips;
        drop(current);
        post(SYNC);
    }
}

/// The layout drags start from and preview against.
pub fn publish(engine: Engine) {
    *lock(&shared().layout) = Some(engine);
}

/// Start previewing a drop of `window_id` under the pointer. `native` is the window's handle
/// when the system move loop carries it (title bar drag).
pub fn begin_window(window_id: WindowId, native: Option<usize>) {
    lock(&shared().requests).push_back(Request::Begin { window_id, native });
    post(REQUEST);
}

/// Finish a window drag at `point` (or the pointer position); `commit` queues its drop.
pub fn end_window(point: Option<(i32, i32)>, commit: bool) {
    lock(&shared().requests).push_back(Request::End { point, commit });
    post(REQUEST);
}

pub fn drain() -> Vec<Drop> {
    std::mem::take(&mut *lock(&shared().drops))
}

enum Kind {
    Strip(Strip),
    Window {
        id: WindowId,
        /// Title bar drag: the window and its rectangle when the loop started.
        native: Option<(HWND, RECT)>,
        moved: bool,
    },
}

struct Session {
    engine: Engine,
    kind: Kind,
    active: Vec<WindowId>,
    start: POINT,
    point: Option<POINT>,
    /// What a release applies: the command previewed for the last pointer position.
    command: Option<Command>,
    scene: Option<Scene>,
}

struct State {
    strips: Vec<HWND>,
    overlay: HWND,
    session: Option<Session>,
    /// Strips the visible strip windows were built from (the window index is the strip index).
    shown: Vec<Strip>,
    /// Where the overlay currently is; None while hidden.
    area: Option<Rect>,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State {
        strips: vec![],
        overlay: null_mut(),
        session: None,
        shown: vec![],
        area: None,
    }) };
}

const STRIP_CLASS: &str = "e-desktop-splitter";
const OVERLAY_CLASS: &str = "e-desktop-split-preview";
const TIMER: usize = 1;
/// Pointer travel before a modifier drag starts previewing (a plain Alt+click does nothing).
const THRESHOLD: i32 = 8;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn cursor() -> POINT {
    let mut p = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut p);
    }
    p
}

/// Rebuild strips from the published ones (deferred while a drag is running).
fn sync() {
    let strips = lock(&shared().strips).clone();
    STATE.with_borrow_mut(|state| {
        if state.session.is_some() {
            return;
        }
        let class = wide(STRIP_CLASS);
        while state.strips.len() < strips.len() {
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
            match strips.get(i) {
                Some(strip) => unsafe {
                    SetWindowPos(
                        h,
                        HWND_TOPMOST,
                        strip.hit.x,
                        strip.hit.y,
                        strip.hit.width as i32,
                        strip.hit.height as i32,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                },
                None => unsafe {
                    ShowWindow(h, SW_HIDE);
                },
            }
        }
        state.shown = strips;
    });
}

fn layout() -> Option<Engine> {
    lock(&shared().layout).clone()
}

/// Windows on either side of a strip, highlighted in the preview.
fn neighbours(engine: &Engine, strip: &Strip) -> Vec<WindowId> {
    let snapshot = engine.snapshot();
    let Some(page) = snapshot
        .monitors
        .iter()
        .find(|m| m.monitor.id == strip.monitor_id)
        .and_then(|m| m.pages.iter().find(|p| p.id == m.active_page))
    else {
        return vec![];
    };
    let near = |i: usize| i + 1 >= strip.edge && i <= strip.edge;
    match strip.column {
        None => page
            .columns
            .iter()
            .enumerate()
            .filter(|(i, _)| near(*i))
            .flat_map(|(_, c)| c.windows.clone())
            .collect(),
        Some(c) => page.columns.get(c).map_or(vec![], |c| {
            c.windows
                .iter()
                .enumerate()
                .filter(|(i, _)| near(*i))
                .map(|(_, id)| id.clone())
                .collect()
        }),
    }
}

fn begin_strip(strip: HWND, start: POINT) {
    let index = unsafe { GetWindowLongPtrW(strip, GWLP_USERDATA) } as usize;
    let Some(engine) = layout() else { return };
    let started = STATE.with_borrow_mut(|state| {
        if state.session.is_some() {
            return false;
        }
        let Some(target) = state.shown.get(index).cloned() else {
            return false;
        };
        state.session = Some(Session {
            active: neighbours(&engine, &target),
            engine,
            kind: Kind::Strip(target),
            start,
            point: None,
            command: None,
            scene: None,
        });
        true
    });
    if started {
        unsafe {
            SetCapture(strip);
        }
        update(Some(start));
    }
}

/// Screen position of a strip's mouse message (client coordinates, negative while captured
/// outside the strip).
fn message_point(h: HWND, l: LPARAM) -> POINT {
    let mut p = POINT {
        x: (l & 0xffff) as u16 as i16 as i32,
        y: ((l >> 16) & 0xffff) as u16 as i16 as i32,
    };
    unsafe {
        ClientToScreen(h, &mut p);
    }
    p
}

fn start_window(window_id: WindowId, native: Option<usize>) {
    let Some(engine) = layout() else { return };
    let native = native.and_then(|h| {
        let h = h as HWND;
        let mut r: RECT = unsafe { zeroed() };
        (unsafe { GetWindowRect(h, &mut r) } != 0).then_some((h, r))
    });
    let overlay = STATE.with_borrow_mut(|state| {
        if state.session.is_some() {
            return None;
        }
        state.session = Some(Session {
            engine,
            active: vec![window_id.clone()],
            kind: Kind::Window {
                id: window_id,
                native,
                moved: false,
            },
            start: cursor(),
            point: None,
            command: None,
            scene: None,
        });
        Some(state.overlay)
    });
    if let Some(overlay) = overlay {
        unsafe {
            SetTimer(overlay, TIMER, 15, None);
        }
    }
}

fn size(r: RECT) -> (i32, i32) {
    (r.right - r.left, r.bottom - r.top)
}

enum Step {
    Wait,
    Cancel,
    /// Repaint; `Some` when the overlay must move to (or hide from) a new area first.
    Show(Option<Option<Rect>>),
}

/// Preview the command for the pointer at `point` (default: the cursor).
fn update(point: Option<POINT>) {
    let point = point.unwrap_or_else(cursor);
    let step = STATE.with_borrow_mut(|state| {
        let Some(session) = state.session.as_mut() else {
            return Step::Wait;
        };
        if session.point.is_some_and(|p| (p.x, p.y) == (point.x, point.y)) {
            return Step::Wait;
        }
        let (dx, dy) = (point.x - session.start.x, point.y - session.start.y);
        let (command, monitor) = match &mut session.kind {
            Kind::Strip(strip) => (
                match strip.column {
                    None => Command::DragEdge {
                        monitor_id: strip.monitor_id.clone(),
                        edge: strip.edge as u32,
                        delta: dx,
                    },
                    Some(column) => Command::DragRow {
                        monitor_id: strip.monitor_id.clone(),
                        column: column as u32,
                        edge: strip.edge as u32,
                        delta: dy,
                    },
                },
                strip.monitor_id.clone(),
            ),
            Kind::Window { id, native, moved } => {
                if let Some((h, initial)) = native.as_ref().filter(|_| !*moved) {
                    // The system loop also resizes by the border: only a move is a drop. Decide
                    // on the first change; afterwards the app resizes itself when it crosses
                    // into a monitor with another scale (WM_DPICHANGED), still a move.
                    let mut now: RECT = unsafe { zeroed() };
                    if unsafe { GetWindowRect(*h, &mut now) } == 0 || size(now) != size(*initial)
                    {
                        return Step::Cancel;
                    }
                    if (now.left, now.top) == (initial.left, initial.top) {
                        return Step::Wait;
                    }
                } else if !*moved && dx.abs().max(dy.abs()) < THRESHOLD {
                    return Step::Wait;
                }
                *moved = true;
                let (x, y) = (i64::from(point.x), i64::from(point.y));
                let Some(monitor) = session.engine.snapshot().monitors.iter().find(|m| {
                    let b = m.monitor.bounds;
                    x >= b.x.into()
                        && y >= b.y.into()
                        && x < i64::from(b.x) + i64::from(b.width)
                        && y < i64::from(b.y) + i64::from(b.height)
                }) else {
                    return Step::Wait;
                };
                (
                    Command::DropWindow {
                        window_id: id.clone(),
                        x: point.x,
                        y: point.y,
                    },
                    monitor.monitor.id.clone(),
                )
            }
        };
        session.point = Some(point);
        let scene = session
            .engine
            .scene(command.clone(), &monitor, &session.active);
        session.command = scene.is_some().then_some(command);
        if scene == session.scene {
            return Step::Wait; // Snapped or unchanged: nothing new to paint.
        }
        session.scene = scene;
        let area = session.scene.as_ref().map(|s| s.area);
        let moved = (area != state.area).then_some(area);
        state.area = area;
        Step::Show(moved)
    });
    let overlay = STATE.with_borrow(|state| state.overlay);
    match step {
        Step::Wait => {}
        Step::Cancel => finish(false),
        Step::Show(moved) => unsafe {
            match moved {
                Some(Some(area)) => {
                    SetWindowPos(
                        overlay,
                        HWND_TOPMOST,
                        area.x,
                        area.y,
                        area.width as i32,
                        area.height as i32,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
                Some(None) => {
                    ShowWindow(overlay, SW_HIDE);
                }
                None => {}
            }
            InvalidateRect(overlay, null(), 0);
            UpdateWindow(overlay);
        },
    }
}

/// End the drag; `commit` queues the previewed command for the controller.
fn finish(commit: bool) {
    let Some((session, overlay)) =
        STATE.with_borrow_mut(|state| {
            state.area = None;
            state.session.take().map(|s| (s, state.overlay))
        })
    else {
        return;
    };
    unsafe {
        KillTimer(overlay, TIMER);
        ShowWindow(overlay, SW_HIDE);
    }
    let carried = match session.kind {
        Kind::Strip(_) => {
            unsafe {
                ReleaseCapture();
            }
            None
        }
        Kind::Window { id, native, .. } => native.map(|_| id),
    };
    // A click on a strip without moving changes nothing.
    let moved = |c: &Command| {
        !matches!(
            c,
            Command::DragEdge { delta: 0, .. } | Command::DragRow { delta: 0, .. }
        )
    };
    if let Some(command) = session.command.filter(|c| commit && moved(c)) {
        lock(&shared().drops).push(Drop { command, carried });
        if let Some(wake) = &*lock(&shared().wake) {
            wake();
        }
    }
    sync(); // Strips published while dragging are applied now.
}

fn requests() {
    loop {
        let Some(request) = lock(&shared().requests).pop_front() else {
            return;
        };
        match request {
            Request::Begin { window_id, native } => start_window(window_id, native),
            Request::End { point, commit } => {
                let window = STATE.with_borrow(|state| {
                    matches!(state.session, Some(Session { kind: Kind::Window { .. }, .. }))
                });
                if window {
                    if commit {
                        update(point.map(|(x, y)| POINT { x, y }));
                    }
                    finish(commit);
                }
            }
        }
    }
}

unsafe extern "system" fn strip_proc(h: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_SETCURSOR => {
            let index = unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) } as usize;
            let rows = STATE.with_borrow(|state| {
                state.shown.get(index).is_some_and(|s| s.column.is_some())
            });
            unsafe {
                SetCursor(LoadCursorW(null_mut(), if rows { IDC_SIZENS } else { IDC_SIZEWE }));
            }
            1
        }
        WM_LBUTTONDOWN => {
            begin_strip(h, message_point(h, l));
            0
        }
        WM_MOUSEMOVE => {
            update(Some(message_point(h, l)));
            0
        }
        WM_LBUTTONUP => {
            update(Some(message_point(h, l)));
            finish(true);
            0
        }
        WM_CAPTURECHANGED => {
            let strip = STATE.with_borrow(|state| {
                matches!(state.session, Some(Session { kind: Kind::Strip(_), .. }))
            });
            if strip {
                finish(false);
            }
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

fn font(scale: f64, size: f64, weight: i32) -> HFONT {
    let face = wide("Segoe UI");
    unsafe {
        CreateFontW(
            -(size * scale).round() as i32,
            0,
            0,
            0,
            weight,
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
    }
}

fn text(dc: HDC, value: &str, mut r: RECT, format: DRAW_TEXT_FORMAT) {
    let mut value: Vec<u16> = value.encode_utf16().collect();
    unsafe {
        DrawTextW(
            dc,
            value.as_mut_ptr(),
            value.len() as i32,
            &mut r,
            format | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
    }
}

fn colors(active: bool) -> (COLORREF, COLORREF) {
    if active {
        (rgb(127, 200, 255), rgb(40, 58, 74))
    } else {
        (rgb(80, 84, 90), rgb(42, 44, 48))
    }
}

/// The layout after the drag: one box per visible window with its title and screen share,
/// plus a tab at the screen edge for each window pushed or queued just past it.
fn paint(dc: HDC, size: RECT, scene: &Scene) {
    fill(dc, size, rgb(20, 20, 22));
    let s = scene.scale;
    let px = |v: f64| ((v * s).round() as i32).max(1);
    let (inset, border, pad) = (px(4.0), px(2.0), px(10.0));
    let big = font(s, 22.0, 600);
    let small = font(s, 13.0, 400);
    unsafe {
        SetBkMode(dc, TRANSPARENT as i32);
    }
    let previous = unsafe { SelectObject(dc, small) };
    for tile in &scene.tiles {
        let outer = RECT {
            left: tile.rect.x + inset,
            top: tile.rect.y + inset,
            right: tile.rect.x + tile.rect.width as i32 - inset,
            bottom: tile.rect.y + tile.rect.height as i32 - inset,
        };
        if outer.right <= outer.left || outer.bottom <= outer.top {
            continue;
        }
        let (line, body) = colors(tile.active);
        fill(dc, outer, line);
        let inner = RECT {
            left: outer.left + border,
            top: outer.top + border,
            right: outer.right - border,
            bottom: outer.bottom - border,
        };
        fill(dc, inner, body);
        unsafe {
            SelectObject(dc, small);
            SetTextColor(dc, rgb(170, 172, 178));
        }
        let title = RECT {
            left: inner.left + pad,
            top: inner.top + pad,
            right: inner.right - pad,
            bottom: inner.top + pad + px(20.0),
        };
        text(dc, &tile.title, title, DT_LEFT);
        unsafe {
            SelectObject(dc, big);
            SetTextColor(dc, rgb(230, 230, 230));
        }
        text(dc, &tile.label, inner, DT_CENTER | DT_VCENTER);
    }
    // Tabs for windows off screen, stacked around the middle of their edge.
    unsafe {
        SelectObject(dc, small);
    }
    let (width, height) = (px(240.0).min(size.right / 3), px(36.0));
    for right in [false, true] {
        let marks: Vec<&Mark> = scene.marks.iter().filter(|m| m.right == right).collect();
        let mut top = (size.bottom - height * marks.len() as i32) / 2;
        for mark in marks {
            let r = if right {
                RECT {
                    left: size.right - width,
                    top,
                    right: size.right,
                    bottom: top + height - px(4.0),
                }
            } else {
                RECT {
                    left: 0,
                    top,
                    right: width,
                    bottom: top + height - px(4.0),
                }
            };
            let (line, body) = colors(mark.active);
            fill(dc, r, line);
            let inner = RECT {
                left: r.left + if right { border } else { 0 },
                top: r.top + border,
                right: r.right - if right { 0 } else { border },
                bottom: r.bottom - border,
            };
            fill(dc, inner, body);
            unsafe {
                SetTextColor(dc, rgb(230, 230, 230));
            }
            let label = if right {
                format!("{} →", mark.title)
            } else {
                format!("← {}", mark.title)
            };
            let inner = RECT {
                left: inner.left + pad,
                right: inner.right - pad,
                ..inner
            };
            text(dc, &label, inner, DT_VCENTER | if right { DT_RIGHT } else { DT_LEFT });
            top += height;
        }
    }
    unsafe {
        SelectObject(dc, previous);
        DeleteObject(big);
        DeleteObject(small);
    }
}

unsafe extern "system" fn overlay_proc(h: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_TIMER => {
            update(None);
            0
        }
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
                match state.session.as_ref().and_then(|s| s.scene.as_ref()) {
                    Some(scene) => paint(memory, size, scene),
                    None => fill(memory, size, rgb(20, 20, 22)),
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
            let classes: [(&Vec<u16>, WNDPROC, HCURSOR); 2] = [
                // Strips choose their cursor (columns ↔, rows ↕) in WM_SETCURSOR.
                (&strip, Some(strip_proc), null_mut()),
                (&overlay, Some(overlay_proc), LoadCursorW(null_mut(), IDC_ARROW)),
            ];
            for (name, procedure, cursor) in classes {
                let class = WNDCLASSW {
                    lpfnWndProc: procedure,
                    hInstance: instance,
                    hCursor: cursor,
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
                if message.hwnd.is_null() {
                    match message.message {
                        SYNC => sync(),
                        REQUEST => requests(),
                        _ => {}
                    }
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
