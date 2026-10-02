//! Windows controller requests and HWND messages share one wait, without polling.
use super::{AppError, ErrorCode, ReceiveError, Request, error};
use std::{
    io,
    ptr::{null, null_mut},
    sync::{Arc, mpsc},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{CreateEventW, ResetEvent, SetEvent},
    UI::WindowsAndMessaging::*,
};

struct WakeEvent(usize);
impl WakeEvent {
    fn handle(&self) -> HANDLE {
        self.0 as HANDLE
    }
    fn signal(&self) -> Result<(), AppError> {
        checked(unsafe { SetEvent(self.handle()) }, "SetEvent")
    }
    fn reset(&self) -> Result<(), AppError> {
        checked(unsafe { ResetEvent(self.handle()) }, "ResetEvent")
    }
}
impl Drop for WakeEvent {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle());
        }
    }
}

fn checked(ok: i32, operation: &str) -> Result<(), AppError> {
    if ok != 0 {
        Ok(())
    } else {
        Err(error(
            ErrorCode::BackendUnavailable,
            format!("{operation}: {}", io::Error::last_os_error()),
        ))
    }
}

pub(super) struct Sender {
    // Drop the channel before signalling disconnect, not after Sender::drop returns.
    channel: Option<mpsc::SyncSender<Request>>,
    wake: Arc<WakeEvent>,
}
impl Clone for Sender {
    fn clone(&self) -> Self {
        Self {
            channel: self.channel.clone(),
            wake: self.wake.clone(),
        }
    }
}
impl Sender {
    pub(super) fn try_send(&self, request: Request) -> Result<(), AppError> {
        self.channel
            .as_ref()
            .unwrap()
            .try_send(request)
            .map_err(|_| {
                error(
                    ErrorCode::BackendUnavailable,
                    "窗口控制器暂时忙碌或已退出，请稍后重试。",
                )
            })?;
        self.wake.signal()
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        drop(self.channel.take());
        // Receiver and other producers still own the event, so it remains valid.
        if let Err(issue) = self.wake.signal() {
            eprintln!("{}", issue.message);
        }
    }
}

pub(super) struct Receiver {
    channel: mpsc::Receiver<Request>,
    wake: Arc<WakeEvent>,
}
pub(super) fn bounded(capacity: usize) -> Result<(Sender, Receiver), AppError> {
    let handle = unsafe { CreateEventW(null(), 1, 0, null()) };
    if handle.is_null() {
        return Err(error(
            ErrorCode::BackendUnavailable,
            format!("CreateEventW: {}", io::Error::last_os_error()),
        ));
    }
    let wake = Arc::new(WakeEvent(handle as usize));
    let (sender, receiver) = mpsc::sync_channel(capacity);
    Ok((
        Sender {
            channel: Some(sender),
            wake: wake.clone(),
        },
        Receiver {
            channel: receiver,
            wake,
        },
    ))
}

fn wait_for_input(handle: HANDLE, deadline: Instant) -> Result<(), ReceiveError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    // Round up sub-millisecond deadlines, never use INFINITE or a fixed poll interval.
    let milliseconds =
        remaining.as_millis() + u128::from(remaining.subsec_nanos() % 1_000_000 != 0);
    let milliseconds = milliseconds.min(u128::from(u32::MAX - 1)) as u32;
    let result = unsafe {
        MsgWaitForMultipleObjectsEx(1, &handle, milliseconds, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
    };
    match result {
        WAIT_OBJECT_0 => Ok(()),
        value if value == WAIT_OBJECT_0 + 1 => Ok(()),
        WAIT_TIMEOUT => Err(ReceiveError::Timeout),
        WAIT_FAILED => Err(ReceiveError::Failed(error(
            ErrorCode::BackendUnavailable,
            format!(
                "MsgWaitForMultipleObjectsEx: {}",
                io::Error::last_os_error()
            ),
        ))),
        _ => Err(ReceiveError::Failed(error(
            ErrorCode::BackendUnavailable,
            format!("Unexpected controller wait result: {result:#x}"),
        ))),
    }
}
impl Receiver {
    fn try_request(&self) -> Result<Option<Request>, ReceiveError> {
        match self.channel.try_recv() {
            Ok(request) => Ok(Some(request)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(ReceiveError::Disconnected),
        }
    }

    pub(super) fn recv_until(&self, deadline: Instant) -> Result<Request, ReceiveError> {
        loop {
            // Dispatch only on this HWND-owning thread, with no application lock held.
            // A batch is bounded; check requests/deadlines after each message so a
            // message flood cannot starve commands, animation or refresh.
            let mut message: MSG = unsafe { std::mem::zeroed() };
            for _ in 0..32 {
                if unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } == 0 {
                    break;
                }
                if message.message == WM_QUIT {
                    return Err(ReceiveError::Disconnected);
                }
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                if let Some(request) = self.try_request()? {
                    return Ok(request);
                }
                if Instant::now() >= deadline {
                    return Err(ReceiveError::Timeout);
                }
            }
            if let Some(request) = self.try_request()? {
                return Ok(request);
            }
            if Instant::now() >= deadline {
                return Err(ReceiveError::Timeout);
            }

            self.wake.reset().map_err(ReceiveError::Failed)?;
            // Enqueue precedes SetEvent. Recheck after ResetEvent so a producer
            // racing the reset cannot leave a queued request with no wake.
            if let Some(request) = self.try_request()? {
                return Ok(request);
            }
            wait_for_input(self.wake.handle(), deadline)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::{GetHandleInformation, HWND},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    };

    struct Probe {
        count: AtomicUsize,
        repeat: bool,
        observed: mpsc::Sender<u32>,
    }
    unsafe extern "system" fn probe_proc(hwnd: HWND, message: u32, w: usize, l: isize) -> isize {
        if message == WM_NCCREATE {
            let create = unsafe { &*(l as *const CREATESTRUCTW) };
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            }
        }
        if message == WM_APP {
            let probe = unsafe { &*(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Probe) };
            if probe.count.fetch_add(1, Ordering::Relaxed) == 0 {
                let _ = probe.observed.send(unsafe { GetCurrentThreadId() });
            }
            if probe.repeat {
                unsafe {
                    PostMessageW(hwnd, WM_APP, 0, 0);
                }
            }
            return 0;
        }
        unsafe { DefWindowProcW(hwnd, message, w, l) }
    }
    fn message_window(probe: &Probe) -> HWND {
        let class: Vec<u16> = "e-desktop-controller-queue-test\0".encode_utf16().collect();
        let instance = unsafe { GetModuleHandleW(null()) };
        let registered = WNDCLASSW {
            lpfnWndProc: Some(probe_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..unsafe { std::mem::zeroed() }
        };
        unsafe {
            RegisterClassW(&registered);
            let hwnd = CreateWindowExW(
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
                instance,
                probe as *const Probe as *const _,
            );
            assert!(!hwnd.is_null());
            hwnd
        }
    }

    #[test]
    fn controller_wait_dispatches_hwnd_messages_without_a_request() {
        let (sender, receiver) = bounded(2).unwrap();
        let (ready, window) = mpsc::channel();
        let (observed, messages) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let probe = Probe {
                count: AtomicUsize::new(0),
                repeat: false,
                observed,
            };
            let hwnd = message_window(&probe);
            let tid = unsafe { GetCurrentThreadId() };
            ready.send((hwnd as usize, tid)).unwrap();
            let request = receiver.recv_until(Instant::now() + Duration::from_secs(5));
            unsafe {
                DestroyWindow(hwnd);
            }
            assert!(matches!(request, Ok(Request::Quit)));
        });
        let (hwnd, tid) = window.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_ne!(unsafe { PostMessageW(hwnd as HWND, WM_APP, 0, 0) }, 0);
        // The old mpsc-only wait cannot deliver this callback until a request/deadline.
        assert_eq!(messages.recv_timeout(Duration::from_secs(2)).unwrap(), tid);
        sender.try_send(Request::Quit).unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn controller_queue_drains_reset_wakes_and_disconnects_then_closes_event() {
        let (sender, receiver) = bounded(2).unwrap();
        let clone = sender.clone();
        let handle = receiver.wake.handle();
        sender.try_send(Request::Quit).unwrap();
        sender.try_send(Request::Quit).unwrap();
        assert!(sender.try_send(Request::Quit).is_err()); // Still bounded.
        receiver.wake.reset().unwrap(); // Simulate clearing an enqueue's signal.
        for _ in 0..2 {
            assert!(matches!(
                receiver.recv_until(Instant::now() + Duration::from_secs(1)),
                Ok(Request::Quit)
            ));
        }
        drop(sender);
        assert!(matches!(receiver.try_request(), Ok(None))); // A producer remains.
        drop(clone);
        assert!(matches!(
            receiver.recv_until(Instant::now() + Duration::from_secs(1)),
            Err(ReceiveError::Disconnected)
        ));
        drop(receiver);
        let mut flags = 0;
        assert_eq!(unsafe { GetHandleInformation(handle, &mut flags) }, 0);
    }

    #[test]
    fn controller_wait_reports_invalid_event_instead_of_spinning() {
        // Fresh thread has no leftover test-window messages; null is not a pseudo-handle.
        assert!(
            std::thread::spawn(|| matches!(
                wait_for_input(null_mut(), Instant::now() + Duration::from_secs(1)),
                Err(ReceiveError::Failed(_))
            ))
            .join()
            .unwrap()
        );
    }

    #[test]
    fn controller_message_flood_respects_deadline_and_queued_request() {
        let (sender, receiver) = bounded(2).unwrap();
        let (observed, _messages) = mpsc::channel();
        let probe = Probe {
            count: AtomicUsize::new(0),
            repeat: true,
            observed,
        };
        let hwnd = message_window(&probe);
        assert_ne!(unsafe { PostMessageW(hwnd, WM_APP, 0, 0) }, 0);
        let start = Instant::now();
        let result = receiver.recv_until(start + Duration::from_millis(20));
        assert!(matches!(result, Err(ReceiveError::Timeout)));
        assert!(probe.count.load(Ordering::Relaxed) > 0);
        assert!(start.elapsed() < Duration::from_secs(2));
        sender.try_send(Request::Quit).unwrap();
        let count = probe.count.load(Ordering::Relaxed);
        assert!(matches!(
            receiver.recv_until(Instant::now() + Duration::from_secs(1)),
            Ok(Request::Quit)
        ));
        assert!(probe.count.load(Ordering::Relaxed) <= count + 1);
        unsafe {
            DestroyWindow(hwnd);
        }
    }
}
