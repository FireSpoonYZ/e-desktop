//! Manual mixed-DPI regression check using an isolated Edge app window titled EDPEEKTEST.
//! Stop e-desktop first. Pass the fixture's process ID; no other windows are moved.
#[cfg(target_os = "windows")]
fn main() {
    use e_desktop::{model::*, platform::Backend};
    use std::{
        mem::{size_of, zeroed},
        ptr::null,
        thread::sleep,
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::RECT,
        Graphics::{Dwm::*, Gdi::SetWindowRgn},
        UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId},
    };

    let pid: u32 = std::env::args()
        .nth(1)
        .expect("fixture PID required")
        .parse()
        .unwrap();
    let mut backend = Backend::new().unwrap();
    let system = backend.enumerate().unwrap();
    assert!(
        !system
            .windows
            .iter()
            .any(|w| w.app_name.to_lowercase().contains("e-desktop")),
        "Stop the desktop manager before running this check"
    );
    let window = system
        .windows
        .iter()
        .find(|w| w.process_id == pid && w.title == "EDPEEKTEST")
        .expect("isolated EDPEEKTEST window with supplied PID");
    let title: Vec<u16> = "EDPEEKTEST\0".encode_utf16().collect();
    let hwnd = unsafe { FindWindowW(null(), title.as_ptr()) };
    let mut actual_pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut actual_pid);
    }
    assert_eq!(actual_pid, pid);

    let checks = (|| -> Result<(), String> {
        // Primary first exercises the previously missing DPI cache when the fixture starts
        // on a 100% side monitor. Then check both side monitors using the same window.
        let mut monitors: Vec<_> = system.monitors.iter().collect();
        monitors.sort_by_key(|m| !m.primary);
        for monitor in monitors {
            let area = monitor.work_area;
            let target = Rect {
                x: area.x + 40,
                width: area.width.saturating_sub(80),
                ..area
            };
            let clip = Rect {
                width: target.width / 2,
                ..target
            };
            for _ in 0..3 {
                backend
                    .apply(&[NativeAction::Placement {
                        window_id: window.id.clone(),
                        rect: target,
                        clip: Some(clip),
                        minimized: false,
                    }])
                    .map_err(|e| e.message)?;
                sleep(Duration::from_millis(150));
            }
            // Backend enumeration reuses placed_pad, so it cannot detect its own wrong pad.
            // Remove the fixture's clip and ask DWM independently for the actual visible frame.
            unsafe {
                if SetWindowRgn(hwnd, std::ptr::null_mut(), 1) == 0 {
                    return Err("SetWindowRgn failed".into());
                }
                DwmFlush();
            }
            sleep(Duration::from_millis(300));
            let mut frame: RECT = unsafe { zeroed() };
            let hr = unsafe {
                DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                    &mut frame as *mut _ as _,
                    size_of::<RECT>() as u32,
                )
            };
            if hr < 0 {
                return Err(format!("DwmGetWindowAttribute: {hr}"));
            }
            let actual = Rect {
                x: frame.left,
                y: frame.top,
                width: (frame.right - frame.left) as u32,
                height: (frame.bottom - frame.top) as u32,
            };
            println!(
                "{} scale={} target={target:?} visible={actual:?}",
                monitor.id, monitor.scale_factor
            );
            if actual != target {
                return Err("Visible frame does not fill target".into());
            }
            backend.restore().map_err(|e| e.message)?;
        }
        Ok(())
    })();
    let restore = backend.restore();
    checks.unwrap();
    restore.unwrap();
    println!("Mixed-DPI visible frame checks passed");
}
#[cfg(not(target_os = "windows"))]
fn main() {}
