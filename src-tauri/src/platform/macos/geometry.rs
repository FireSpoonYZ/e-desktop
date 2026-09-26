use crate::model::Rect;

// CGFloat is f64 on both supported 64-bit macOS architectures.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub origin: Point,
    pub size: Size,
}

pub fn physical(frame: Frame, scale: f64) -> Option<Rect> {
    let x = (frame.origin.x * scale).round();
    let y = (frame.origin.y * scale).round();
    let width = (frame.size.width * scale).round();
    let height = (frame.size.height * scale).round();
    if ![x, y, width, height, scale].iter().all(|n| n.is_finite())
        || scale <= 0.0
        || x < i32::MIN as f64
        || x > i32::MAX as f64
        || y < i32::MIN as f64
        || y > i32::MAX as f64
        || width <= 0.0
        || width > u32::MAX as f64
        || height <= 0.0
        || height > u32::MAX as f64
    {
        return None;
    }
    Some(Rect {
        x: x as i32,
        y: y as i32,
        width: width as u32,
        height: height as u32,
    })
}
pub fn logical(rect: Rect, scale: f64) -> Frame {
    Frame {
        origin: Point {
            x: rect.x as f64 / scale,
            y: rect.y as f64 / scale,
        },
        size: Size {
            width: rect.width as f64 / scale,
            height: rect.height as f64 / scale,
        },
    }
}
pub fn cocoa_to_cg(frame: Frame, main_height: f64) -> Frame {
    Frame {
        origin: Point {
            x: frame.origin.x,
            y: main_height - frame.origin.y - frame.size.height,
        },
        ..frame
    }
}
pub fn overlap(a: Rect, b: Rect) -> i64 {
    let width = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width))
        - i64::from(a.x.max(b.x));
    let height = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height))
        - i64::from(a.y.max(b.y));
    width.max(0).saturating_mul(height.max(0))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retina_negative_origin_round_trip_and_cocoa_axis() {
        let frame = Frame {
            origin: Point {
                x: -1200.0,
                y: 24.0,
            },
            size: Size {
                width: 800.0,
                height: 600.0,
            },
        };
        let rect = physical(frame, 2.0).unwrap();
        assert_eq!(
            rect,
            Rect {
                x: -2400,
                y: 48,
                width: 1600,
                height: 1200
            }
        );
        assert_eq!(logical(rect, 2.0), frame);
        assert_eq!(
            cocoa_to_cg(
                Frame {
                    origin: Point {
                        x: -1200.0,
                        y: 276.0
                    },
                    ..frame
                },
                900.0
            ),
            frame
        );
        assert!(physical(frame, f64::NAN).is_none());
        assert!(
            physical(
                Frame {
                    size: Size {
                        width: 0.0,
                        height: 3.0
                    },
                    ..frame
                },
                1.0
            )
            .is_none()
        );
        assert_eq!(overlap(rect, rect), 1_920_000);
        assert_eq!(overlap(rect, Rect { x: 0, ..rect }), 0);
    }
}
