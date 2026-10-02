#[cfg(windows)]
mod capture;

const USAGE: &str =
    "Usage: wgc-probe <HWND decimal or 0xHEX> [duration-ms: 1..30000, default 3000]";

#[derive(Debug, PartialEq)]
struct Options {
    hwnd: usize,
    duration_ms: u64,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        if !(1..=2).contains(&args.len()) {
            return Err(USAGE.into());
        }
        let raw = &args[0];
        let hwnd = match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            Some(hex) => usize::from_str_radix(hex, 16),
            None => raw.parse(),
        }
        .map_err(|_| format!("Invalid HWND: {raw}"))?;
        if hwnd == 0 {
            return Err("HWND must be nonzero".into());
        }
        let duration_ms = match args.get(1) {
            Some(raw) => raw.parse().map_err(|_| "Invalid duration-ms")?,
            None => 3000,
        };
        if !(1..=30_000).contains(&duration_ms) {
            return Err("duration-ms must be in 1..30000".into());
        }
        Ok(Self { hwnd, duration_ms })
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() == 1 && matches!(args[0].as_str(), "-h" | "--help") {
        println!("{USAGE}");
        return std::process::ExitCode::SUCCESS;
    }
    let result = Options::parse(&args).and_then(run);
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("wgc-probe: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn run(options: Options) -> Result<(), String> {
    capture::run(options).map_err(|error| error.to_string())
}

#[cfg(not(windows))]
fn run(_: Options) -> Result<(), String> {
    Err("Windows only; requires Windows 10 1903 or newer".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn explicit_handle_and_bounded_duration() {
        assert_eq!(
            Options::parse(&args(&["0x1234"])).unwrap(),
            Options {
                hwnd: 0x1234,
                duration_ms: 3000
            }
        );
        assert_eq!(Options::parse(&args(&["4660", "1"])).unwrap().hwnd, 0x1234);
        assert_eq!(
            Options::parse(&args(&["0X1234", "30000"]))
                .unwrap()
                .duration_ms,
            30_000
        );
    }

    #[test]
    fn rejects_missing_invalid_and_overflowing_handles() {
        for values in [
            vec![],
            vec!["0"],
            vec!["0x0"],
            vec!["-1"],
            vec!["window"],
            vec!["0x"],
            vec!["0xffffffffffffffffffff"],
            vec!["1", "2", "3"],
        ] {
            assert!(Options::parse(&args(&values)).is_err(), "{values:?}");
        }
    }

    #[test]
    fn rejects_unbounded_or_invalid_durations() {
        for duration in ["0", "30001", "-1", "forever", "18446744073709551616"] {
            assert!(
                Options::parse(&args(&["1", duration])).is_err(),
                "{duration}"
            );
        }
    }
}
