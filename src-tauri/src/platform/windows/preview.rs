//! Live DWM thumbnails and click-through static fallback surfaces. Never mutate a source.
use super::snapshot::{Dib, Frame, Identity, bitmap_info};
use super::*;
pub use crate::preview::{PreviewSlot, PreviewState, PreviewStatus};
use std::collections::HashSet;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

pub(super) struct Thumbnail {
    pub(super) handle: isize,
    // Keep release injectable so ownership tests never register real DWM resources.
    pub(super) unregister: unsafe extern "system" fn(isize) -> i32,
}
impl Drop for Thumbnail {
    fn drop(&mut self) {
        let hr = unsafe { (self.unregister)(self.handle) };
        if hr < 0 {
            eprintln!("DwmUnregisterThumbnail failed: {hr:#x}");
        }
    }
}

#[derive(Default)]
pub(super) struct Previews {
    destination: Option<Entry>,
    thumbnails: HashMap<String, Thumbnail>,
    snapshots: HashMap<String, SnapshotOverlay>,
}

const OVERLAY_BYTES: usize = 32 * 1024 * 1024;
const SURFACE_BYTES: usize = 4 * 1024 * 1024;

/// UpdateLayeredWindow copies the pixels; only the HWND (controller-thread owned) persists.
struct SnapshotOverlay {
    hwnd: usize,
    bytes: usize,
}
unsafe extern "system" fn snapshot_proc(h: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        _ => unsafe { DefWindowProcW(h, message, w, l) },
    }
}
fn overlay_bytes(dest: RECT) -> Option<usize> {
    let width = usize::try_from(i64::from(dest.right) - i64::from(dest.left)).ok()?;
    let height = usize::try_from(i64::from(dest.bottom) - i64::from(dest.top)).ok()?;
    let bytes = width.checked_mul(height)?.checked_mul(4)?;
    (bytes > 0 && bytes <= SURFACE_BYTES).then_some(bytes)
}
impl SnapshotOverlay {
    fn new(owner: HWND) -> Result<Self, String> {
        let class = wide("e-desktop-static-preview");
        let instance = unsafe { GetModuleHandleW(null()) };
        let registered = WNDCLASSW {
            lpfnWndProc: Some(snapshot_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..unsafe { zeroed() }
        };
        unsafe {
            RegisterClassW(&registered);
        }
        let h = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
                class.as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                owner,
                null_mut(),
                instance,
                null(),
            )
        };
        if h.is_null() {
            return Err("CreateWindowExW static preview failed".into());
        }
        Ok(Self {
            hwnd: h as usize,
            bytes: 0,
        })
    }

    fn paint(
        &mut self,
        owner: HWND,
        frame: &Frame,
        dest: RECT,
        src: RECT,
        bytes: usize,
    ) -> Result<(), String> {
        let size = SIZE {
            cx: dest.right - dest.left,
            cy: dest.bottom - dest.top,
        };
        let mut dib =
            Dib::new(size, SURFACE_BYTES).ok_or("Static preview bitmap allocation failed")?;
        let copied = unsafe {
            SetStretchBltMode(dib.dc, HALFTONE);
            SetBrushOrgEx(dib.dc, 0, 0, null_mut());
            StretchDIBits(
                dib.dc,
                0,
                0,
                size.cx,
                size.cy,
                src.left,
                src.top,
                src.right - src.left,
                src.bottom - src.top,
                frame.pixels.as_ptr().cast(),
                &bitmap_info(frame.size),
                DIB_RGB_COLORS,
                SRCCOPY,
            )
        };
        if copied == 0 || copied == GDI_ERROR as i32 {
            return Err("StretchDIBits static preview failed".into());
        }
        unsafe {
            GdiFlush();
        }
        for pixel in dib.pixels().chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        let mut point = POINT {
            x: dest.left,
            y: dest.top,
        };
        if unsafe { ClientToScreen(owner, &mut point) } == 0 {
            return Err("ClientToScreen static preview failed".into());
        }
        let origin = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let h = self.hwnd as HWND;
        if unsafe {
            UpdateLayeredWindow(
                h,
                null_mut(),
                &point,
                &size,
                dib.dc,
                &origin,
                0,
                &blend,
                ULW_ALPHA,
            )
        } == 0
        {
            return Err("UpdateLayeredWindow static preview failed".into());
        }
        // Owned, not globally topmost: stays above the overview, never activates/grabs input.
        if unsafe {
            SetWindowPos(
                h,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        } == 0
        {
            return Err("SetWindowPos static preview failed".into());
        }
        self.bytes = bytes;
        let mut message: MSG = unsafe { zeroed() };
        while unsafe { PeekMessageW(&mut message, h, 0, 0, PM_REMOVE) } != 0 {
            unsafe {
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }
}
impl Drop for SnapshotOverlay {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd as HWND);
        }
    }
}

fn dwm_result(hr: i32, operation: &str) -> Result<(), String> {
    if hr < 0 {
        Err(format!("{operation} failed (HRESULT {hr:#x})"))
    } else {
        Ok(())
    }
}

/// Letterbox before clipping; crop the corresponding source rather than squeezing it.
fn geometry(source: SIZE, target: RECT, clip: RECT) -> Option<(RECT, RECT)> {
    let (sw, sh) = (i64::from(source.cx), i64::from(source.cy));
    let (tw, th) = (
        i64::from(target.right) - i64::from(target.left),
        i64::from(target.bottom) - i64::from(target.top),
    );
    if sw <= 0 || sh <= 0 || tw <= 0 || th <= 0 {
        return None;
    }
    let (w, h) = if sw * th > sh * tw {
        (tw, (sh * tw / sw).max(1))
    } else {
        ((sw * th / sh).max(1), th)
    };
    let left = i64::from(target.left) + (tw - w) / 2;
    let top = i64::from(target.top) + (th - h) / 2;
    let fitted = RECT {
        left: left as i32,
        top: top as i32,
        right: (left + w) as i32,
        bottom: (top + h) as i32,
    };
    let destination = intersect(fitted, clip)?;
    let source = RECT {
        left: ((i64::from(destination.left) - left) * sw / w) as i32,
        top: ((i64::from(destination.top) - top) * sh / h) as i32,
        right: (((i64::from(destination.right) - left) * sw + w - 1) / w) as i32,
        bottom: (((i64::from(destination.bottom) - top) * sh + h - 1) / h) as i32,
    };
    Some((destination, source))
}

impl Backend {
    /// Windows-only capability; the host must not advertise it on X11/macOS.
    pub fn previews_available(&self) -> bool {
        let mut enabled = 0;
        unsafe { DwmIsCompositionEnabled(&mut enabled) >= 0 && enabled != 0 }
    }

    /// Call before hiding/closing/destroying the overview, on its owning controller thread.
    /// Also called by restore/drop, and when enumeration observes a dead destination.
    pub fn clear_previews(&mut self) {
        self.previews.thumbnails.clear();
        self.previews.snapshots.clear();
        if let Some(destination) = self.previews.destination.take() {
            if self.alive(&destination) {
                unsafe { RemovePropW(destination.hwnd as HWND, self.property.as_ptr()) };
            }
        }
    }

    pub(super) fn prune_previews(&mut self) {
        self.poll_snapshots();
        if self
            .previews
            .destination
            .as_ref()
            .is_some_and(|e| !self.alive(e) || unsafe { IsWindowVisible(e.hwnd as HWND) } == 0)
        {
            self.clear_previews();
            return;
        }
        let stale: Vec<_> = self
            .previews
            .thumbnails
            .keys()
            .chain(self.previews.snapshots.keys())
            .filter(|id| self.entry(id).is_err())
            .cloned()
            .collect();
        for id in stale {
            self.previews.thumbnails.remove(&id);
            self.previews.snapshots.remove(&id);
        }
    }

    /// `destination` MUST come from the trusted host's overview HWND, never IPC input.
    /// Slots contain registered window IDs only. Empty slots release all resources.
    /// The host must serialize sync/clear with overview visibility and discard old replies.
    pub fn sync_previews(
        &mut self,
        destination: usize,
        slots: &[PreviewSlot],
    ) -> Result<Vec<PreviewStatus>, AppError> {
        if slots.is_empty() {
            self.clear_previews();
            return Ok(Vec::new());
        }
        let _dpi = DpiScope::enter().inspect_err(|_| self.clear_previews())?;
        let hwnd = destination as HWND;
        let mut pid = 0;
        if unsafe {
            IsWindow(hwnd) == 0
                || IsWindowVisible(hwnd) == 0
                || GetWindowThreadProcessId(hwnd, &mut pid) == 0
                || pid != GetCurrentProcessId()
                || GetAncestor(hwnd, GA_ROOT) != hwnd
        } {
            self.clear_previews();
            return Err(error(
                ErrorCode::InvalidCommand,
                "Preview destination must be a visible application top-level window",
                None,
            ));
        }
        let mut requested = HashSet::new();
        if slots
            .iter()
            .any(|s| !requested.insert(s.window_id.as_str()))
        {
            self.clear_previews();
            return Err(error(
                ErrorCode::InvalidCommand,
                "Duplicate preview window ID",
                None,
            ));
        }
        if !self.previews_available() {
            self.clear_previews();
            return Ok(slots
                .iter()
                .map(|s| PreviewStatus {
                    window_id: s.window_id.clone(),
                    state: PreviewState::Unavailable,
                    message: "DWM composition is unavailable".into(),
                })
                .collect());
        }
        self.prune_previews();
        if self.previews.destination.as_ref().map(|e| e.hwnd) != Some(destination) {
            self.clear_previews();
            let cookie = self.next;
            self.next = self
                .next
                .checked_add(1)
                .ok_or_else(|| error(ErrorCode::BackendUnavailable, "Window ID exhausted", None))?;
            if unsafe { SetPropW(hwnd, self.property.as_ptr(), cookie as HANDLE) } == 0 {
                return Err(failed("SetPropW preview lifetime registration", None));
            }
            self.previews.destination = Some(Entry {
                hwnd: destination,
                pid,
                cookie,
                saved: None,
                minimized: false,
                decor: None,
                pads: vec![],
                placed_pad: None,
                region_box: None,
                at_bottom: false,
                min_width: None,
                reported_min: None,
                placed_visible: None,
            });
        }
        self.previews
            .thumbnails
            .retain(|id, _| requested.contains(id.as_str()));
        self.previews
            .snapshots
            .retain(|id, _| requested.contains(id.as_str()));
        let mut client: RECT = unsafe { zeroed() };
        if unsafe { GetClientRect(hwnd, &mut client) } == 0 {
            self.clear_previews();
            return Err(failed("GetClientRect", None));
        }
        Ok(slots
            .iter()
            .map(|slot| {
                let (state, message) = match self.update_preview(hwnd, slot, client) {
                    Ok(state) => (state, if self.previews.snapshots.contains_key(&slot.window_id) {
                        "Static snapshot from before hiding; content does not update while minimized".into()
                    } else { String::new() }),
                    Err((state, message)) => (state, message),
                };
                if state != PreviewState::Ready {
                    self.previews.thumbnails.remove(&slot.window_id);
                    self.previews.snapshots.remove(&slot.window_id);
                }
                PreviewStatus {
                    window_id: slot.window_id.clone(),
                    state,
                    message,
                }
            })
            .collect())
    }

    fn update_preview(
        &mut self,
        destination: HWND,
        slot: &PreviewSlot,
        client: RECT,
    ) -> Result<PreviewState, (PreviewState, String)> {
        // Shared lifetime check includes HWND, PID and the backend's per-instance cookie.
        let source = self
            .entry(&slot.window_id)
            .map_err(|e| (PreviewState::SourceGone, e.message))?
            .hwnd as HWND;
        let mut cloaked = 0u32;
        let hr = unsafe {
            DwmGetWindowAttribute(
                source,
                DWMWA_CLOAKED as u32,
                &mut cloaked as *mut _ as _,
                size_of::<u32>() as u32,
            )
        };
        if slot.rect.width == 0
            || slot.rect.height == 0
            || slot.clip.width == 0
            || slot.clip.height == 0
        {
            return Ok(PreviewState::Hidden);
        }
        let target = native(slot.rect).map_err(|e| (PreviewState::Failed, e.message))?;
        let clip = native(slot.clip).map_err(|e| (PreviewState::Failed, e.message))?;
        let Some(clip) = intersect(clip, client).and_then(|c| intersect(c, target)) else {
            return Ok(PreviewState::Hidden);
        };
        let failed = |message| (PreviewState::Failed, message);
        if hr < 0
            || cloaked != 0
            || unsafe { IsIconic(source) != 0 || IsWindowVisible(source) == 0 }
        {
            self.previews.thumbnails.remove(&slot.window_id);
            let key = Identity::of(
                self.entry(&slot.window_id)
                    .map_err(|e| (PreviewState::SourceGone, e.message))?,
            );
            let frame = self.snapshots.get(key).ok_or_else(|| {
                (
                    PreviewState::Unavailable,
                    "Source has no completed pre-hide snapshot; minimized content is not captured"
                        .into(),
                )
            })?;
            let Some((dest, src)) = geometry(frame.size, target, clip) else {
                return Ok(PreviewState::Hidden);
            };
            let used = self
                .previews
                .snapshots
                .iter()
                .filter(|(id, _)| *id != &slot.window_id)
                .map(|(_, s)| s.bytes)
                .sum::<usize>();
            let bytes = overlay_bytes(dest)
                .filter(|n| used + n <= OVERLAY_BYTES)
                .ok_or_else(|| failed("Static preview surface exceeds memory budget".into()))?;
            if !self.previews.snapshots.contains_key(&slot.window_id) {
                self.previews.snapshots.insert(
                    slot.window_id.clone(),
                    SnapshotOverlay::new(destination).map_err(failed)?,
                );
            }
            self.previews
                .snapshots
                .get_mut(&slot.window_id)
                .unwrap()
                .paint(destination, frame, dest, src, bytes)
                .map_err(failed)?;
            self.entry(&slot.window_id)
                .map_err(|e| (PreviewState::SourceGone, e.message))?;
            return Ok(PreviewState::Ready);
        }
        // Foreground/visible windows keep the original live DWM path.
        self.previews.snapshots.remove(&slot.window_id);
        if !self.previews.thumbnails.contains_key(&slot.window_id) {
            let mut handle = 0;
            dwm_result(
                unsafe { DwmRegisterThumbnail(destination, source, &mut handle) },
                "DwmRegisterThumbnail",
            )
            .map_err(failed)?;
            self.previews.thumbnails.insert(
                slot.window_id.clone(),
                Thumbnail {
                    handle,
                    unregister: DwmUnregisterThumbnail,
                },
            );
        }
        let handle = self.previews.thumbnails[&slot.window_id].handle;
        let mut size: SIZE = unsafe { zeroed() };
        dwm_result(
            unsafe { DwmQueryThumbnailSourceSize(handle, &mut size) },
            "DwmQueryThumbnailSourceSize",
        )
        .map_err(failed)?;
        if size.cx <= 0 || size.cy <= 0 {
            return Err((
                PreviewState::Unavailable,
                "Source has no drawable size".into(),
            ));
        }
        let Some((destination, source)) = geometry(size, target, clip) else {
            return Ok(PreviewState::Hidden);
        };
        let properties = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_RECTSOURCE
                | DWM_TNP_OPACITY
                | DWM_TNP_VISIBLE
                | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: destination,
            rcSource: source,
            opacity: 255,
            fVisible: 1,
            fSourceClientAreaOnly: 0,
        };
        dwm_result(
            unsafe { DwmUpdateThumbnailProperties(handle, &properties) },
            "DwmUpdateThumbnailProperties",
        )
        .map_err(failed)?;
        // Destruction/reuse during DWM calls must not be reported as a successful source.
        self.entry(&slot.window_id)
            .map_err(|e| (PreviewState::SourceGone, e.message))?;
        Ok(PreviewState::Ready)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn letterbox_and_crop_stay_inside_slot_and_preserve_source_coordinates() {
        let target = RECT {
            left: 10,
            top: 20,
            right: 210,
            bottom: 220,
        };
        let (dest, src) = geometry(SIZE { cx: 400, cy: 200 }, target, target).unwrap();
        assert_eq!(
            rect(dest),
            Rect {
                x: 10,
                y: 70,
                width: 200,
                height: 100
            }
        );
        assert_eq!(
            rect(src),
            Rect {
                x: 0,
                y: 0,
                width: 400,
                height: 200
            }
        );
        let clip = RECT {
            left: 60,
            top: 80,
            right: 200,
            bottom: 140,
        };
        let (dest, src) = geometry(SIZE { cx: 400, cy: 200 }, target, clip).unwrap();
        assert_eq!(rect(dest), rect(clip));
        assert_eq!(
            rect(src),
            Rect {
                x: 100,
                y: 20,
                width: 280,
                height: 120
            }
        );
        let (dest, _) = geometry(SIZE { cx: 100, cy: 400 }, target, target).unwrap();
        assert_eq!(
            rect(dest),
            Rect {
                x: 85,
                y: 20,
                width: 50,
                height: 200
            }
        );
        assert!(geometry(SIZE { cx: 0, cy: 1 }, target, target).is_none());
        assert!(
            geometry(
                SIZE { cx: 400, cy: 200 },
                target,
                RECT {
                    left: 0,
                    top: 0,
                    right: 5,
                    bottom: 5
                }
            )
            .is_none()
        );
        let huge = RECT {
            left: 0,
            top: 0,
            right: i32::MAX,
            bottom: i32::MAX,
        };
        assert!(
            geometry(
                SIZE {
                    cx: i32::MAX,
                    cy: 1
                },
                huge,
                huge
            )
            .is_some()
        );
    }

    #[test]
    fn static_surface_budget_uses_clipped_geometry_and_checks_overflow() {
        let target = RECT {
            left: -100,
            top: 20,
            right: 300,
            bottom: 220,
        };
        let clip = RECT {
            left: 0,
            top: 50,
            right: 200,
            bottom: 200,
        };
        let (dest, source) = geometry(SIZE { cx: 1024, cy: 512 }, target, clip).unwrap();
        assert_eq!(rect(dest), rect(clip));
        assert_eq!(
            (source.left, source.top, source.right, source.bottom),
            (256, 76, 768, 461)
        );
        assert_eq!(overlay_bytes(dest), Some(200 * 150 * 4));
        assert!(
            overlay_bytes(RECT {
                left: i32::MIN,
                top: i32::MIN,
                right: i32::MAX,
                bottom: i32::MAX
            })
            .is_none()
        );
        assert!(
            overlay_bytes(RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 10
            })
            .is_none()
        );
        assert!(
            overlay_bytes(RECT {
                left: 0,
                top: 0,
                right: 1025,
                bottom: 1024
            })
            .is_none()
        );
    }

    #[test]
    fn thumbnails_unregister_once_on_removal_clear_and_drop() {
        static RELEASED: Mutex<Vec<isize>> = Mutex::new(Vec::new());
        unsafe extern "system" fn release(handle: isize) -> i32 {
            RELEASED.lock().unwrap().push(handle);
            0
        }
        let mut previews = Previews::default();
        for handle in 1..=3 {
            previews.thumbnails.insert(
                handle.to_string(),
                Thumbnail {
                    handle,
                    unregister: release,
                },
            );
        }
        previews.thumbnails.retain(|id, _| id != "1");
        assert_eq!(*RELEASED.lock().unwrap(), vec![1]);
        previews.thumbnails.clear();
        previews.thumbnails.insert(
            "4".into(),
            Thumbnail {
                handle: 4,
                unregister: release,
            },
        );
        drop(previews);
        let mut released = RELEASED.lock().unwrap().clone();
        released.sort();
        assert_eq!(released, vec![1, 2, 3, 4]);
    }
}
