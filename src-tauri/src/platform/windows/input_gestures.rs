//! Global wheel and touchpad gestures (lane: input-gestures). The mouse hook hands modifier +
//! wheel events here; a hidden message window on the hook thread receives precision-touchpad
//! contacts through Raw Input. Both queue hook events for the controller.
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::Instant,
};

use windows_sys::Win32::{
    Devices::HumanInterfaceDevice::*,
    Foundation::*,
    System::LibraryLoader::GetModuleHandleW,
    UI::{Input::KeyboardAndMouse::*, Input::*, WindowsAndMessaging::*},
};

use super::hook::{Raw, down, mask_modifier, push};
use crate::{
    config::WheelModifier,
    gestures::{Contact, Frames, Slot, Swipe, Touchpad, wheel_modifier_held},
};

/// Re-register the touchpad after the configured finger count changed.
const WM_SYNC: u32 = WM_APP + 0x47;
const DIGITIZER: u16 = 0x0D;
const TOUCH_PAD: u16 = 0x05;
const TIP_SWITCH: u16 = 0x42;
const CONTACT_ID: u16 = 0x51;
const CONTACT_COUNT: u16 = 0x54;
const GENERIC_DESKTOP: u16 = 0x01;
const X: u16 = 0x30;
const Y: u16 = 0x31;

static WHEEL: Mutex<Option<WheelModifier>> = Mutex::new(None);
/// 0 when touchpad gestures are off.
static FINGERS: AtomicU8 = AtomicU8::new(0);
static WINDOW: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static LAST_MASK: Cell<Option<u32>> = const { Cell::new(None) };
    static PAD: RefCell<Pad> = RefCell::new(Pad::default());
}

/// None disables either gesture (paused management, open surfaces for the wheel).
pub fn configure(wheel: Option<WheelModifier>, fingers: Option<u8>) {
    *WHEEL.lock().unwrap_or_else(|e| e.into_inner()) = wheel;
    let fingers = fingers.unwrap_or(0);
    if FINGERS.swap(fingers, Ordering::AcqRel) != fingers {
        let window = WINDOW.load(Ordering::Acquire);
        if window != 0 {
            unsafe { PostMessageW(window as HWND, WM_SYNC, 0, 0) };
        }
    }
}

/// Mouse hook wheel message at the clamped point; true swallows it.
pub(super) fn wheel(message: u32, info: &MSLLHOOKSTRUCT, x: i32, y: i32) -> bool {
    let Some(modifier) = *WHEEL.lock().unwrap_or_else(|e| e.into_inner()) else {
        return false;
    };
    let win = down(VK_LWIN) || down(VK_RWIN);
    if !wheel_modifier_held(modifier, down(VK_CONTROL), down(VK_MENU), win)
        || down(VK_LBUTTON)
        || down(VK_RBUTTON)
        || down(VK_MBUTTON)
    {
        return false;
    }
    let delta = i32::from((info.mouseData >> 16) as u16 as i16);
    // Shift turns the vertical wheel sideways: down is right, as for niri Mod+Shift+Wheel.
    let (delta, horizontal) = if message == WM_MOUSEHWHEEL {
        (delta, true)
    } else if down(VK_SHIFT) {
        (-delta, true)
    } else {
        (delta, false)
    };
    // Releasing Win or Alt after only wheel input would open Start or the menu bar.
    if LAST_MASK
        .get()
        .is_none_or(|last| info.time.wrapping_sub(last) >= 100)
    {
        LAST_MASK.set(Some(info.time));
        mask_modifier();
    }
    push(Raw::Wheel {
        x,
        y,
        delta,
        horizontal,
        time: info.time,
    });
    true
}

/// Y logical units per X logical unit of physical length; 1 without physical extents.
fn y_scale(x: &HIDP_VALUE_CAPS, y: &HIDP_VALUE_CAPS) -> f64 {
    let per_unit = |c: &HIDP_VALUE_CAPS| {
        let logical = f64::from(c.LogicalMax) - f64::from(c.LogicalMin);
        let physical = f64::from(c.PhysicalMax) - f64::from(c.PhysicalMin);
        (logical > 0.0 && physical > 0.0).then(|| physical / logical)
    };
    match (per_unit(x), per_unit(y)) {
        (Some(px), Some(py)) => py / px,
        _ => 1.0,
    }
}

struct Device {
    /// u64 keeps the opaque preparsed blob aligned.
    preparsed: Vec<u64>,
    /// Finger link collections in report order.
    fingers: Vec<u16>,
    count: u16,
    x_min: f64,
    width: f64,
    y_min: f64,
    y_scale: f64,
    frames: Frames,
}

impl Device {
    fn new(handle: HANDLE) -> Option<Self> {
        let mut size = 0u32;
        unsafe { GetRawInputDeviceInfoW(handle, RIDI_PREPARSEDDATA, null_mut(), &mut size) };
        if size == 0 {
            return None;
        }
        let mut preparsed = vec![0u64; (size as usize).div_ceil(8)];
        let read = unsafe {
            GetRawInputDeviceInfoW(
                handle,
                RIDI_PREPARSEDDATA,
                preparsed.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if read == u32::MAX || read == 0 {
            return None;
        }
        let data = preparsed.as_ptr() as PHIDP_PREPARSED_DATA;
        let mut caps: HIDP_CAPS = unsafe { zeroed() };
        if unsafe { HidP_GetCaps(data, &mut caps) } != HIDP_STATUS_SUCCESS {
            return None;
        }
        let mut length = caps.NumberInputValueCaps;
        let mut values: Vec<HIDP_VALUE_CAPS> = vec![unsafe { zeroed() }; length.into()];
        if unsafe { HidP_GetValueCaps(HidP_Input, values.as_mut_ptr(), &mut length, data) }
            != HIDP_STATUS_SUCCESS
        {
            return None;
        }
        values.truncate(length.into());
        let find = |page: u16, usage: u16| {
            values.iter().filter(move |c| {
                !c.IsRange && c.UsagePage == page && unsafe { c.Anonymous.NotRange.Usage } == usage
            })
        };
        let count = find(DIGITIZER, CONTACT_COUNT).next()?.LinkCollection;
        let mut fingers: Vec<u16> = find(GENERIC_DESKTOP, X).map(|c| c.LinkCollection).collect();
        fingers.sort_unstable();
        fingers.dedup();
        let x = find(GENERIC_DESKTOP, X).next()?;
        let y = find(GENERIC_DESKTOP, Y).next()?;
        let width = f64::from(x.LogicalMax) - f64::from(x.LogicalMin);
        (width > 0.0).then(|| Self {
            fingers,
            count,
            x_min: x.LogicalMin.into(),
            width,
            y_min: y.LogicalMin.into(),
            y_scale: y_scale(x, y),
            frames: Frames::default(),
            preparsed,
        })
    }

    fn value(&self, report: &[u8], page: u16, link: u16, usage: u16) -> Option<u32> {
        let mut value = 0;
        (unsafe {
            HidP_GetUsageValue(
                HidP_Input,
                page,
                link,
                usage,
                &mut value,
                self.preparsed.as_ptr() as PHIDP_PREPARSED_DATA,
                report.as_ptr(),
                report.len() as u32,
            )
        } == HIDP_STATUS_SUCCESS)
            .then_some(value)
    }

    fn tip(&self, report: &mut [u8], link: u16) -> bool {
        let mut usages = [0u16; 16];
        let mut length = usages.len() as u32;
        let status = unsafe {
            HidP_GetUsages(
                HidP_Input,
                DIGITIZER,
                link,
                usages.as_mut_ptr(),
                &mut length,
                self.preparsed.as_ptr() as PHIDP_PREPARSED_DATA,
                report.as_mut_ptr(),
                report.len() as u32,
            )
        };
        status == HIDP_STATUS_SUCCESS
            && usages[..(length as usize).min(usages.len())].contains(&TIP_SWITCH)
    }

    /// The touching contacts once this report completes a frame.
    fn report(&mut self, report: &mut [u8]) -> Option<Vec<Contact>> {
        let count = self.value(report, DIGITIZER, self.count, CONTACT_COUNT)?;
        let mut slots = Vec::with_capacity(self.fingers.len());
        for &link in &self.fingers {
            let (Some(x), Some(y)) = (
                self.value(report, GENERIC_DESKTOP, link, X),
                self.value(report, GENERIC_DESKTOP, link, Y),
            ) else {
                continue;
            };
            slots.push(Slot {
                tip: self.tip(report, link),
                contact: Contact {
                    id: self
                        .value(report, DIGITIZER, link, CONTACT_ID)
                        .unwrap_or(link.into()),
                    x: (f64::from(x) - self.x_min) / self.width,
                    y: (f64::from(y) - self.y_min) * self.y_scale / self.width,
                },
            });
        }
        self.frames.report(count, &slots)
    }
}

#[derive(Default)]
struct Pad {
    registered: bool,
    /// None: not a usable precision touchpad.
    devices: HashMap<usize, Option<Device>>,
    swipe: Option<(u8, Touchpad)>,
}

fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

impl Pad {
    fn sync(&mut self, window: HWND) {
        // The controller ends its swipe as well: the restarted recognizer never releases it.
        if self.swipe.take().is_some() {
            push(Raw::Swipe {
                x: 0,
                y: 0,
                swipe: Swipe::Cancel,
            });
        }
        let wanted = FINGERS.load(Ordering::Acquire) != 0;
        if wanted == self.registered {
            return;
        }
        let device = RAWINPUTDEVICE {
            usUsagePage: DIGITIZER,
            usUsage: TOUCH_PAD,
            dwFlags: if wanted {
                RIDEV_INPUTSINK
            } else {
                RIDEV_REMOVE
            },
            hwndTarget: if wanted { window } else { null_mut() },
        };
        if unsafe { RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) } != 0 {
            self.registered = wanted;
        }
    }

    fn input(&mut self, handle: HRAWINPUT) {
        let fingers = FINGERS.load(Ordering::Acquire);
        if fingers == 0 {
            return;
        }
        let header = size_of::<RAWINPUTHEADER>() as u32;
        let mut size = 0u32;
        unsafe { GetRawInputData(handle, RID_INPUT, null_mut(), &mut size, header) };
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let read = unsafe {
            GetRawInputData(
                handle,
                RID_INPUT,
                buffer.as_mut_ptr().cast(),
                &mut size,
                header,
            )
        };
        if read == u32::MAX || (read as usize) < size_of::<RAWINPUTHEADER>() + 8 {
            return;
        }
        let raw = buffer.as_mut_ptr() as *mut RAWINPUT;
        let (kind, device) = unsafe { ((*raw).header.dwType, (*raw).header.hDevice) };
        if kind != RIM_TYPEHID {
            return;
        }
        let (report_size, reports, offset) = unsafe {
            let hid = &raw const (*raw).data.hid;
            (
                (*hid).dwSizeHid as usize,
                (*hid).dwCount as usize,
                (&raw const (*hid).bRawData) as usize - raw as usize,
            )
        };
        if report_size == 0 || offset + report_size * reports > read as usize {
            return;
        }
        let bytes: &mut [u8] =
            unsafe { std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast(), buffer.len() * 8) };
        let Some(entry) = self
            .devices
            .entry(device as usize)
            .or_insert_with(|| Device::new(device))
        else {
            return;
        };
        for report in bytes[offset..offset + report_size * reports].chunks_exact_mut(report_size) {
            let Some(contacts) = entry.report(report) else {
                continue;
            };
            if self.swipe.as_ref().is_none_or(|(n, _)| *n != fingers) {
                self.swipe = Some((fingers, Touchpad::new(fingers)));
            }
            let Some(swipe) = self.swipe.as_mut().unwrap().1.frame(&contacts, now_ms()) else {
                continue;
            };
            let mut point: POINT = unsafe { zeroed() };
            unsafe { GetCursorPos(&mut point) };
            push(Raw::Swipe {
                x: point.x,
                y: point.y,
                swipe,
            });
        }
    }
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_INPUT => PAD.with_borrow_mut(|pad| pad.input(lparam as HRAWINPUT)),
        WM_SYNC => {
            PAD.with_borrow_mut(|pad| pad.sync(window));
            return 0;
        }
        _ => {}
    }
    // WM_INPUT also needs the default procedure to release the input.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Hidden message window on the hook thread; null if it could not be created.
pub(super) fn create_window() -> HWND {
    let class = super::wide("e-desktop-touchpad");
    let instance = unsafe { GetModuleHandleW(null()) };
    let mut registered: WNDCLASSW = unsafe { zeroed() };
    registered.lpfnWndProc = Some(procedure);
    registered.hInstance = instance;
    registered.lpszClassName = class.as_ptr();
    unsafe { RegisterClassW(&registered) };
    let window = unsafe {
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
            instance,
            null(),
        )
    };
    if !window.is_null() {
        WINDOW.store(window as usize, Ordering::Release);
        PAD.with_borrow_mut(|pad| pad.sync(window));
    }
    window
}

pub(super) fn destroy_window(window: HWND) {
    if window.is_null() {
        return;
    }
    WINDOW.store(0, Ordering::Release);
    PAD.with_borrow_mut(|pad| {
        if pad.registered {
            let device = RAWINPUTDEVICE {
                usUsagePage: DIGITIZER,
                usUsage: TOUCH_PAD,
                dwFlags: RIDEV_REMOVE,
                hwndTarget: null_mut(),
            };
            unsafe { RegisterRawInputDevices(&device, 1, size_of::<RAWINPUTDEVICE>() as u32) };
            pad.registered = false;
        }
        pad.devices.clear();
    });
    unsafe { DestroyWindow(window) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touchpad_y_is_measured_in_x_units_of_physical_length() {
        let caps = |logical: i32, physical: i32| {
            let mut c: HIDP_VALUE_CAPS = unsafe { zeroed() };
            c.LogicalMax = logical;
            c.PhysicalMax = physical;
            c
        };
        // 1000 x units over 100 mm, 500 y units over 100 mm: one y unit is two x units.
        assert_eq!(y_scale(&caps(1000, 100), &caps(500, 100)), 2.0);
        assert_eq!(
            y_scale(&caps(1000, 0), &caps(500, 100)),
            1.0,
            "no physical extent"
        );
    }
}
