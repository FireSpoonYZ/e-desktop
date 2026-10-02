use serde::Serialize;
use std::{collections::BTreeMap, path::PathBuf};

pub const HELP: &str = r#"window-handoff-probe (stage 0; exit 0 is NOT visual acceptance)
  --help                         no native calls
  --self-test                    pure logic / memory DC only; no HWND
  --offline-proxy SPEC.json       one raw BGRA -> memory-DC proxy -> file; no HWND
  [--run] --pid PID --hwnd HEX --tag TOKEN --role A..E
    --mode baseline|prehide|staged --output NEW_DIRECTORY
    --target x,y,width,height    physical VISIBLE frame in screen pixels
    [--prehide-ms 100] [--staged-ms 150] [--animation-ms 160]
    [--handoff-ms 250]           absolute from target apply, includes animation
    [--minimized-hold-ms 200] [--observe-ms 300]
    [--cancel-file PATH]         create file to cancel; Ctrl+C also requests cleanup
    [--fixture-only-allow-unknown-affinity]  explicitly owned fixture exception; see README
No --run: parse only, with NO desktop side effects. One target, one trial, one capture.
Hold/observe times are experiment observation, NOT readiness conditions.
Parent MUST independently bound total process time; native APIs are not cancellable.
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Baseline,
    Prehide,
    Staged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
impl Rect {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Result<Self, String> {
        if width <= 0
            || height <= 0
            || width > 16384
            || height > 16384
            || x.checked_add(width).is_none()
            || y.checked_add(height).is_none()
        {
            return Err("invalid/overflowing physical rectangle".into());
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }
    pub fn right(self) -> i32 {
        self.x + self.width
    }
    pub fn bottom(self) -> i32 {
        self.y + self.height
    }
    pub fn contains(self, other: Self) -> bool {
        self.x <= other.x
            && self.y <= other.y
            && self.right() >= other.right()
            && self.bottom() >= other.bottom()
    }
    pub fn union(self, other: Self) -> Result<Self, String> {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let w = i64::from(self.right().max(other.right())) - i64::from(x);
        let h = i64::from(self.bottom().max(other.bottom())) - i64::from(y);
        Self::new(
            x,
            y,
            i32::try_from(w).map_err(|_| "union overflow")?,
            i32::try_from(h).map_err(|_| "union overflow")?,
        )
    }
    pub fn interpolate(self, target: Self, t: f64) -> Self {
        let lerp = |a: i32, b: i32| {
            (f64::from(a) + (f64::from(b) - f64::from(a)) * t.clamp(0.0, 1.0)).round() as i32
        };
        Self {
            x: lerp(self.x, target.x),
            y: lerp(self.y, target.y),
            width: lerp(self.width, target.width),
            height: lerp(self.height, target.height),
        }
    }
}

pub const RAW_BYTES: usize = 32 * 1024 * 1024;
pub const FRAME_BYTES: usize = 4 * 1024 * 1024;
pub const SCENE_BYTES: usize = 64 * 1024 * 1024;
pub fn pixel_bytes(w: i32, h: i32, limit: usize) -> Result<usize, String> {
    if w <= 0 || h <= 0 {
        return Err("nonpositive pixel size".into());
    }
    (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        .filter(|n| *n <= limit)
        .ok_or_else(|| "pixel budget exceeded".into())
}
pub fn reduced_size(w: i32, h: i32) -> (i32, i32) {
    let edge = w.max(h).max(1024);
    (
        (i64::from(w) * 1024 / i64::from(edge)).max(1) as i32,
        (i64::from(h) * 1024 / i64::from(edge)).max(1) as i32,
    )
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub pid: u32,
    #[serde(serialize_with = "serialize_hwnd")]
    pub hwnd: usize,
    pub tag: String,
    pub role: String,
    pub mode: Mode,
    pub output: PathBuf,
    pub target: Rect,
    pub prehide_ms: u64,
    pub staged_ms: u64,
    pub animation_ms: u64,
    pub handoff_ms: u64,
    pub minimized_hold_ms: u64,
    pub observe_ms: u64,
    pub cancel_file: Option<PathBuf>,
    pub fixture_only_allow_unknown_affinity: bool,
}
fn serialize_hwnd<S: serde::Serializer>(hwnd: &usize, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&format!("0x{hwnd:x}"))
}
pub fn workspace_rect(screen: Rect, work: Rect, monitor: Rect) -> Result<Rect, String> {
    let dx = i64::from(work.x) - i64::from(monitor.x);
    let dy = i64::from(work.y) - i64::from(monitor.y);
    Rect::new(
        i32::try_from(i64::from(screen.x) - dx).map_err(|_| "workspace x overflow")?,
        i32::try_from(i64::from(screen.y) - dy).map_err(|_| "workspace y overflow")?,
        screen.width,
        screen.height,
    )
}
pub fn identity_matches(
    expected_pid: u32,
    actual_pid: u32,
    expected_title: &[u16],
    actual_title: &[u16],
    expected_cookie: usize,
    actual_cookie: usize,
    process_alive: bool,
) -> bool {
    process_alive
        && expected_pid == actual_pid
        && expected_title == actual_title
        && expected_cookie != 0
        && expected_cookie == actual_cookie
}
pub enum Action {
    Help,
    SelfTest,
    OfflineProxy(PathBuf),
    Plan(Config),
    Run(Config),
}
pub fn parse(args: &[String]) -> Result<Action, String> {
    if args.is_empty() || args == ["--help"] {
        return Ok(Action::Help);
    }
    if args == ["--self-test"] {
        return Ok(Action::SelfTest);
    }
    if args.len() == 2 && args[0] == "--offline-proxy" && !args[1].is_empty() {
        return Ok(Action::OfflineProxy(args[1].clone().into()));
    }
    let mut opts = BTreeMap::new();
    let mut run = false;
    let mut fixture_exception = false;
    let mut i = 0;
    while i < args.len() {
        let key = &args[i];
        if key == "--run" {
            if run {
                return Err("duplicate --run".into());
            }
            run = true;
            i += 1;
            continue;
        }
        if key == "--fixture-only-allow-unknown-affinity" {
            if fixture_exception {
                return Err("duplicate fixture exception".into());
            }
            fixture_exception = true;
            i += 1;
            continue;
        }
        if ![
            "--pid",
            "--hwnd",
            "--tag",
            "--role",
            "--mode",
            "--output",
            "--target",
            "--prehide-ms",
            "--staged-ms",
            "--animation-ms",
            "--handoff-ms",
            "--minimized-hold-ms",
            "--observe-ms",
            "--cancel-file",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("unknown option {key}"));
        }
        let value = args
            .get(i + 1)
            .filter(|s| !s.starts_with("--"))
            .ok_or_else(|| format!("missing value for {key}"))?;
        if opts.insert(key.as_str(), value.as_str()).is_some() {
            return Err(format!("duplicate {key}"));
        }
        i += 2;
    }
    let required = |key| {
        opts.get(key)
            .copied()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("required {key}"))
    };
    let pid = required("--pid")?
        .parse::<u32>()
        .map_err(|_| "invalid PID")?;
    let hex = required("--hwnd")?;
    let hex = hex
        .strip_prefix("0x")
        .or_else(|| hex.strip_prefix("0X"))
        .unwrap_or(hex);
    let hwnd = usize::from_str_radix(hex, 16).map_err(|_| "HWND must be hexadecimal")?;
    if pid == 0 || pid == std::process::id() || hwnd == 0 || hwnd > isize::MAX as usize {
        return Err("invalid/self PID or HWND".into());
    }
    let tag = required("--tag")?.to_string();
    if tag.len() > 64 || !tag.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
        return Err("tag must be 1..64 ASCII letters/digits/hyphens".into());
    }
    let role = required("--role")?.to_string();
    if !["A", "B", "C", "D", "E"].contains(&role.as_str()) {
        return Err("role must be A..E".into());
    }
    let mode = match required("--mode")? {
        "baseline" => Mode::Baseline,
        "prehide" => Mode::Prehide,
        "staged" => Mode::Staged,
        _ => return Err("mode must be baseline|prehide|staged".into()),
    };
    let v = required("--target")?
        .split(',')
        .map(str::parse::<i32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "invalid target")?;
    if v.len() != 4 {
        return Err("target requires x,y,width,height".into());
    }
    let target = Rect::new(v[0], v[1], v[2], v[3])?;
    let ms = |key, default, min, max| -> Result<u64, String> {
        let n = opts
            .get(key)
            .map(|v| v.parse::<u64>())
            .transpose()
            .map_err(|_| format!("invalid milliseconds: {key}"))?
            .unwrap_or(default);
        if n < min || n > max {
            return Err(format!("{key} must be {min}..{max} ms"));
        }
        Ok(n)
    };
    let config = Config {
        pid,
        hwnd,
        tag,
        role,
        mode,
        target,
        output: required("--output")?.into(),
        prehide_ms: ms("--prehide-ms", 100, 1, 5000)?,
        staged_ms: ms("--staged-ms", 150, 1, 5000)?,
        animation_ms: ms("--animation-ms", 160, 0, 5000)?,
        handoff_ms: ms("--handoff-ms", 250, 1, 10000)?,
        minimized_hold_ms: ms("--minimized-hold-ms", 200, 0, 5000)?,
        observe_ms: ms("--observe-ms", 300, 0, 5000)?,
        cancel_file: opts.get("--cancel-file").map(PathBuf::from),
        fixture_only_allow_unknown_affinity: fixture_exception,
    };
    if config.handoff_ms < config.animation_ms {
        return Err("handoff budget must include animation".into());
    }
    Ok(if run {
        Action::Run(config)
    } else {
        Action::Plan(config)
    })
}

/// Only an explicitly trusted disposable fixture can admit unavailable metadata.
pub fn affinity_admission(
    value: Result<u32, u32>,
    layered: bool,
    allow: bool,
    fixture_image: bool,
) -> Result<&'static str, String> {
    match value {
        Ok(0) => Ok("public-api-wda-none"),
        Ok(v) => Err(format!("protected affinity {v:#x}")),
        Err(87) if !layered && allow && fixture_image => Ok("explicit-owned-fixture-exception"),
        Err(e) => Err(format!("unknown display affinity (Win32 {e}); refused")),
    }
}

/// Consumer protocol: one native call per trial; timeout/cancel never opens a second slot.
#[derive(Debug)]
pub struct CaptureGate {
    pub generation: u64,
    pub in_flight: bool,
    pub cancelled: bool,
}
impl CaptureGate {
    pub fn new() -> Self {
        Self {
            generation: 1,
            in_flight: false,
            cancelled: false,
        }
    }
    pub fn request(&mut self) -> Result<u64, String> {
        if self.in_flight || self.cancelled {
            return Err("capture unavailable".into());
        }
        self.in_flight = true;
        Ok(self.generation)
    }
    pub fn accepts(
        &self,
        generation: u64,
        before_deadline: bool,
        same_identity: bool,
        same_geometry: bool,
    ) -> bool {
        self.in_flight
            && !self.cancelled
            && generation == self.generation
            && before_deadline
            && same_identity
            && same_geometry
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
        self.generation += 1;
    }
}

pub fn self_check() -> Result<(), String> {
    let mut gate = CaptureGate::new();
    let gen_id = gate.request()?;
    if gate.request().is_ok()
        || !gate.accepts(gen_id, true, true, true)
        || gate.accepts(gen_id, false, true, true)
        || gate.accepts(gen_id, true, false, true)
        || gate.accepts(gen_id, true, true, false)
    {
        return Err("capture gate failure".into());
    }
    gate.cancel();
    if gate.accepts(gen_id, true, true, true) || gate.request().is_ok() {
        return Err("late cancellation failure".into());
    }
    pixel_bytes(1024, 1024, FRAME_BYTES)?;
    pixel_bytes(4096, 2048, RAW_BYTES)?;
    pixel_bytes(4096, 4096, SCENE_BYTES)?;
    if pixel_bytes(4096, 2049, RAW_BYTES).is_ok() || pixel_bytes(4097, 4096, SCENE_BYTES).is_ok() {
        return Err("raw/scene budget failure".into());
    }
    if pixel_bytes(1025, 1024, FRAME_BYTES).is_ok()
        || pixel_bytes(0, 1, RAW_BYTES).is_ok()
        || pixel_bytes(i32::MAX, i32::MAX, RAW_BYTES).is_ok()
    {
        return Err("budget failure".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args() -> Vec<String> {
        "--pid 42 --hwnd 0x123 --tag trial-1 --role C --mode staged --output out --target -800,20,700,490"
            .split_whitespace().map(String::from).collect()
    }
    #[test]
    fn parameters_and_no_run_are_strict() {
        assert!(matches!(parse(&args()).unwrap(), Action::Plan(_)));
        let mut a = args();
        a.push("--run".into());
        assert!(matches!(parse(&a).unwrap(), Action::Run(_)));
        for suffix in [
            "--run --run",
            "--pid 4",
            "--wat 1",
            "--handoff-ms 159",
            "--observe-ms 5001",
        ] {
            let mut a = args();
            a.extend(suffix.split_whitespace().map(String::from));
            assert!(parse(&a).is_err(), "{suffix}");
        }
        for (from, to) in [
            ("42", "0"),
            ("0x123", "0"),
            ("trial-1", "a/b"),
            ("C", "Z"),
            ("staged", "fixture"),
            ("-800,20,700,490", "2147483647,0,1,1"),
        ] {
            let a = args()
                .into_iter()
                .map(|v| if v == from { to.into() } else { v })
                .collect::<Vec<_>>();
            assert!(parse(&a).is_err(), "{from} -> {to}");
        }
    }
    #[test]
    fn identity_reuse_and_workspace_coordinates() {
        let title = [65u16, 0];
        assert!(identity_matches(42, 42, &title, &title, 7, 7, true));
        for (pid, cookie, alive) in [(43, 7, true), (42, 8, true), (42, 7, false), (42, 0, true)] {
            assert!(!identity_matches(42, pid, &title, &title, 7, cookie, alive));
        }
        assert!(!identity_matches(42, 42, &title, &[66, 0], 7, 7, true));
        let monitor = Rect::new(-1920, 0, 1920, 1080).unwrap();
        let work = Rect::new(-1880, 30, 1880, 1050).unwrap();
        let screen = Rect::new(-1500, 100, 700, 491).unwrap();
        assert_eq!(
            workspace_rect(screen, work, monitor).unwrap(),
            Rect::new(-1540, 70, 700, 491).unwrap()
        );
    }
    #[test]
    fn protection_admission_is_fail_closed_except_owned_fixture() {
        assert!(affinity_admission(Ok(0), false, false, false).is_ok());
        for v in [1, 17, 99] {
            assert!(affinity_admission(Ok(v), false, true, true).is_err());
        }
        assert!(affinity_admission(Err(87), false, true, true).is_ok());
        for (error, layered, allow, image) in [
            (87, false, false, true),
            (87, true, true, true),
            (87, false, true, false),
            (5, false, true, true),
            (6, false, true, true),
        ] {
            assert!(affinity_admission(Err(error), layered, allow, image).is_err());
        }
    }
    #[test]
    fn cancellation_deadline_identity_and_single_slot() {
        self_check().unwrap();
    }
    #[test]
    fn rectangle_cover_and_reduction() {
        let old = Rect::new(-900, 40, 700, 490).unwrap();
        let new = Rect::new(-1000, 10, 800, 800).unwrap();
        let cover = old.union(new).unwrap();
        assert!(cover.contains(old) && cover.contains(new));
        assert_eq!(old.interpolate(new, 1.0), new);
        assert_eq!(reduced_size(2048, 1024), (1024, 512));
        assert_eq!(reduced_size(701, 491), (701, 491));
        assert!(Rect::new(0, 0, 0, 5).is_err());
    }
}
