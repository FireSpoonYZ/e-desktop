//! Narrow macOS ABI declarations. All owned CF references stay on the backend thread.
use std::ffi::{c_char, c_void};
pub type Ref = *const c_void;
pub type Obj = *mut c_void;
pub use super::geometry::{Frame, Point, Size};
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFRelease(value: Ref);
    pub fn CFRetain(value: Ref) -> Ref;
    pub fn CFEqual(a: Ref, b: Ref) -> u8;
    pub fn CFGetTypeID(value: Ref) -> usize;
    pub fn CFStringGetTypeID() -> usize;
    pub fn CFArrayGetTypeID() -> usize;
    pub fn CFBooleanGetTypeID() -> usize;
    pub fn CFBooleanGetValue(value: Ref) -> u8;
    pub fn CFStringCreateWithCString(allocator: Ref, text: *const c_char, encoding: u32) -> Ref;
    pub fn CFStringGetLength(value: Ref) -> isize;
    pub fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    pub fn CFStringGetCString(value: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
    pub fn CFArrayGetCount(value: Ref) -> isize;
    pub fn CFArrayGetValueAtIndex(value: Ref, index: isize) -> Ref;
    pub static kCFBooleanTrue: Ref;
    pub static kCFBooleanFalse: Ref;
}
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    pub fn AXIsProcessTrusted() -> u8;
    pub fn AXUIElementCreateApplication(pid: i32) -> Ref;
    pub fn AXUIElementCreateSystemWide() -> Ref;
    pub fn AXUIElementCopyAttributeValue(element: Ref, attribute: Ref, value: *mut Ref) -> i32;
    pub fn AXUIElementSetAttributeValue(element: Ref, attribute: Ref, value: Ref) -> i32;
    pub fn AXUIElementIsAttributeSettable(element: Ref, attribute: Ref, value: *mut u8) -> i32;
    pub fn AXUIElementPerformAction(element: Ref, action: Ref) -> i32;
    pub fn AXUIElementSetMessagingTimeout(element: Ref, timeout: f32) -> i32;
    pub fn AXUIElementGetPid(element: Ref, pid: *mut i32) -> i32;
    pub fn AXValueCreate(kind: u32, value: *const c_void) -> Ref;
    pub fn AXValueGetTypeID() -> usize;
    pub fn AXValueGetValue(value: Ref, kind: u32, output: *mut c_void) -> u8;
    pub fn CGMainDisplayID() -> u32;
    pub fn CGDisplayBounds(display: u32) -> Frame;
}
#[link(name = "proc")]
unsafe extern "C" {
    pub fn proc_listallpids(buffer: *mut c_void, size: i32) -> i32;
}
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}
#[link(name = "objc")]
unsafe extern "C" {
    pub fn objc_getClass(name: *const c_char) -> Obj;
    pub fn sel_registerName(name: *const c_char) -> Obj;
    pub fn objc_msgSend();
    #[cfg(target_arch = "x86_64")]
    pub fn objc_msgSend_stret();
}
unsafe extern "C" {
    pub fn pthread_main_np() -> i32;
    pub static _dispatch_main_q: u8;
    pub fn dispatch_sync_f(
        queue: *const c_void,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
}

/// Create/Copy results own one retain; Get results must be retained before storing.
/// The Rc marker intentionally prevents moving AX/CF objects to another thread.
pub struct Owned(pub Ref, std::marker::PhantomData<std::rc::Rc<()>>);
impl Owned {
    pub unsafe fn from_create(value: Ref) -> Option<Self> {
        (!value.is_null()).then_some(Self(value, std::marker::PhantomData))
    }
    pub unsafe fn retained(value: Ref) -> Self {
        unsafe { Self(CFRetain(value), std::marker::PhantomData) }
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}
pub fn string(text: &str) -> Owned {
    let text = std::ffi::CString::new(text).expect("static AX attribute");
    unsafe {
        Owned::from_create(CFStringCreateWithCString(
            std::ptr::null(),
            text.as_ptr(),
            0x08000100,
        ))
        .expect("CFString allocation")
    }
}
pub fn text(value: Ref) -> Option<String> {
    unsafe {
        if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        let size = CFStringGetMaximumSizeForEncoding(CFStringGetLength(value), 0x08000100) + 1;
        let mut buffer = vec![0u8; usize::try_from(size).ok()?];
        if CFStringGetCString(value, buffer.as_mut_ptr().cast(), size, 0x08000100) == 0 {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(buffer.as_ptr().cast())
                .to_string_lossy()
                .into_owned(),
        )
    }
}
pub fn attribute(element: Ref, name: &str) -> Result<Owned, i32> {
    let name = string(name);
    let mut value = std::ptr::null();
    let error = unsafe { AXUIElementCopyAttributeValue(element, name.0, &mut value) };
    if error != 0 {
        return Err(error);
    }
    unsafe { Owned::from_create(value).ok_or(-25212) }
}
pub fn set(element: Ref, name: &str, value: Ref) -> Result<(), i32> {
    let name = string(name);
    result(unsafe { AXUIElementSetAttributeValue(element, name.0, value) })
}
pub fn action(element: Ref, name: &str) -> Result<(), i32> {
    let name = string(name);
    result(unsafe { AXUIElementPerformAction(element, name.0) })
}
pub fn result(error: i32) -> Result<(), i32> {
    if error == 0 { Ok(()) } else { Err(error) }
}
pub fn settable(element: Ref, name: &str) -> bool {
    let name = string(name);
    let mut value = 0;
    unsafe { AXUIElementIsAttributeSettable(element, name.0, &mut value) == 0 && value != 0 }
}
pub fn boolean(element: Ref, name: &str) -> Result<bool, i32> {
    let value = attribute(element, name)?;
    unsafe {
        if CFGetTypeID(value.0) != CFBooleanGetTypeID() {
            return Err(-25200);
        }
        Ok(CFBooleanGetValue(value.0) != 0)
    }
}
pub fn set_boolean(element: Ref, name: &str, value: bool) -> Result<(), i32> {
    set(element, name, unsafe {
        if value {
            kCFBooleanTrue
        } else {
            kCFBooleanFalse
        }
    })
}
pub fn geometry(element: Ref) -> Result<Frame, i32> {
    let point = attribute(element, "AXPosition")?;
    let size = attribute(element, "AXSize")?;
    let mut frame = Frame::default();
    unsafe {
        if CFGetTypeID(point.0) != AXValueGetTypeID()
            || CFGetTypeID(size.0) != AXValueGetTypeID()
            || AXValueGetValue(point.0, 1, (&mut frame.origin as *mut Point).cast()) == 0
            || AXValueGetValue(size.0, 2, (&mut frame.size as *mut Size).cast()) == 0
        {
            return Err(-25200);
        }
    }
    Ok(frame)
}
pub fn set_geometry(element: Ref, frame: Frame) -> Result<(), i32> {
    unsafe {
        let point = Owned::from_create(AXValueCreate(1, (&frame.origin as *const Point).cast()))
            .ok_or(-25200)?;
        let size = Owned::from_create(AXValueCreate(2, (&frame.size as *const Size).cast()))
            .ok_or(-25200)?;
        // Position again after resize: some applications constrain a resize at the old origin.
        set(element, "AXPosition", point.0)?;
        set(element, "AXSize", size.0)?;
        set(element, "AXPosition", point.0)
    }
}

pub unsafe fn send_obj(receiver: Obj, selector: &std::ffi::CStr) -> Obj {
    let f: unsafe extern "C" fn(Obj, Obj) -> Obj =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(receiver, sel_registerName(selector.as_ptr())) }
}
pub unsafe fn send_void(receiver: Obj, selector: &std::ffi::CStr) {
    let f: unsafe extern "C" fn(Obj, Obj) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(receiver, sel_registerName(selector.as_ptr())) }
}
pub unsafe fn send_number(receiver: Obj, selector: &std::ffi::CStr) -> usize {
    let f: unsafe extern "C" fn(Obj, Obj) -> usize =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(receiver, sel_registerName(selector.as_ptr())) }
}
pub unsafe fn send_frame(receiver: Obj, selector: &std::ffi::CStr) -> Frame {
    #[cfg(target_arch = "aarch64")]
    {
        let f: unsafe extern "C" fn(Obj, Obj) -> Frame =
            unsafe { std::mem::transmute(objc_msgSend as *const ()) };
        unsafe { f(receiver, sel_registerName(selector.as_ptr())) }
    }
    #[cfg(target_arch = "x86_64")]
    {
        let f: unsafe extern "C" fn(*mut Frame, Obj, Obj) =
            unsafe { std::mem::transmute(objc_msgSend_stret as *const ()) };
        let mut frame = Frame::default();
        unsafe { f(&mut frame, receiver, sel_registerName(selector.as_ptr())) };
        frame
    }
}
