//! Static shell wallpaper only: no desktop capture, icons, or dynamic-wallpaper surfaces.
use super::*;
use std::{sync::Arc, time::SystemTime};
use windows::{
    Win32::{
        Foundation::{GENERIC_READ, RPC_E_CHANGED_MODE},
        Graphics::Imaging::*,
        System::Com::*,
        UI::Shell::*,
    },
    core::{PCWSTR, PWSTR},
};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_MONITORS: u32 = 32;

struct Apartment(bool);
impl Apartment {
    fn enter() -> Result<Self, AppError> {
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result == RPC_E_CHANGED_MODE {
            return Ok(Self(false)); // Use the controller's existing apartment.
        }
        result.ok().map_err(wallpaper_error)?;
        Ok(Self(true))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn wallpaper_error(e: impl std::fmt::Display) -> AppError {
    error(
        ErrorCode::OperationDenied,
        format!("Static desktop wallpaper unavailable (compositor): {e}"),
        None,
    )
}

struct TaskString(PWSTR);
impl Drop for TaskString {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.0.cast())) };
    }
}
impl TaskString {
    fn text(&self) -> Result<String, AppError> {
        unsafe { self.0.to_string() }.map_err(wallpaper_error)
    }
}

#[derive(PartialEq)]
struct MonitorWallpaper {
    bounds: Rect,
    path: String,
    modified: Option<SystemTime>,
    length: u64,
}

#[derive(PartialEq)]
struct Settings {
    monitors: Vec<MonitorWallpaper>,
    color: u32,
    position: DESKTOP_WALLPAPER_POSITION,
}

impl Settings {
    fn read() -> Result<Self, AppError> {
        let shell: IDesktopWallpaper =
            unsafe { CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL) }
                .map_err(wallpaper_error)?;
        let count = unsafe { shell.GetMonitorDevicePathCount() }.map_err(wallpaper_error)?;
        if count == 0 || count > MAX_MONITORS {
            return Err(wallpaper_error("monitor count outside 1..=32"));
        }
        let mut monitors = Vec::new();
        for i in 0..count {
            let id =
                TaskString(unsafe { shell.GetMonitorDevicePathAt(i) }.map_err(wallpaper_error)?);
            let r = unsafe { shell.GetMonitorRECT(PCWSTR(id.0.0)) }.map_err(wallpaper_error)?;
            // The shell also remembers detached monitors; GetMonitorRECT returns an empty
            // rectangle (S_FALSE) for them. They must not affect span/tile or block animation.
            if r.right <= r.left || r.bottom <= r.top {
                continue;
            }
            let bounds = rect(RECT {
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            });
            native(bounds)?;
            let path =
                TaskString(unsafe { shell.GetWallpaper(PCWSTR(id.0.0)) }.map_err(wallpaper_error)?)
                    .text()?;
            let metadata = if path.is_empty() {
                None
            } else {
                Some(std::fs::metadata(&path).map_err(wallpaper_error)?)
            };
            monitors.push(MonitorWallpaper {
                bounds,
                path,
                modified: metadata.as_ref().and_then(|m| m.modified().ok()),
                length: metadata.map_or(0, |m| m.len()),
            });
        }
        if monitors.is_empty() {
            return Err(wallpaper_error("no attached wallpaper monitors"));
        }
        Ok(Self {
            monitors,
            color: unsafe { shell.GetBackgroundColor() }
                .map_err(wallpaper_error)?
                .0,
            position: unsafe { shell.GetPosition() }.map_err(wallpaper_error)?,
        })
    }
}

struct Image {
    // Aligned packed DIB: header followed by top-down BGRX pixels.
    dib: Vec<u32>,
    width: i32,
    height: i32,
    // GDI handles are process-wide; Backend serializes painting and destruction.
    tile_brush: usize,
}
impl Drop for Image {
    fn drop(&mut self) {
        if self.tile_brush != 0 {
            unsafe { DeleteObject(self.tile_brush as HBRUSH) };
        }
    }
}
impl Image {
    fn decode(
        factory: &IWICImagingFactory,
        path: &str,
        budget: &mut usize,
        tiled: bool,
    ) -> Result<Self, AppError> {
        let path = wide(path);
        let decoder = unsafe {
            factory.CreateDecoderFromFilename(
                PCWSTR(path.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
        }
        .map_err(wallpaper_error)?;
        let frame = unsafe { decoder.GetFrame(0) }.map_err(wallpaper_error)?;
        let (mut width, mut height) = (0, 0);
        unsafe { frame.GetSize(&mut width, &mut height) }.map_err(wallpaper_error)?;
        let bytes = image_bytes(width, height, *budget)?;
        *budget -= bytes;
        let converter = unsafe { factory.CreateFormatConverter() }.map_err(wallpaper_error)?;
        unsafe {
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppBGR,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
        }
        .map_err(wallpaper_error)?;
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { zeroed() }
        };
        let header_words = size_of::<BITMAPINFOHEADER>() / 4;
        let mut dib = vec![0u32; header_words + bytes / 4];
        unsafe { std::ptr::write(dib.as_mut_ptr().cast::<BITMAPINFOHEADER>(), header) };
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(dib.as_mut_ptr().add(header_words).cast::<u8>(), bytes)
        };
        unsafe { converter.CopyPixels(null(), width * 4, pixels) }.map_err(wallpaper_error)?;
        let tile_brush = if tiled {
            let brush = unsafe { CreateDIBPatternBrushPt(dib.as_ptr().cast(), DIB_RGB_COLORS) };
            if brush.is_null() {
                return Err(wallpaper_error("CreateDIBPatternBrushPt failed"));
            }
            brush
        } else {
            null_mut()
        };
        Ok(Self {
            dib,
            width: width as i32,
            height: height as i32,
            tile_brush: tile_brush as usize,
        })
    }
}

fn image_bytes(width: u32, height: u32, budget: usize) -> Result<usize, AppError> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| wallpaper_error("decoded dimensions overflow"))?;
    if width == 0
        || height == 0
        || width > i32::MAX as u32
        || height > i32::MAX as u32
        || bytes > budget as u64
    {
        return Err(wallpaper_error(
            "decoded pixels exceed the 64 MiB cache limit or have invalid dimensions",
        ));
    }
    Ok(bytes as usize)
}

pub(super) struct Wallpaper {
    settings: Settings,
    images: HashMap<String, Image>,
    background_brush: usize,
}
impl Drop for Wallpaper {
    fn drop(&mut self) {
        unsafe { DeleteObject(self.background_brush as HBRUSH) };
    }
}
impl Wallpaper {
    /// Only called on a fresh animation. Same settings/files reuse the decoded pixels.
    pub(super) fn load(previous: Option<&Arc<Self>>) -> Result<Arc<Self>, AppError> {
        let _apartment = Apartment::enter()?;
        let settings = Settings::read()?;
        if let Some(previous) = previous.filter(|p| p.settings == settings) {
            return Ok(Arc::clone(previous));
        }
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .map_err(wallpaper_error)?;
        let mut images = HashMap::new();
        let mut budget = MAX_BYTES;
        for monitor in &settings.monitors {
            if !monitor.path.is_empty() && !images.contains_key(&monitor.path) {
                images.insert(
                    monitor.path.clone(),
                    Image::decode(
                        &factory,
                        &monitor.path,
                        &mut budget,
                        settings.position == DWPOS_TILE,
                    )?,
                );
            }
        }
        let brush = unsafe { CreateSolidBrush(settings.color) };
        if brush.is_null() {
            return Err(wallpaper_error("CreateSolidBrush failed"));
        }
        Ok(Arc::new(Self {
            settings,
            images,
            background_brush: brush as usize,
        }))
    }

    pub(super) fn covers(&self, viewport: Rect) -> bool {
        self.settings
            .monitors
            .iter()
            .any(|m| covers_bounds(m.bounds, viewport))
    }

    pub(super) fn validate(&self, viewport: Rect) -> Result<(), AppError> {
        let monitor = self
            .settings
            .monitors
            .iter()
            .find(|m| covers_bounds(m.bounds, viewport))
            .ok_or_else(|| wallpaper_error("animated viewport has no matching physical monitor"))?;
        let (screen, path) = self.screen_and_path(monitor);
        native(screen).map_err(wallpaper_error)?;
        if let Some(image) = path.and_then(|p| self.images.get(p)) {
            destination(screen, image.width, image.height, self.settings.position)?;
        }
        Ok(())
    }

    fn screen_and_path<'a>(&'a self, monitor: &'a MonitorWallpaper) -> (Rect, Option<&'a String>) {
        if self.settings.position == DWPOS_SPAN || self.settings.position == DWPOS_TILE {
            (
                union(self.settings.monitors.iter().map(|m| m.bounds)),
                self.settings
                    .monitors
                    .iter()
                    .find(|m| !m.path.is_empty())
                    .map(|m| &m.path),
            )
        } else {
            (monitor.bounds, Some(&monitor.path))
        }
    }

    pub(super) fn paint(&self, hdc: HDC, viewport: Rect) -> bool {
        let client = RECT {
            left: 0,
            top: 0,
            right: viewport.width as i32,
            bottom: viewport.height as i32,
        };
        if unsafe { FillRect(hdc, &client, self.background_brush as HBRUSH) } == 0 {
            return false;
        }
        // Work areas may be inset/negative; anchor wallpaper to the complete physical monitor.
        let Some(monitor) = self
            .settings
            .monitors
            .iter()
            .find(|m| covers_bounds(m.bounds, viewport))
        else {
            return false;
        };
        let (screen, path) = self.screen_and_path(monitor);
        let Some(image) = path.and_then(|p| self.images.get(p)) else {
            return true; // Solid-color desktops have no wallpaper path.
        };
        if self.settings.position == DWPOS_TILE {
            return unsafe {
                SetBrushOrgEx(
                    hdc,
                    screen.x - viewport.x,
                    screen.y - viewport.y,
                    null_mut(),
                ) != 0
                    && FillRect(hdc, &client, image.tile_brush as HBRUSH) != 0
            };
        }
        let Ok(dest) = destination(screen, image.width, image.height, self.settings.position)
        else {
            return false;
        };
        unsafe {
            SetStretchBltMode(hdc, HALFTONE);
            SetBrushOrgEx(hdc, 0, 0, null_mut());
            StretchDIBits(
                hdc,
                dest.x - viewport.x,
                dest.y - viewport.y,
                dest.width as i32,
                dest.height as i32,
                0,
                0,
                image.width,
                image.height,
                image
                    .dib
                    .as_ptr()
                    .add(size_of::<BITMAPINFOHEADER>() / 4)
                    .cast(),
                image.dib.as_ptr().cast(),
                DIB_RGB_COLORS,
                SRCCOPY,
            ) != 0
        }
    }
}

fn union(areas: impl Iterator<Item = Rect>) -> Rect {
    let mut left = i32::MAX;
    let mut top = i32::MAX;
    let mut right = i64::from(i32::MIN);
    let mut bottom = right;
    for r in areas {
        left = left.min(r.x);
        top = top.min(r.y);
        right = right.max(i64::from(r.x) + i64::from(r.width));
        bottom = bottom.max(i64::from(r.y) + i64::from(r.height));
    }
    Rect {
        x: left,
        y: top,
        width: u32::try_from(right - i64::from(left)).unwrap_or(u32::MAX),
        height: u32::try_from(bottom - i64::from(top)).unwrap_or(u32::MAX),
    }
}

/// Full wallpaper destination, clipped by the paint DC, not resized to the work area.
fn destination(
    screen: Rect,
    width: i32,
    height: i32,
    position: DESKTOP_WALLPAPER_POSITION,
) -> Result<Rect, AppError> {
    if position == DWPOS_STRETCH {
        return Ok(screen);
    }
    let sx = f64::from(screen.width) / f64::from(width);
    let sy = f64::from(screen.height) / f64::from(height);
    let scale = if position == DWPOS_CENTER {
        1.0
    } else if position == DWPOS_FIT {
        sx.min(sy)
    } else {
        sx.max(sy)
    }; // Fill and span.
    let w = (f64::from(width) * scale).round() as u32;
    let h = (f64::from(height) * scale).round() as u32;
    let r = Rect {
        x: i32::try_from(i64::from(screen.x) + (i64::from(screen.width) - i64::from(w)) / 2)
            .map_err(wallpaper_error)?,
        y: i32::try_from(i64::from(screen.y) + (i64::from(screen.height) - i64::from(h)) / 2)
            .map_err(wallpaper_error)?,
        width: w,
        height: h,
    };
    native(r).map_err(wallpaper_error)?;
    Ok(r)
}

pub(super) struct Backdrop {
    pub(super) wallpaper: Arc<Wallpaper>,
    pub(super) viewport: Rect,
    pub(super) paint_failed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_wallpaper_paints_color_and_image_without_a_desktop_capture() {
        // A memory DC only: no HWND, desktop DC, GUI launch or wallpaper setting changes.
        struct Canvas {
            dc: HDC,
            bitmap: HBITMAP,
            previous: HGDIOBJ,
        }
        impl Drop for Canvas {
            fn drop(&mut self) {
                unsafe {
                    SelectObject(self.dc, self.previous);
                    DeleteObject(self.bitmap);
                    DeleteDC(self.dc);
                }
            }
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: 4,
                biHeight: -4,
                biPlanes: 1,
                biBitCount: 32,
                ..unsafe { zeroed() }
            },
            ..unsafe { zeroed() }
        };
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        assert!(!dc.is_null());
        let mut bits = null_mut();
        let bitmap =
            unsafe { CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0) };
        assert!(!bitmap.is_null());
        let previous = unsafe { SelectObject(dc, bitmap) };
        let canvas = Canvas {
            dc,
            bitmap,
            previous,
        };
        let mut dib = vec![0u32; size_of::<BITMAPINFOHEADER>() / 4 + 4];
        let header = BITMAPINFOHEADER {
            biWidth: 2,
            biHeight: -2,
            ..info.bmiHeader
        };
        unsafe { std::ptr::write(dib.as_mut_ptr().cast::<BITMAPINFOHEADER>(), header) };
        // BGRX words: red, blue / yellow, black.
        dib[size_of::<BITMAPINFOHEADER>() / 4..].copy_from_slice(&[0x00ff0000, 0xff, 0xffff00, 0]);
        let tile = unsafe { CreateDIBPatternBrushPt(dib.as_ptr().cast(), DIB_RGB_COLORS) };
        assert!(!tile.is_null());
        let mut wallpaper = Wallpaper {
            settings: Settings {
                monitors: vec![MonitorWallpaper {
                    bounds: Rect {
                        x: -6,
                        y: -3,
                        width: 6,
                        height: 6,
                    },
                    path: "memory".into(),
                    modified: None,
                    length: 0,
                }],
                color: 0x0000ff00,
                position: DWPOS_CENTER,
            },
            images: HashMap::from([(
                "memory".into(),
                Image {
                    dib,
                    width: 2,
                    height: 2,
                    tile_brush: tile as usize,
                },
            )]),
            background_brush: unsafe { CreateSolidBrush(0x0000ff00) } as usize,
        };
        let viewport = Rect {
            x: -5,
            y: -2,
            width: 4,
            height: 4,
        };
        wallpaper.validate(viewport).unwrap();
        assert!(wallpaper.paint(canvas.dc, viewport));
        unsafe { GdiFlush() };
        let pixels = || unsafe { std::slice::from_raw_parts(bits.cast::<u32>(), 16).to_vec() };
        // Wallpaper centered on the full monitor, not centered again on its inset work area.
        assert_eq!(pixels()[0] & 0xffffff, 0x00ff00);
        assert_eq!(pixels()[5] & 0xffffff, 0xff0000);
        assert_eq!(pixels()[6] & 0xffffff, 0x0000ff);
        wallpaper.settings.position = DWPOS_TILE;
        assert!(wallpaper.paint(canvas.dc, viewport));
        unsafe { GdiFlush() };
        assert_eq!(pixels()[0] & 0xffffff, 0); // Full monitor offset (1,1): bottom-right tile.
        assert_eq!(pixels()[5] & 0xffffff, 0xff0000);
        wallpaper.settings.monitors[0].path.clear();
        assert!(wallpaper.paint(canvas.dc, viewport));
        unsafe { GdiFlush() };
        assert!(pixels().iter().all(|p| p & 0xffffff == 0x00ff00));
    }
    #[test]
    fn wallpaper_uses_full_negative_monitor_not_inset_work_area() {
        let screen = Rect {
            x: -1920,
            y: -100,
            width: 1920,
            height: 1080,
        };
        let viewport = Rect {
            x: -1900,
            y: -64,
            width: 1880,
            height: 1024,
        };
        let fill = destination(screen, 1000, 1000, DWPOS_FILL).unwrap();
        assert_eq!(
            fill,
            Rect {
                x: -1920,
                y: -520,
                width: 1920,
                height: 1920
            }
        );
        assert_eq!((fill.x - viewport.x, fill.y - viewport.y), (-20, -456));
        assert_eq!(
            destination(screen, 1000, 1000, DWPOS_FIT).unwrap(),
            Rect {
                x: -1500,
                y: -100,
                width: 1080,
                height: 1080
            }
        );
        assert_eq!(
            destination(screen, 1000, 1000, DWPOS_STRETCH).unwrap(),
            screen
        );
        assert_eq!(
            destination(screen, 1000, 1000, DWPOS_CENTER).unwrap(),
            Rect {
                x: -1460,
                y: -60,
                width: 1000,
                height: 1000
            }
        );
        let span = union(
            [
                screen,
                Rect {
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                },
            ]
            .into_iter(),
        );
        assert_eq!(
            span,
            Rect {
                x: -1920,
                y: -100,
                width: 3840,
                height: 1180
            }
        );
        assert_eq!(destination(span, 3840, 1180, DWPOS_SPAN).unwrap(), span);
        assert!(destination(screen, 1, 16_000_000, DWPOS_FILL).is_err());
    }
    #[test]
    fn wallpaper_pixels_are_bounded_before_decode_allocation() {
        assert_eq!(image_bytes(3840, 2160, MAX_BYTES).unwrap(), 33_177_600);
        assert!(image_bytes(0, 1, MAX_BYTES).is_err());
        assert!(image_bytes(u32::MAX, u32::MAX, MAX_BYTES).is_err());
        assert!(image_bytes(3840, 2160, 1).is_err());
    }
}
