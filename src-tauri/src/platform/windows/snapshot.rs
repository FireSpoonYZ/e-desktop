//! Best-effort static pictures, captured off the controller thread before minimization.
//! PrintWindow is not cancellable: one stuck call occupies the sole worker until it returns.
use super::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::Storage::Xps::PrintWindow;

const CACHE_BYTES: usize = 32 * 1024 * 1024;
const CAPTURE_BYTES: usize = 32 * 1024 * 1024;
const FRAME_BYTES: usize = 4 * 1024 * 1024;
const CAPTURE_INTERVAL: Duration = Duration::from_millis(500);
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Identity {
    hwnd: usize,
    pid: u32,
    cookie: usize,
}
impl Identity {
    pub(super) fn of(entry: &Entry) -> Self {
        Self {
            hwnd: entry.hwnd,
            pid: entry.pid,
            cookie: entry.cookie,
        }
    }
    fn alive(self, property: &[u16]) -> bool {
        let mut pid = 0;
        let h = self.hwnd as HWND;
        unsafe {
            IsWindow(h) != 0
                && GetWindowThreadProcessId(h, &mut pid) != 0
                && pid == self.pid
                && GetPropW(h, property.as_ptr()) as usize == self.cookie
        }
    }
}

pub(super) struct Frame {
    pub(super) size: SIZE,
    pub(super) pixels: Vec<u8>,
}
struct Cached {
    frame: Frame,
    captured: Instant,
}
struct Request {
    key: Identity,
    property: Vec<u16>,
    cancelled: Arc<AtomicBool>,
}
struct Pending {
    key: Identity,
    cancelled: Arc<AtomicBool>,
}
struct Worker {
    requests: SyncSender<Request>,
    results: Receiver<(Identity, Option<Frame>)>,
}
#[derive(Default)]
pub(super) struct Snapshots {
    frames: HashMap<Identity, Cached>,
    attempts: HashMap<Identity, Instant>,
    worker: Option<Worker>,
    pending: Option<Pending>,
    last_request: Option<Instant>,
}

fn pixel_bytes(size: SIZE, limit: usize) -> Option<usize> {
    if size.cx <= 0 || size.cy <= 0 {
        return None;
    }
    let bytes = (size.cx as usize)
        .checked_mul(size.cy as usize)?
        .checked_mul(4)?;
    (bytes <= limit).then_some(bytes)
}

/// The longest dimension is capped at 1024; no large full-resolution pixel copy.
fn reduced_size(size: SIZE) -> SIZE {
    let longest = size.cx.max(size.cy);
    if longest <= 1024 {
        return size;
    }
    SIZE {
        cx: (i64::from(size.cx) * 1024 / i64::from(longest)).max(1) as i32,
        cy: (i64::from(size.cy) * 1024 / i64::from(longest)).max(1) as i32,
    }
}
pub(super) fn bitmap_info(size: SIZE) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size.cx,
            biHeight: -size.cy,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { zeroed() }
        },
        ..unsafe { zeroed() }
    }
}

/// Only used on the creating thread; no HDC/HBITMAP ever crosses the worker channel.
pub(super) struct Dib {
    pub(super) dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    bytes: usize,
}
impl Dib {
    pub(super) fn new(size: SIZE, limit: usize) -> Option<Self> {
        let bytes = pixel_bytes(size, limit)?;
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        if dc.is_null() {
            return None;
        }
        let mut bits = null_mut();
        let bitmap = unsafe {
            CreateDIBSection(
                dc,
                &bitmap_info(size),
                DIB_RGB_COLORS,
                &mut bits,
                null_mut(),
                0,
            )
        };
        if bitmap.is_null() || bits.is_null() {
            unsafe {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                DeleteDC(dc);
            }
            return None;
        }
        let previous = unsafe { SelectObject(dc, bitmap) };
        if previous.is_null() || previous as isize == -1 {
            unsafe {
                DeleteObject(bitmap);
                DeleteDC(dc);
            }
            return None;
        }
        unsafe {
            std::ptr::write_bytes(bits.cast::<u8>(), 0, bytes);
        }
        Some(Self {
            dc,
            bitmap,
            previous,
            bits: bits.cast(),
            bytes,
        })
    }
    pub(super) fn pixels(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.bytes) }
    }
}
impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

fn drawable(request: &Request) -> bool {
    let h = request.key.hwnd as HWND;
    let mut cloaked = 0u32;
    !request.cancelled.load(Ordering::Acquire)
        && request.key.alive(&request.property)
        && unsafe { IsIconic(h) == 0 && IsWindowVisible(h) != 0 }
        && unsafe {
            DwmGetWindowAttribute(
                h,
                DWMWA_CLOAKED as u32,
                &mut cloaked as *mut _ as _,
                size_of::<u32>() as u32,
            ) >= 0
        }
        && cloaked == 0
}

fn capture(request: &Request) -> Option<Frame> {
    let _dpi = DpiScope::enter().ok()?;
    if !drawable(request) {
        return None;
    }
    let h = request.key.hwnd as HWND;
    let outer = outer_frame(h)?;
    let size = SIZE {
        cx: i32::try_from(i64::from(outer.right) - i64::from(outer.left)).ok()?,
        cy: i32::try_from(i64::from(outer.bottom) - i64::from(outer.top)).ok()?,
    };
    let full = Dib::new(size, CAPTURE_BYTES)?;
    // SDK PW_RENDERFULLCONTENT (WinUser.h). Applications may still refuse/return black.
    if unsafe { PrintWindow(h, full.dc, 2) } == 0 || !drawable(request) {
        return None;
    }
    if !same_rect(outer, outer_frame(h)?) {
        return None;
    }
    let small_size = reduced_size(size);
    let mut small = Dib::new(small_size, FRAME_BYTES)?;
    if unsafe {
        SetStretchBltMode(small.dc, HALFTONE);
        SetBrushOrgEx(small.dc, 0, 0, null_mut());
        StretchBlt(
            small.dc,
            0,
            0,
            small_size.cx,
            small_size.cy,
            full.dc,
            0,
            0,
            size.cx,
            size.cy,
            SRCCOPY,
        )
    } == 0
    {
        return None;
    }
    unsafe {
        GdiFlush();
    }
    let pixels = small.pixels();
    if !prepare_pixels(pixels) {
        return None;
    }
    Some(Frame {
        size: small_size,
        pixels: pixels.to_vec(),
    })
}

// ponytail: reject all-black pictures, including genuinely black windows; revisit only
// with application-specific evidence rather than replacing a useful old snapshot.
fn prepare_pixels(pixels: &mut [u8]) -> bool {
    if !pixels
        .chunks_exact(4)
        .any(|p| p[..3].iter().any(|c| *c != 0))
    {
        return false;
    }
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    true
}

impl Snapshots {
    fn start_worker(&mut self) -> bool {
        if self.worker.is_some() {
            return true;
        }
        let (requests, receive) = mpsc::sync_channel::<Request>(1);
        let (send, results) = mpsc::sync_channel(1);
        // Intentionally detached: Drop closes channels, but must never join PrintWindow.
        match std::thread::Builder::new()
            .name("window-snapshot".into())
            .spawn(move || {
                while let Ok(request) = receive.recv() {
                    let frame = capture(&request);
                    if send.send((request.key, frame)).is_err() {
                        break;
                    }
                }
            }) {
            Ok(_) => {
                self.worker = Some(Worker { requests, results });
                true
            }
            Err(e) => {
                eprintln!("Snapshot worker unavailable: {e}");
                false
            }
        }
    }

    fn due(&self, key: Identity, now: Instant) -> bool {
        self.pending.is_none()
            && self
                .last_request
                .is_none_or(|t| now.duration_since(t) >= CAPTURE_INTERVAL)
            && self
                .attempts
                .get(&key)
                .is_none_or(|t| now.duration_since(*t) >= REFRESH_INTERVAL)
    }

    pub(super) fn request(&mut self, key: Identity, property: &[u16], now: Instant) {
        if !self.due(key, now) || !self.start_worker() {
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let request = Request {
            key,
            property: property.to_vec(),
            cancelled: cancelled.clone(),
        };
        if self
            .worker
            .as_ref()
            .unwrap()
            .requests
            .try_send(request)
            .is_ok()
        {
            self.pending = Some(Pending { key, cancelled });
            self.attempts.insert(key, now);
            self.last_request = Some(now);
        }
    }

    pub(super) fn cancel(&mut self, key: Identity) {
        if let Some(pending) = self.pending.as_ref().filter(|p| p.key == key) {
            pending.cancelled.store(true, Ordering::Release);
        }
    }

    pub(super) fn poll(&mut self, live: &HashSet<Identity>) {
        self.retain(live);
        let result = self.worker.as_ref().and_then(|w| w.results.try_recv().ok());
        if let Some((key, frame)) = result {
            let valid = self
                .pending
                .take()
                .is_some_and(|p| p.key == key && !p.cancelled.load(Ordering::Acquire));
            if valid && live.contains(&key) {
                if let Some(frame) = frame {
                    self.insert(key, frame, Instant::now());
                }
            }
        }
    }

    fn retain(&mut self, live: &HashSet<Identity>) {
        self.frames.retain(|key, _| live.contains(key));
        self.attempts.retain(|key, _| live.contains(key));
        if let Some(p) = &self.pending {
            if !live.contains(&p.key) {
                p.cancelled.store(true, Ordering::Release);
            }
        }
    }

    fn insert(&mut self, key: Identity, frame: Frame, now: Instant) {
        if pixel_bytes(frame.size, FRAME_BYTES) != Some(frame.pixels.len()) {
            return;
        }
        self.frames.remove(&key);
        while self
            .frames
            .values()
            .map(|c| c.frame.pixels.len())
            .sum::<usize>()
            + frame.pixels.len()
            > CACHE_BYTES
        {
            let oldest = *self
                .frames
                .iter()
                .min_by_key(|(_, c)| c.captured)
                .unwrap()
                .0;
            self.frames.remove(&oldest);
        }
        self.frames.insert(
            key,
            Cached {
                frame,
                captured: now,
            },
        );
    }

    pub(super) fn get(&self, key: Identity) -> Option<&Frame> {
        self.frames.get(&key).map(|c| &c.frame)
    }

    /// Never-attempted sources first, then oldest attempt; failures cannot starve peers.
    pub(super) fn next(
        &self,
        keys: impl Iterator<Item = Identity>,
        now: Instant,
    ) -> Option<Identity> {
        keys.filter(|key| self.due(*key, now)).min_by_key(|key| {
            (
                self.attempts.get(key).copied(),
                self.frames.contains_key(key),
            )
        })
    }
}
impl Drop for Snapshots {
    fn drop(&mut self) {
        if let Some(p) = &self.pending {
            p.cancelled.store(true, Ordering::Release);
        }
        // GDI objects of an in-flight call remain owned by its worker until it returns.
    }
}

/// saved.clipped means a manager region is installed, not that the picture is partial.
/// A full tile still has a region masking the invisible resize border.
fn capture_eligible(entry: &Entry) -> bool {
    if entry.saved.is_none() || entry.minimized {
        return false;
    }
    let Some(region) = entry.region_box else {
        return true;
    };
    let (Some(pad), Some(visible)) = (entry.placed_pad, entry.placed_visible) else {
        return false;
    };
    region.left <= pad[0]
        && region.top <= pad[1]
        && i64::from(region.right) >= i64::from(pad[0]) + i64::from(visible.width)
        && i64::from(region.bottom) >= i64::from(pad[1]) + i64::from(visible.height)
}

impl Backend {
    pub(super) fn poll_snapshots(&mut self) {
        let live = self
            .entries
            .values()
            .filter(|e| self.alive(e))
            .map(Identity::of)
            .collect();
        self.snapshots.poll(&live);
    }

    pub(super) fn warm_snapshots(&mut self) {
        self.poll_snapshots();
        let now = Instant::now();
        let key = self.snapshots.next(
            self.entries
                .values()
                .filter(|e| {
                    self.alive(e)
                        && capture_eligible(e)
                        && unsafe {
                            IsIconic(e.hwnd as HWND) == 0 && IsWindowVisible(e.hwnd as HWND) != 0
                        }
                })
                .map(Identity::of),
            now,
        );
        if let Some(key) = key {
            self.snapshots.request(key, &self.property, now);
        }
    }

    pub(super) fn snapshot_before_hide(&mut self, id: &str) {
        self.poll_snapshots();
        let key = Identity::of(&self.entries[id]);
        // Never accept a completion after our hide/clip; keep the last finished picture.
        self.snapshots.cancel(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(cookie: usize) -> Identity {
        Identity {
            hwnd: 1,
            pid: 2,
            cookie,
        }
    }
    fn frame(bytes: usize) -> Frame {
        Frame {
            size: SIZE {
                cx: 1024,
                cy: (bytes / 4096) as i32,
            },
            pixels: vec![1; bytes],
        }
    }

    #[test]
    fn capture_eligibility_tracks_full_partial_and_restored_regions() {
        let mut entry = Entry {
            hwnd: 0,
            pid: 0,
            cookie: 1,
            saved: None,
            minimized: false,
            decor: None,
            pads: vec![],
            placed_pad: Some([7, 0, 7, 7]),
            region_box: None,
            at_bottom: false,
            min_width: None,
            reported_min: None,
            placed_visible: Some(Rect {
                x: 100,
                y: 100,
                width: 400,
                height: 300,
            }),
        };
        assert!(
            !capture_eligible(&entry),
            "metadata-only window is not owned"
        );
        entry.saved = Some(Saved {
            placement: unsafe { zeroed() },
            region: None,
            clipped: true,
        });
        entry.region_box = Some(RECT {
            left: 7,
            top: 0,
            right: 407,
            bottom: 300,
        });
        assert!(
            capture_eligible(&entry),
            "full tile with resize border masked"
        );
        entry.region_box.as_mut().unwrap().right = 207;
        assert!(!capture_eligible(&entry), "partial tile");
        entry.region_box = None;
        entry.saved.as_mut().unwrap().clipped = false;
        assert!(capture_eligible(&entry), "original region restored");
        entry.minimized = true;
        assert!(!capture_eligible(&entry));
    }

    #[test]
    fn sizes_are_checked_and_4k_is_bounded_without_a_full_pixel_copy() {
        assert_eq!(
            pixel_bytes(SIZE { cx: 3840, cy: 2160 }, CAPTURE_BYTES),
            Some(33_177_600)
        );
        assert!(pixel_bytes(SIZE { cx: 7680, cy: 4320 }, CAPTURE_BYTES).is_none());
        assert!(
            pixel_bytes(
                SIZE {
                    cx: i32::MAX,
                    cy: i32::MAX
                },
                CAPTURE_BYTES
            )
            .is_none()
        );
        assert!(pixel_bytes(SIZE { cx: 0, cy: -1 }, FRAME_BYTES).is_none());
        let size = reduced_size(SIZE { cx: 3840, cy: 2160 });
        assert_eq!((size.cx, size.cy), (1024, 576));
        assert!(pixel_bytes(size, FRAME_BYTES).is_some());
        let size = reduced_size(SIZE { cx: 1, cy: 100_000 });
        assert_eq!((size.cx, size.cy), (1, 1024));
    }

    #[test]
    fn black_pictures_are_rejected_and_valid_pixels_are_opaque() {
        assert!(!prepare_pixels(&mut [0, 0, 0, 255]));
        assert!(!prepare_pixels(&mut []));
        let mut pixels = [1, 2, 3, 0, 0, 0, 0, 128];
        assert!(prepare_pixels(&mut pixels));
        assert_eq!(pixels, [1, 2, 3, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn cache_evicts_oldest_and_rejects_invalid_frames() {
        let now = Instant::now();
        let mut cache = Snapshots::default();
        for cookie in 1..=9 {
            cache.insert(
                key(cookie),
                frame(FRAME_BYTES),
                now + Duration::from_millis(cookie as u64),
            );
        }
        assert!(cache.get(key(1)).is_none());
        assert!(cache.get(key(9)).is_some());
        assert_eq!(
            cache
                .frames
                .values()
                .map(|c| c.frame.pixels.len())
                .sum::<usize>(),
            CACHE_BYTES
        );
        cache.insert(
            key(9),
            Frame {
                size: SIZE { cx: 1, cy: 1 },
                pixels: vec![],
            },
            now,
        );
        assert_eq!(cache.get(key(9)).unwrap().pixels.len(), FRAME_BYTES);
    }

    #[test]
    fn lifetime_reuse_and_cancelled_completion_do_not_replace_good_frames() {
        let now = Instant::now();
        let mut cache = Snapshots::default();
        cache.insert(key(1), frame(4096), now);
        cache.attempts.insert(key(1), now);
        let cancelled = Arc::new(AtomicBool::new(false));
        cache.pending = Some(Pending {
            key: key(1),
            cancelled: cancelled.clone(),
        });
        let (requests, _) = mpsc::sync_channel(1);
        let (send, results) = mpsc::sync_channel(1);
        cache.worker = Some(Worker { requests, results });
        cache.cancel(key(1));
        send.send((key(1), Some(frame(8192)))).unwrap();
        cache.poll(&HashSet::from([key(1)]));
        assert_eq!(cache.get(key(1)).unwrap().pixels.len(), 4096);
        assert!(cache.pending.is_none());
        // Failure (including capture size/black rejection) also retains the good old picture.
        cache.pending = Some(Pending {
            key: key(1),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        send.send((key(1), None)).unwrap();
        cache.poll(&HashSet::from([key(1)]));
        assert_eq!(cache.get(key(1)).unwrap().pixels.len(), 4096);
        // A reused HWND has a new cookie; its predecessor's late reply is discarded.
        cache.pending = Some(Pending {
            key: key(1),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        send.send((key(1), Some(frame(8192)))).unwrap();
        cache.poll(&HashSet::from([key(2)]));
        assert!(cache.pending.is_none());
        assert!(cache.get(key(2)).is_none());
        assert!(cache.get(key(1)).is_none());
        assert!(!cache.attempts.contains_key(&key(1)));
    }

    #[test]
    fn requests_coalesce_while_busy_and_retry_fairly_after_completion() {
        let now = Instant::now();
        let mut cache = Snapshots::default();
        cache.pending = Some(Pending {
            key: key(1),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        cache.request(key(2), &[], now); // Does not spawn a worker or queue another request.
        assert!(cache.worker.is_none());
        assert!(cache.next([key(1), key(2)].into_iter(), now).is_none());
        cache.pending = None;
        cache.last_request = Some(now);
        assert!(!cache.due(key(2), now + Duration::from_millis(499)));
        cache.attempts.insert(key(1), now);
        let later = now + REFRESH_INTERVAL;
        assert_eq!(
            cache.next([key(1), key(2)].into_iter(), later),
            Some(key(2))
        );
        cache.insert(key(2), frame(4096), now);
        cache.attempts.insert(key(2), now);
        assert_eq!(
            cache.next([key(1), key(2)].into_iter(), later),
            Some(key(1))
        );
        // Many missing/failing pictures must not outrank an older successful attempt.
        cache.insert(key(1), frame(4096), now);
        cache.frames.remove(&key(2));
        cache
            .attempts
            .insert(key(2), now + Duration::from_millis(1));
        assert_eq!(
            cache.next([key(1), key(2)].into_iter(), later + REFRESH_INTERVAL),
            Some(key(1))
        );
    }
}
