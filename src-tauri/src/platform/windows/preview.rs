//! DWM resources only: never save, move, restore, or activate a source window.
use super::*;
pub use crate::preview::{PreviewSlot, PreviewState, PreviewStatus};
use std::collections::HashSet;

struct Thumbnail {
    handle: isize,
    // Keep release injectable so ownership tests never register real DWM resources.
    unregister: unsafe extern "system" fn(isize) -> i32,
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
        if let Some(destination) = self.previews.destination.take() {
            if self.alive(&destination) {
                unsafe { RemovePropW(destination.hwnd as HWND, self.property.as_ptr()) };
            }
        }
    }

    pub(super) fn prune_previews(&mut self) {
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
            .filter(|id| self.entry(id).is_err())
            .cloned()
            .collect();
        for id in stale {
            self.previews.thumbnails.remove(&id);
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
            });
        }
        self.previews
            .thumbnails
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
                    Ok(state) => (state, String::new()),
                    Err((state, message)) => (state, message),
                };
                if state != PreviewState::Ready {
                    self.previews.thumbnails.remove(&slot.window_id);
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
        if hr < 0
            || cloaked != 0
            || unsafe { IsIconic(source) != 0 || IsWindowVisible(source) == 0 }
        {
            return Err((
                PreviewState::Unavailable,
                "Source is minimized, hidden, or unavailable; live content cannot be guaranteed"
                    .into(),
            ));
        }
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
