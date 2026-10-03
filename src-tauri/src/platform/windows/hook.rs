//! Low-level mouse hook thread. The callback only swallows modifier+button presses on managed
//! windows and queues coalesced pointer events; the controller thread does everything else.
//! The same thread receives WinEvents: windows appearing or disappearing, and system
//! move/size loops of managed windows.
use std::{
    cell::Cell,
    collections::{HashSet, VecDeque},
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};

use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{Accessibility::*, HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

use crate::{
    config::DragModifier,
    model::{AppError, ErrorCode, Rect},
};

#[derive(Debug, Clone)]
pub enum Raw {
    /// `pressed`: a mouse button is held (native drags must not open hot corners).
    Move {
        x: i32,
        y: i32,
        pressed: bool,
    },
    Grab {
        hwnd: usize,
        x: i32,
        y: i32,
    },
    Release {
        x: i32,
        y: i32,
    },
    /// An ordinary left button release (may end a native move/resize of some window).
    Up,
    /// A managed window entered (`start`) or left the system move/size loop.
    MoveSize {
        hwnd: usize,
        start: bool,
        moving: Option<bool>,
    },
    /// A managed window was destroyed or hidden, or a new top-level window was shown.
    Windows,
    // lane: input-gestures
    /// A swallowed modifier + wheel event; `delta` > 0 is up, or right when `horizontal`.
    Wheel {
        x: i32,
        y: i32,
        delta: i32,
        horizontal: bool,
        time: u32,
    },
    /// A touchpad swipe step with the pointer at (x, y).
    Swipe {
        x: i32,
        y: i32,
        swipe: crate::gestures::Swipe,
    },
}

#[derive(Default)]
pub struct Settings {
    /// Top-level windows a modifier+button press may grab.
    pub targets: HashSet<usize>,
    /// Every managed top-level window (lifetime and move/size events).
    pub managed: HashSet<usize>,
    pub modifier: Option<DragModifier>,
    /// Report all plain pointer motion (focus-follows-mouse, revealed bars).
    pub moves: bool,
    /// Otherwise report motion only when entering or leaving these zones (hot corners, top edges).
    pub zones: Vec<Rect>,
    /// Monitor bounds where a foreign window covers the display. Clicks there are not swallowed.
    pub suspended: Vec<Rect>,
}

#[derive(Default)]
struct Shared {
    settings: Mutex<Settings>,
    events: Mutex<VecDeque<Raw>>,
    pending: AtomicBool,
    wake: Mutex<Option<Box<dyn Fn() + Send>>>,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(Shared::default)
}

thread_local! {
    static GRABBED: Cell<bool> = const { Cell::new(false) };
    static ZONE: Cell<Option<usize>> = const { Cell::new(None) };
}

pub(super) fn push(event: Raw) {
    let shared = shared();
    {
        let mut events = shared.events.lock().unwrap_or_else(|e| e.into_inner());
        // lane: input-gestures: one swipe drag per drained batch; the batch is already woken.
        if let (
            Some(Raw::Swipe {
                swipe: crate::gestures::Swipe::Drag(sum),
                ..
            }),
            Raw::Swipe {
                swipe: crate::gestures::Swipe::Drag(step),
                ..
            },
        ) = (events.back_mut(), &event)
        {
            *sum += step;
            return;
        }
        if matches!(
            (events.back(), &event),
            (Some(Raw::Move { .. }), Raw::Move { .. }) | (Some(Raw::Windows), Raw::Windows)
        ) {
            events.pop_back();
        }
        events.push_back(event);
    }
    // One wake per drained batch keeps pointer traffic from filling the command channel.
    if !shared.pending.swap(true, Ordering::AcqRel) {
        if let Some(wake) = &*shared.wake.lock().unwrap_or_else(|e| e.into_inner()) {
            wake();
        }
    }
}

pub fn configure(settings: Settings) {
    *shared().settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
}

pub fn drain() -> Vec<Raw> {
    let shared = shared();
    shared.pending.store(false, Ordering::Release);
    shared
        .events
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect()
}

pub(super) fn down(key: VIRTUAL_KEY) -> bool {
    (unsafe { GetAsyncKeyState(key as i32) }) < 0
}

/// Consume the held modifier so releasing it does not open the Alt menu or Start.
pub(super) fn mask_modifier() {
    let mut inputs: [INPUT; 2] = unsafe { zeroed() };
    for (input, flags) in inputs.iter_mut().zip([0, KEYEVENTF_KEYUP]) {
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous.ki = KEYBDINPUT {
            wVk: 0xE8, // Unassigned virtual key, the conventional menu mask key.
            wScan: 0,
            dwFlags: flags,
            time: 0,
            dwExtraInfo: 0,
        };
    }
    unsafe {
        SendInput(2, inputs.as_ptr(), size_of::<INPUT>() as i32);
    }
}

/// The hook reports the unclipped target of a relative move (e.g. y < 0 when pushing against
/// the top edge); clamp it onto the nearest monitor like the real cursor.
fn clamp(pt: POINT) -> (i32, i32) {
    let monitor = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    let mut info: MONITORINFO = unsafe { zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return (pt.x, pt.y);
    }
    let r = info.rcMonitor;
    (
        pt.x.clamp(r.left, r.right - 1),
        pt.y.clamp(r.top, r.bottom - 1),
    )
}

pub(super) fn on_suspended(rects: &[Rect], x: i32, y: i32) -> bool {
    let (x, y) = (i64::from(x), i64::from(y));
    rects.iter().any(|r| {
        x >= i64::from(r.x)
            && y >= i64::from(r.y)
            && x < i64::from(r.x) + i64::from(r.width)
            && y < i64::from(r.y) + i64::from(r.height)
    })
}

/// Modifier-click is swallowed only for a grab target that is not on a suspended monitor.
pub(super) fn swallows_grab(on_suspended_monitor: bool, is_target: bool) -> bool {
    is_target && !on_suspended_monitor
}

fn handle(message: u32, info: &MSLLHOOKSTRUCT) -> bool {
    let (x, y) = clamp(info.pt);
    let grabbed = GRABBED.get();
    match message {
        WM_MOUSEMOVE => {
            let (moves, zone) = {
                let settings = shared().settings.lock().unwrap_or_else(|e| e.into_inner());
                let zone = settings.zones.iter().position(|r| {
                    x as i64 >= r.x as i64
                        && y as i64 >= r.y as i64
                        && (x as i64) < r.x as i64 + r.width as i64
                        && (y as i64) < r.y as i64 + r.height as i64
                });
                (settings.moves, zone)
            };
            let crossed = zone != ZONE.replace(zone);
            if grabbed || moves || crossed {
                let pressed = down(VK_LBUTTON) || down(VK_RBUTTON) || down(VK_MBUTTON);
                push(Raw::Move { x, y, pressed });
            }
            false
        }
        WM_LBUTTONDOWN => {
            if grabbed || info.flags & LLMHF_INJECTED != 0 {
                return false;
            }
            let hwnd = {
                let settings = shared().settings.lock().unwrap_or_else(|e| e.into_inner());
                // Before any swallow: a covering app on this monitor gets the click.
                if !swallows_grab(on_suspended(&settings.suspended, x, y), true) {
                    return false;
                }
                let held = match settings.modifier {
                    Some(DragModifier::Alt) => down(VK_MENU),
                    Some(DragModifier::Super) => down(VK_LWIN) || down(VK_RWIN),
                    None => false,
                };
                if !held {
                    return false;
                }
                // WindowFromPoint only hit-tests windows of the calling thread (none here).
                let hwnd = unsafe { GetAncestor(WindowFromPoint(info.pt), GA_ROOT) } as usize;
                if !swallows_grab(false, settings.targets.contains(&hwnd)) {
                    return false;
                }
                hwnd
            };
            GRABBED.set(true);
            mask_modifier();
            push(Raw::Grab { hwnd, x, y });
            true
        }
        WM_LBUTTONUP if grabbed => {
            GRABBED.set(false);
            push(Raw::Release { x, y });
            true
        }
        WM_LBUTTONUP => {
            push(Raw::Up);
            false
        }
        // lane: input-gestures
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let suspended = {
                let settings = shared().settings.lock().unwrap_or_else(|e| e.into_inner());
                on_suspended(&settings.suspended, x, y)
            };
            !grabbed && !suspended && super::input_gestures::wheel(message, info, x, y)
        }
        _ => false,
    }
}

/// A window enumeration would pick up as a new layout window.
fn candidate(h: HWND) -> bool {
    unsafe {
        GetAncestor(h, GA_ROOT) == h
            && GetWindow(h, GW_OWNER).is_null()
            && GetWindowLongPtrW(h, GWL_STYLE) as u32 & WS_CHILD == 0
            && GetWindowLongPtrW(h, GWL_EXSTYLE) as u32 & (WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) == 0
    }
}

/// Classify the native operation from its hit target, not the size it eventually reports:
/// a title-bar drag may restore, snap, or change DPI while it is still a move.
pub(super) fn moving_from_hit(hit: i32) -> Option<bool> {
    match hit as u32 {
        HTCAPTION => Some(true),
        HTLEFT | HTRIGHT | HTTOP | HTTOPLEFT | HTTOPRIGHT | HTBOTTOM | HTBOTTOMLEFT
        | HTBOTTOMRIGHT => Some(false),
        _ => None,
    }
}

fn native_moving(hwnd: HWND) -> Option<bool> {
    let mut point: POINT = unsafe { zeroed() };
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return None;
    }
    let coords = ((point.y as u32 & 0xffff) << 16) | (point.x as u32 & 0xffff);
    let mut hit = 0;
    let ok = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_NCHITTEST,
            0,
            coords as LPARAM,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            30,
            &mut hit,
        )
    };
    (ok != 0).then(|| moving_from_hit(hit as i32)).flatten()
}

unsafe extern "system" fn window_event(
    _: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _: u32,
    _: u32,
) {
    if hwnd.is_null() || object != OBJID_WINDOW || child != CHILDID_SELF as i32 {
        return;
    }
    let managed = shared()
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .managed
        .contains(&(hwnd as usize));
    match event {
        EVENT_SYSTEM_MOVESIZESTART | EVENT_SYSTEM_MOVESIZEEND if managed => push(Raw::MoveSize {
            hwnd: hwnd as usize,
            start: event == EVENT_SYSTEM_MOVESIZESTART,
            moving: if event == EVENT_SYSTEM_MOVESIZESTART {
                native_moving(hwnd)
            } else {
                None
            },
        }),
        EVENT_OBJECT_DESTROY | EVENT_OBJECT_HIDE if managed => push(Raw::Windows),
        EVENT_OBJECT_SHOW if managed || candidate(hwnd) => push(Raw::Windows),
        _ => {}
    }
}

unsafe extern "system" fn procedure(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && handle(wparam as u32, unsafe {
            &*(lparam as *const MSLLHOOKSTRUCT)
        })
    {
        return 1;
    }
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

/// Owns the hook thread; dropping it unhooks.
pub struct Hook {
    thread: u32,
    handle: Option<JoinHandle<()>>,
}

pub fn start(wake: Box<dyn Fn() + Send>) -> Result<Hook, AppError> {
    let failure = |message: String| AppError {
        code: ErrorCode::BackendUnavailable,
        message,
        window_id: None,
    };
    *shared().wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("pointer-hook".into())
        .spawn(move || unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let mut message: MSG = zeroed();
            // Create the message queue before publishing the thread ID for PostThreadMessage.
            PeekMessageW(&mut message, null_mut(), 0, 0, PM_NOREMOVE);
            let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(procedure), GetModuleHandleW(null()), 0);
            if hook.is_null() {
                let _ = tx.send(Err(GetLastError()));
                return;
            }
            // Out-of-context WinEvents arrive through this thread's message loop.
            let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
            let events = [
                (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
                (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
            ]
            .map(|(min, max)| {
                SetWinEventHook(min, max, null_mut(), Some(window_event), 0, 0, flags)
            });
            // lane: input-gestures: Raw Input from the touchpad arrives on this thread too.
            let touchpad = super::input_gestures::create_window();
            let _ = tx.send(Ok(GetCurrentThreadId()));
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                DispatchMessageW(&message);
            }
            super::input_gestures::destroy_window(touchpad);
            for event in events.into_iter().filter(|h| !h.is_null()) {
                UnhookWinEvent(event);
            }
            UnhookWindowsHookEx(hook);
        })
        .map_err(|e| failure(e.to_string()))?;
    match rx.recv() {
        Ok(Ok(thread)) => Ok(Hook {
            thread,
            handle: Some(handle),
        }),
        Ok(Err(code)) => Err(failure(format!(
            "鼠标钩子安装失败（Win32 {code}），指针功能不可用。"
        ))),
        Err(_) => Err(failure("鼠标钩子线程意外退出。".into())),
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        unsafe {
            PostThreadMessageW(self.thread, WM_QUIT, 0, 0);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_title_move_is_distinct_from_border_resize() {
        assert_eq!(moving_from_hit(HTCAPTION as i32), Some(true));
        for hit in [
            HTLEFT,
            HTRIGHT,
            HTTOP,
            HTTOPLEFT,
            HTTOPRIGHT,
            HTBOTTOM,
            HTBOTTOMLEFT,
            HTBOTTOMRIGHT,
        ] {
            assert_eq!(moving_from_hit(hit as i32), Some(false));
        }
        assert_eq!(moving_from_hit(HTCLIENT as i32), None);
        assert_eq!(moving_from_hit(HTERROR), None);
    }

    #[test]
    fn a_suspended_monitor_is_not_a_grab_target() {
        assert!(swallows_grab(false, true));
        assert!(!swallows_grab(true, true), "click reaches the covering app");
        assert!(!swallows_grab(false, false));
        let covered = Rect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert!(on_suspended(&[covered], -10, 20));
        assert!(!on_suspended(&[covered], 10, 20), "the other monitor still grabs");
    }
}
