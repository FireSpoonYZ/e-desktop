//! Layout animations, niri style: each monitor draws only its own layout.
//!
//! Moving real windows frame by frame cannot keep them on their monitor: window regions do
//! not clip DirectComposition content (Chromium, Electron, Terminal), so a column sliding in
//! at a screen edge was drawn across the neighbouring monitor. While an animation runs, an
//! opaque, click-through overlay covers every animating monitor and shows DWM live
//! thumbnails of its windows at their frame positions, cropped to that monitor. The real
//! windows move once, underneath, straight to their targets.
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
        self.thumbnails.clear();
        for (_, h) in self.overlays.drain(..) {
            unsafe {
                DestroyWindow(h as HWND);
            }
        }
    }
}

const CLASS: &str = "e-desktop-compositor";

unsafe extern "system" fn overlay_proc(h: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        _ => unsafe { DefWindowProcW(h, message, w, l) },
    }
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
        unsafe {
            RegisterClassW(&registered); // Fails harmlessly once registered.
        }
        let h = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT,
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
        unsafe {
            SetLayeredWindowAttributes(h, 0, 255, LWA_ALPHA);
        }
        self.compositor.overlays.push((bounds, h as usize));
        Ok(h)
    }

    /// Draw the animation frame `sprites`; the first call covers the monitors before any real
    /// window moves, then moves the windows that may take their targets right away.
    pub fn compose(&mut self, sprites: &[Sprite]) -> Result<(), AppError> {
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
        for area in areas {
            let h = self.overlay(area)?;
            if unsafe { IsWindowVisible(h) } == 0 {
                unsafe {
                    SetWindowPos(
                        h,
                        HWND_TOPMOST,
                        area.x,
                        area.y,
                        area.width as i32,
                        area.height as i32,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                    UpdateWindow(h);
                }
            }
        }
        self.compositor.active = true;
        self.draw(sprites);
        pump();
        if fresh {
            // The covered picture must be on screen before anything moves underneath.
            unsafe {
                DwmFlush();
            }
        }
        let mut moved = false;
        for action in sprites.iter().filter_map(|s| s.early.as_ref()) {
            let NativeAction::Placement { window_id, .. } = action else {
                continue;
            };
            if self.compositor.placed.get(window_id) == Some(action) {
                continue;
            }
            // A refusal is reported by the final placement at the end of the animation.
            if self.apply(std::slice::from_ref(action)).is_ok() {
                self.compositor
                    .placed
                    .insert(window_id.clone(), action.clone());
                moved = true;
            }
        }
        if moved {
            self.draw(sprites); // Windows restored just now have a picture from here on.
        }
        pump();
        Ok(())
    }

    fn draw(&mut self, sprites: &[Sprite]) {
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
                continue;
            };
            let (source, pad) = (entry.hwnd as HWND, entry.placed_pad);
            // Minimized windows have no picture; they are off screen in this frame anyway.
            if unsafe { IsIconic(source) } != 0 {
                self.compositor.thumbnails.remove(&s.window_id);
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
                if unsafe { DwmRegisterThumbnail(overlay, source, &mut handle) } < 0 {
                    continue;
                }
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
            if unsafe { DwmQueryThumbnailSourceSize(handle, &mut size) } < 0
                || size.cx <= 0
                || size.cy <= 0
            {
                continue;
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
            }
            unsafe {
                DwmUpdateThumbnailProperties(handle, &properties);
            }
        }
    }

    /// Uncover the monitors. Call once the real windows hold their final placements.
    pub fn compose_end(&mut self) {
        if !self.compositor.active {
            return;
        }
        // Hide first: an overlay without its pictures would flash the backdrop.
        for &(_, h) in &self.compositor.overlays {
            unsafe {
                ShowWindow(h as HWND, SW_HIDE);
            }
        }
        self.compositor.thumbnails.clear();
        self.compositor.placed.clear();
        self.compositor.active = false;
        pump();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
