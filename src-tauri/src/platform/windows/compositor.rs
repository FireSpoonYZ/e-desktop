//! Layout animations, niri style: each monitor draws only its own layout.
//!
//! Moving real windows frame by frame cannot keep them on their monitor: window regions do
//! not clip DirectComposition content (Chromium, Electron, Terminal), so a column sliding in
//! at a screen edge was drawn across the neighbouring monitor. While an animation runs, an
//! opaque, non-activating overlay covers every animating monitor and shows DWM live
//! thumbnails of its windows at their frame positions, cropped to that monitor. The real
//! windows move once, underneath, straight to their targets. Mouse messages in the animated
//! viewport are absorbed until handoff, not passed to HWNDs at different hit positions.
//! Keyboard input and global shortcuts are unaffected; existing mouse capture/hooks are not
//! intercepted by this window-local policy.
use super::preview::Thumbnail;
use super::*;
use crate::animation::Sprite;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

#[derive(Default)]
pub(super) struct Compositor {
    /// One overlay per monitor area, kept hidden between animations.
    overlays: Vec<(Rect, usize)>,
    /// Window id -> (overlay it is drawn in, thumbnail).
    thumbnails: HashMap<String, (usize, Thumbnail)>,
    /// Real windows already moved to their target during this animation.
    placed: HashMap<String, NativeAction>,
    active: bool,
}

impl Drop for Compositor {
    fn drop(&mut self) {
        for &(_, h) in &self.overlays {
            unsafe { ShowWindow(h as HWND, SW_HIDE) };
        }
        self.thumbnails.clear();
        for (_, h) in self.overlays.drain(..) {
            unsafe {
                DestroyWindow(h as HWND);
            }
        }
    }
}

impl Compositor {
    fn retain(&mut self, sprites: &[Sprite]) {
        let requested: HashSet<_> = sprites.iter().map(|s| s.window_id.as_str()).collect();
        self.thumbnails
            .retain(|id, _| requested.contains(id.as_str()));
        self.placed.retain(|id, _| requested.contains(id.as_str()));
    }

    fn clear(&mut self) {
        self.thumbnails.clear();
        self.placed.clear();
        self.active = false;
    }
}

const CLASS: &str = "e-desktop-compositor";

fn thumbnail_result(hr: i32, operation: &str, id: &str) -> Result<(), AppError> {
    if hr < 0 {
        Err(error(
            ErrorCode::OperationDenied,
            format!("{operation} (compositor) failed (HRESULT {hr:#x})"),
            Some(id),
        ))
    } else {
        Ok(())
    }
}

fn overlay_message(message: u32) -> Option<LRESULT> {
    match message {
        WM_NCHITTEST => Some(HTCLIENT as LRESULT),
        WM_MOUSEACTIVATE => Some(MA_NOACTIVATEANDEAT as LRESULT),
        WM_MOUSEFIRST..=WM_MOUSELAST => Some(0),
        _ => None,
    }
}

unsafe extern "system" fn overlay_proc(h: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    overlay_message(message).unwrap_or_else(|| unsafe { DefWindowProcW(h, message, w, l) })
}

/// The overlays live on the controller thread, which has no message loop of its own.
fn pump() {
    let mut message: MSG = unsafe { zeroed() };
    while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
        unsafe {
            DispatchMessageW(&message);
        }
    }
}

/// Map `source` onto `dest` (stretched), cropped to `clip`: the destination part that stays
/// visible and the matching part of the source.
fn project(source: RECT, dest: RECT, clip: RECT) -> Option<(RECT, RECT)> {
    let visible = intersect(dest, clip)?;
    let (dw, dh) = (
        i64::from(dest.right) - i64::from(dest.left),
        i64::from(dest.bottom) - i64::from(dest.top),
    );
    let (sw, sh) = (
        i64::from(source.right) - i64::from(source.left),
        i64::from(source.bottom) - i64::from(source.top),
    );
    if dw <= 0 || dh <= 0 || sw <= 0 || sh <= 0 {
        return None;
    }
    let x =
        |v: i32| (i64::from(source.left) + (i64::from(v) - i64::from(dest.left)) * sw / dw) as i32;
    let y =
        |v: i32| (i64::from(source.top) + (i64::from(v) - i64::from(dest.top)) * sh / dh) as i32;
    Some((
        visible,
        RECT {
            left: x(visible.left),
            top: y(visible.top),
            right: x(visible.right),
            bottom: y(visible.bottom),
        },
    ))
}

impl Backend {
    pub fn composing(&self) -> bool {
        self.compositor.active
    }

    fn overlay(&mut self, bounds: Rect) -> Result<HWND, AppError> {
        if let Some(&(_, h)) = self.compositor.overlays.iter().find(|(b, _)| *b == bounds) {
            return Ok(h as HWND);
        }
        native(bounds)?;
        let class = wide(CLASS);
        let instance = unsafe { GetModuleHandleW(null()) };
        let registered = WNDCLASSW {
            lpfnWndProc: Some(overlay_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            // Where no window is drawn (never between tiled columns), like niri's backdrop.
            hbrBackground: unsafe { CreateSolidBrush(0x0026_2626) },
            ..unsafe { zeroed() }
        };
        if registered.hbrBackground.is_null() {
            return Err(failed("CreateSolidBrush (compositor overlay)", None));
        }
        if unsafe { RegisterClassW(&registered) } == 0 {
            let failure = unsafe { GetLastError() };
            unsafe { DeleteObject(registered.hbrBackground) };
            if failure != ERROR_CLASS_ALREADY_EXISTS {
                return Err(error(
                    ErrorCode::OperationDenied,
                    format!("RegisterClassW (compositor overlay) failed (Win32 {failure})"),
                    None,
                ));
            }
        }
        let h = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                class.as_ptr(),
                null(),
                WS_POPUP,
                bounds.x,
                bounds.y,
                bounds.width as i32,
                bounds.height as i32,
                null_mut(),
                null_mut(),
                instance,
                null(),
            )
        };
        if h.is_null() {
            return Err(failed("CreateWindowExW (compositor overlay)", None));
        }
        if unsafe { SetLayeredWindowAttributes(h, 0, 255, LWA_ALPHA) } == 0 {
            let failure = failed("SetLayeredWindowAttributes (compositor overlay)", None);
            unsafe { DestroyWindow(h) };
            return Err(failure);
        }
        self.compositor.overlays.push((bounds, h as usize));
        Ok(h)
    }

    /// Prepare valid pictures before showing an overlay, then move live sources underneath.
    pub fn compose(&mut self, sprites: &[Sprite]) -> Result<(), AppError> {
        let result = self.compose_frame(sprites);
        if result.is_err() {
            self.compose_end();
        }
        result
    }

    fn compose_frame(&mut self, sprites: &[Sprite]) -> Result<(), AppError> {
        let _dpi = DpiScope::enter()?;
        let fresh = !self.compositor.active;
        let mut areas: Vec<Rect> = vec![];
        for s in sprites {
            if !areas.contains(&s.bounds) {
                areas.push(s.bounds);
            }
        }
        // A monitor that dropped out of this frame (full-display pause) must not keep a
        // topmost overlay. Other monitors in `areas` stay up, so their animation does not snap.
        for &(bounds, h) in &self.compositor.overlays {
            if !areas.contains(&bounds) {
                unsafe {
                    ShowWindow(h as HWND, SW_HIDE);
                }
            }
        }
        if areas.is_empty() {
            self.compose_end();
            return Ok(());
        }
        self.compositor.retain(sprites);
        for &area in &areas {
            self.overlay(area)?;
        }
        // Iconic sources have no live picture. Restore only those early-safe sources before
        // drawing. They can briefly expose the target until DWM supplies their first picture;
        // visible sources must instead move only after the old-frame overlay covers them.
        self.place_sprites(sprites, true)?;
        self.draw(sprites)?;
        self.compositor.active = true;
        for area in areas {
            let h = self.overlay(area)?;
            if unsafe { IsWindowVisible(h) } == 0 {
                if unsafe {
                    SetWindowPos(
                        h,
                        HWND_TOPMOST,
                        area.x,
                        area.y,
                        area.width as i32,
                        area.height as i32,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    )
                } == 0
                {
                    return Err(failed("SetWindowPos (compositor overlay)", None));
                }
                unsafe { UpdateWindow(h) };
            }
        }
        pump();
        if fresh {
            // The covered picture must be on screen before anything moves underneath.
            let hr = unsafe { DwmFlush() };
            if hr < 0 {
                return Err(error(
                    ErrorCode::OperationDenied,
                    format!("DwmFlush (compositor) failed (HRESULT {hr:#x})"),
                    None,
                ));
            }
        }
        if self.place_sprites(sprites, false)? {
            self.draw(sprites)?;
        }
        pump();
        Ok(())
    }

    fn place_sprites(
        &mut self,
        sprites: &[Sprite],
        restoring_only: bool,
    ) -> Result<bool, AppError> {
        let mut moved = false;
        for action in sprites.iter().filter_map(|s| s.early.as_ref()) {
            let NativeAction::Placement { window_id, .. } = action else {
                continue;
            };
            if self.compositor.placed.get(window_id) == Some(action) {
                continue;
            }
            // Source destruction is handled by draw; do not retry placement on a reused HWND.
            let Ok(entry) = self.entry(window_id) else {
                self.compositor.thumbnails.remove(window_id);
                self.compositor.placed.remove(window_id);
                continue;
            };
            if restoring_only && unsafe { IsIconic(entry.hwnd as HWND) } == 0 {
                continue;
            }
            // apply restores a refused source. Uncover it rather than hiding that error behind
            // a stale thumbnail until the final frame; the caller can fall back to native frames.
            self.apply(std::slice::from_ref(action))?;
            self.compositor
                .placed
                .insert(window_id.clone(), action.clone());
            moved = true;
        }
        Ok(moved)
    }

    fn draw(&mut self, sprites: &[Sprite]) -> Result<(), AppError> {
        for s in sprites {
            let Some(&(_, overlay)) = self
                .compositor
                .overlays
                .iter()
                .find(|(b, _)| *b == s.bounds)
            else {
                continue;
            };
            let overlay = overlay as HWND;
            let Ok(entry) = self.entry(&s.window_id) else {
                self.compositor.thumbnails.remove(&s.window_id);
                self.compositor.placed.remove(&s.window_id);
                continue;
            };
            let (source, pad) = (entry.hwnd as HWND, entry.placed_pad);
            let (x, y) = (
                i64::from(s.rect.x) - i64::from(s.bounds.x),
                i64::from(s.rect.y) - i64::from(s.bounds.y),
            );
            let clamp = |v: i64| v.clamp(i32::MIN.into(), i32::MAX.into()) as i32;
            let dest = RECT {
                left: clamp(x),
                top: clamp(y),
                right: clamp(x + i64::from(s.rect.width)),
                bottom: clamp(y + i64::from(s.rect.height)),
            };
            let client = RECT {
                left: 0,
                top: 0,
                right: s.bounds.width as i32,
                bottom: s.bounds.height as i32,
            };
            let on_screen = intersect(dest, client).is_some();
            if unsafe { IsIconic(source) != 0 || IsWindowVisible(source) == 0 } {
                self.compositor.thumbnails.remove(&s.window_id);
                self.compositor.placed.remove(&s.window_id);
                if on_screen {
                    return Err(error(
                        ErrorCode::OperationDenied,
                        "Compositor source is minimized or hidden",
                        Some(&s.window_id),
                    ));
                }
                continue;
            }
            // Keep existing off-screen registrations hidden, but do not create new ones.
            if !on_screen && !self.compositor.thumbnails.contains_key(&s.window_id) {
                continue;
            }
            if self
                .compositor
                .thumbnails
                .get(&s.window_id)
                .map(|(o, _)| *o)
                != Some(overlay as usize)
            {
                self.compositor.thumbnails.remove(&s.window_id);
                let mut handle = 0;
                thumbnail_result(
                    unsafe { DwmRegisterThumbnail(overlay, source, &mut handle) },
                    "DwmRegisterThumbnail",
                    &s.window_id,
                )?;
                self.compositor.thumbnails.insert(
                    s.window_id.clone(),
                    (
                        overlay as usize,
                        Thumbnail {
                            handle,
                            unregister: DwmUnregisterThumbnail,
                        },
                    ),
                );
            }
            let handle = self.compositor.thumbnails[&s.window_id].1.handle;
            let mut size: SIZE = unsafe { zeroed() };
            thumbnail_result(
                unsafe { DwmQueryThumbnailSourceSize(handle, &mut size) },
                "DwmQueryThumbnailSourceSize",
                &s.window_id,
            )?;
            if size.cx <= 0 || size.cy <= 0 {
                return Err(error(
                    ErrorCode::OperationDenied,
                    "Compositor source has no drawable size",
                    Some(&s.window_id),
                ));
            }
            // The thumbnail is the whole outer frame; frames are the visible bounds.
            let pad = pad
                .or_else(|| Some(frame_pad(outer_frame(source)?, dwm_frame(source)?)))
                .unwrap_or_default();
            let visible = RECT {
                left: pad[0],
                top: pad[1],
                right: size.cx - pad[2],
                bottom: size.cy - pad[3],
            };
            let mut properties = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_VISIBLE,
                ..unsafe { zeroed() }
            };
            if let Some((destination, source)) = project(visible, dest, client) {
                properties = DWM_THUMBNAIL_PROPERTIES {
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
            } else if on_screen {
                return Err(error(
                    ErrorCode::OperationDenied,
                    "Compositor source has no drawable frame",
                    Some(&s.window_id),
                ));
            }
            thumbnail_result(
                unsafe { DwmUpdateThumbnailProperties(handle, &properties) },
                "DwmUpdateThumbnailProperties",
                &s.window_id,
            )?;
            // The cookie/PID/HWND check must still hold after the DWM calls.
            if self.entry(&s.window_id).is_err() {
                self.compositor.thumbnails.remove(&s.window_id);
                self.compositor.placed.remove(&s.window_id);
            }
        }
        Ok(())
    }

    /// Uncover the monitors. Call once the real windows hold their final placements.
    pub fn compose_end(&mut self) {
        if !self.compositor.active
            && self.compositor.thumbnails.is_empty()
            && self.compositor.placed.is_empty()
        {
            return;
        }
        // Cleanup also covers preparation failures before `active` was set.
        // Hide first: an overlay without its pictures would flash the backdrop.
        for &(_, h) in &self.compositor.overlays {
            unsafe {
                ShowWindow(h as HWND, SW_HIDE);
            }
        }
        self.compositor.clear();
        pump();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_absorbs_mouse_without_activation_or_keyboard_interception() {
        assert_eq!(overlay_message(WM_NCHITTEST), Some(HTCLIENT as LRESULT));
        assert_eq!(
            overlay_message(WM_MOUSEACTIVATE),
            Some(MA_NOACTIVATEANDEAT as LRESULT)
        );
        for message in [
            WM_MOUSEMOVE,
            WM_LBUTTONDOWN,
            WM_LBUTTONUP,
            WM_RBUTTONDOWN,
            WM_MOUSEWHEEL,
            WM_MOUSEHWHEEL,
            WM_XBUTTONDOWN,
        ] {
            assert_eq!(overlay_message(message), Some(0));
        }
        for message in [WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_HOTKEY, WM_PAINT] {
            assert_eq!(overlay_message(message), None);
        }
    }

    #[test]
    fn retarget_keeps_live_handles_and_cleanup_releases_once_even_before_active() {
        use std::sync::Mutex;
        static RELEASED: Mutex<Vec<isize>> = Mutex::new(Vec::new());
        unsafe extern "system" fn release(handle: isize) -> i32 {
            RELEASED.lock().unwrap().push(handle);
            0
        }
        let mut compositor = Compositor::default();
        let bounds = Rect {
            x: -1920,
            y: -100,
            width: 1920,
            height: 1080,
        };
        let action = NativeAction::Placement {
            window_id: "keep".into(),
            rect: bounds,
            clip: Some(bounds),
            minimized: false,
        };
        for (id, handle) in [("keep", 1), ("gone", 2)] {
            compositor.thumbnails.insert(
                id.into(),
                (
                    99,
                    Thumbnail {
                        handle,
                        unregister: release,
                    },
                ),
            );
        }
        compositor.placed.insert("keep".into(), action.clone());
        compositor.placed.insert("gone".into(), action.clone());
        let sprite = Sprite {
            window_id: "keep".into(),
            rect: bounds,
            bounds,
            early: Some(action.clone()),
        };
        compositor.retain(std::slice::from_ref(&sprite));
        compositor.retain(&[Sprite {
            rect: Rect { x: -1000, ..bounds },
            ..sprite
        }]);
        assert_eq!(*RELEASED.lock().unwrap(), vec![2]);
        assert_eq!(compositor.thumbnails["keep"].1.handle, 1);
        assert_eq!(compositor.placed.len(), 1);
        assert_eq!(compositor.placed["keep"], action);
        // Preparation can fail before active; those registrations still need releasing.
        assert!(!compositor.active);
        compositor.clear();
        compositor.clear();
        assert!(compositor.thumbnails.is_empty());
        assert!(compositor.placed.is_empty());
        compositor.active = true;
        compositor.clear();
        assert!(!compositor.active);
        drop(compositor);
        assert_eq!(*RELEASED.lock().unwrap(), vec![2, 1]);
    }

    #[test]
    fn thumbnail_errors_preserve_operation_and_source_identity() {
        assert!(thumbnail_result(0, "DwmUpdateThumbnailProperties", "a").is_ok());
        assert!(thumbnail_result(1, "DwmUpdateThumbnailProperties", "a").is_ok());
        let failure = thumbnail_result(0x80070057u32 as i32, "DwmUpdateThumbnailProperties", "a")
            .unwrap_err();
        assert_eq!(failure.code, ErrorCode::OperationDenied);
        assert_eq!(failure.window_id.as_deref(), Some("a"));
        assert!(failure.message.contains("DwmUpdateThumbnailProperties"));
        assert!(failure.message.contains("80070057"));
    }

    #[test]
    fn projection_rejects_empty_and_inverted_picture_or_frame() {
        let valid = RECT {
            left: 0,
            top: 0,
            right: 100,
            bottom: 100,
        };
        for invalid in [
            RECT { right: 0, ..valid },
            RECT { left: 101, ..valid },
            RECT { bottom: 0, ..valid },
            RECT { top: 101, ..valid },
        ] {
            assert!(project(invalid, valid, valid).is_none());
            assert!(project(valid, invalid, valid).is_none());
            assert!(project(valid, valid, invalid).is_none());
        }
    }

    #[test]
    fn projection_stretches_and_crops_the_source_with_the_destination() {
        let source = RECT {
            left: 9,
            top: 0,
            right: 1929,
            bottom: 1080,
        };
        let clip = RECT {
            left: 0,
            top: 0,
            right: 1000,
            bottom: 1080,
        };
        // Same size, half off the left edge: the right half of the source stays.
        let dest = RECT {
            left: -960,
            top: 0,
            right: 960,
            bottom: 1080,
        };
        let (d, s) = project(source, dest, clip).unwrap();
        assert_eq!((d.left, d.right), (0, 960));
        assert_eq!((s.left, s.right, s.top, s.bottom), (969, 1929, 0, 1080));
        // Half size: the whole source is squeezed in.
        let dest = RECT {
            left: 0,
            top: 0,
            right: 960,
            bottom: 540,
        };
        let (_, s) = project(source, dest, clip).unwrap();
        assert_eq!((s.left, s.right, s.bottom), (9, 1929, 1080));
        // Entirely off the monitor: nothing to draw.
        let dest = RECT {
            left: -2000,
            top: 0,
            right: -80,
            bottom: 1080,
        };
        assert!(project(source, dest, clip).is_none());
    }
}
