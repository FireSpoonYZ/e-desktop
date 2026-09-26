use super::{error, ffi::*, geometry};
use crate::model::{AppError, ErrorCode, Monitor};
use std::ffi::c_void;

/// AppKit is accessed only on the main thread; no NSObject escapes the callback.
/// Integration must not synchronously block the main thread waiting on this backend.
pub fn monitors() -> Result<Vec<Monitor>, AppError> {
    let mut output = Err(error(
        ErrorCode::BackendUnavailable,
        "NSScreen work areas unavailable",
        None,
    ));
    unsafe {
        if pthread_main_np() != 0 {
            read_screens((&mut output as *mut Result<Vec<Monitor>, AppError>).cast());
        } else {
            dispatch_sync_f(
                std::ptr::addr_of!(_dispatch_main_q).cast(),
                (&mut output as *mut Result<Vec<Monitor>, AppError>).cast(),
                read_screens,
            );
        }
    }
    output
}
unsafe extern "C" fn read_screens(context: *mut c_void) {
    // No panic across the C dispatch callback.
    let output = unsafe { &mut *context.cast::<Result<Vec<Monitor>, AppError>>() };
    *output = unsafe { collect() };
}
unsafe fn collect() -> Result<Vec<Monitor>, AppError> {
    unsafe {
        let pool = send_obj(
            send_obj(objc_getClass(c"NSAutoreleasePool".as_ptr()), c"alloc"),
            c"init",
        );
        let result = collect_inner();
        send_void(pool, c"drain");
        result
    }
}
unsafe fn collect_inner() -> Result<Vec<Monitor>, AppError> {
    unsafe {
        let screens = send_obj(objc_getClass(c"NSScreen".as_ptr()), c"screens");
        let count = send_number(screens, c"count");
        let at: unsafe extern "C" fn(Obj, Obj, usize) -> Obj =
            std::mem::transmute(objc_msgSend as *const ());
        let lookup: unsafe extern "C" fn(Obj, Obj, Obj) -> Obj =
            std::mem::transmute(objc_msgSend as *const ());
        let double: unsafe extern "C" fn(Obj, Obj) -> f64 =
            std::mem::transmute(objc_msgSend as *const ());
        let main = CGMainDisplayID();
        let main_height = CGDisplayBounds(main).size.height;
        let key = string("NSScreenNumber");
        let mut monitors = Vec::new();
        for index in 0..count {
            let screen = at(screens, sel_registerName(c"objectAtIndex:".as_ptr()), index);
            let description = send_obj(screen, c"deviceDescription");
            let number = lookup(
                description,
                sel_registerName(c"objectForKey:".as_ptr()),
                key.0.cast_mut(),
            );
            let display = send_number(number, c"unsignedIntValue") as u32;
            let scale = double(screen, sel_registerName(c"backingScaleFactor".as_ptr()));
            let visible = geometry::cocoa_to_cg(send_frame(screen, c"visibleFrame"), main_height);
            let bounds = geometry::physical(CGDisplayBounds(display), scale);
            let work_area = geometry::physical(visible, scale);
            let (Some(bounds), Some(work_area)) = (bounds, work_area) else {
                return Err(error(
                    ErrorCode::BackendUnavailable,
                    "Invalid NSScreen geometry or backing scale",
                    None,
                ));
            };
            monitors.push(Monitor {
                id: format!("mac-display-{display}"),
                name: format!("Display {display}"),
                bounds,
                work_area,
                scale_factor: scale,
                primary: display == main,
            });
        }
        if monitors.is_empty() {
            return Err(error(
                ErrorCode::BackendUnavailable,
                "No NSScreen work areas available",
                None,
            ));
        }
        if monitors
            .iter()
            .any(|m| (m.scale_factor - monitors[0].scale_factor).abs() > 0.001)
        {
            return Err(error(
                ErrorCode::UnsupportedSession,
                "Mixed backing-scale displays are not supported yet; use displays with the same scale. No windows were moved.",
                None,
            ));
        }
        Ok(monitors)
    }
}
