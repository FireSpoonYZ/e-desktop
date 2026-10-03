//! lane: rules-spawn-screenshot. Platform-neutral parts of the screenshot commands: the
//! save-path template, cropping and the one-at-a-time guard. Capture lives in the backend.
use std::sync::atomic::{AtomicBool, Ordering};

use crate::model::{AppError, Command, ErrorCode};

pub const DEFAULT_PATH: &str =
    r"%USERPROFILE%\Pictures\Screenshots\Screenshot from %Y-%m-%d %H-%M-%S.png";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Region,
    Screen,
    Window,
}

impl Kind {
    pub fn of(command: &Command) -> Option<Self> {
        match command {
            Command::Screenshot => Some(Self::Region),
            Command::ScreenshotScreen => Some(Self::Screen),
            Command::ScreenshotWindow => Some(Self::Window),
            _ => None,
        }
    }
}

pub fn unsupported() -> AppError {
    AppError {
        code: ErrorCode::NotImplemented,
        message: "截图目前只支持 Windows。".into(),
        window_id: None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

/// `%NAME%` environment variables first (unknown names stay literal), then
/// `%Y %m %d %H %M %S` and `%%`. Other `%` sequences are kept as written.
pub fn expand_path(
    template: &str,
    time: LocalTime,
    env: impl Fn(&str) -> Option<String>,
) -> String {
    let mut expanded = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('%') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after
            .find('%')
            .and_then(|end| Some((end, env(&after[..end]).filter(|_| end > 0)?)))
        {
            Some((end, value)) => {
                expanded.push_str(&value);
                rest = &after[end + 1..];
            }
            None => {
                expanded.push('%');
                rest = after;
            }
        }
    }
    expanded.push_str(rest);
    let mut path = String::new();
    let mut chars = expanded.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            path.push(c);
            continue;
        }
        let next = chars.clone().next();
        let value = match next {
            Some('Y') => format!("{:04}", time.year),
            Some('m') => format!("{:02}", time.month),
            Some('d') => format!("{:02}", time.day),
            Some('H') => format!("{:02}", time.hour),
            Some('M') => format!("{:02}", time.minute),
            Some('S') => format!("{:02}", time.second),
            Some('%') => "%".into(),
            _ => {
                path.push('%');
                continue;
            }
        };
        chars.next();
        path.push_str(&value);
    }
    path
}

/// Top-down BGRA pixels, alpha forced opaque.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Image {
    /// The part of `self` inside `(x, y, width, height)`; None when they do not overlap.
    pub fn crop(&self, x: i64, y: i64, width: u32, height: u32) -> Option<Self> {
        let left = x.max(0);
        let top = y.max(0);
        let right = (x + i64::from(width)).min(i64::from(self.width));
        let bottom = (y + i64::from(height)).min(i64::from(self.height));
        if right <= left || bottom <= top {
            return None;
        }
        let stride = self.width as usize * 4;
        let mut pixels = Vec::with_capacity(((right - left) * (bottom - top) * 4) as usize);
        for row in top..bottom {
            let start = row as usize * stride + left as usize * 4;
            pixels.extend_from_slice(&self.pixels[start..start + (right - left) as usize * 4]);
        }
        Some(Self {
            width: (right - left) as u32,
            height: (bottom - top) as u32,
            pixels,
        })
    }
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// A screenshot (including the region picker) is running; pointer gestures stay off.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// Held for the whole screenshot; only one runs at a time.
pub struct Busy(());

impl Busy {
    pub fn begin() -> Option<Self> {
        (!ACTIVE.swap(true, Ordering::AcqRel)).then_some(Self(()))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIME: LocalTime = LocalTime {
        year: 2026,
        month: 3,
        day: 7,
        hour: 9,
        minute: 5,
        second: 4,
    };

    fn env(name: &str) -> Option<String> {
        match name {
            "USERPROFILE" => Some(r"C:\Users\me".into()),
            "S" => Some("not a seconds field".into()),
            _ => None,
        }
    }

    #[test]
    fn default_path_expands_environment_then_time_fields() {
        assert_eq!(
            expand_path(DEFAULT_PATH, TIME, env),
            r"C:\Users\me\Pictures\Screenshots\Screenshot from 2026-03-07 09-05-04.png"
        );
        // Unknown variables and specifiers stay literal; %% is a percent sign.
        assert_eq!(expand_path("%NOPE%/%Y%%%q%", TIME, env), "%NOPE%/2026%%q%");
        assert_eq!(expand_path("100%", TIME, env), "100%");
        assert_eq!(expand_path("%%S", TIME, env), "%S");
        // A defined one-letter variable written as %S% is an environment variable.
        assert_eq!(expand_path("a%S%b", TIME, env), "anot a seconds fieldb");
    }

    #[test]
    fn crop_clips_to_the_image_and_keeps_row_order() {
        let image = Image {
            width: 3,
            height: 2,
            pixels: (0..24).collect(),
        };
        assert_eq!(
            image.crop(1, 0, 5, 1),
            Some(Image {
                width: 2,
                height: 1,
                pixels: (4..12).collect()
            })
        );
        assert_eq!(
            image.crop(-1, 1, 2, 9).unwrap().pixels,
            (12..16).collect::<Vec<u8>>()
        );
        assert_eq!(image.crop(3, 0, 1, 1), None);
        assert_eq!(image.crop(0, 0, 3, 2), Some(image.clone()));
    }

    #[test]
    fn screenshot_commands_map_to_kinds_and_only_one_runs_at_a_time() {
        assert_eq!(Kind::of(&Command::Screenshot), Some(Kind::Region));
        assert_eq!(Kind::of(&Command::ScreenshotScreen), Some(Kind::Screen));
        assert_eq!(Kind::of(&Command::ScreenshotWindow), Some(Kind::Window));
        assert_eq!(Kind::of(&Command::Refresh), None);
        let busy = Busy::begin().unwrap();
        assert!(active() && Busy::begin().is_none());
        drop(busy);
        assert!(!active());
    }
}
