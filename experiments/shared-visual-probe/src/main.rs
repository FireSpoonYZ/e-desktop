mod pe;
#[cfg(windows)]
mod windows;

use std::{ffi::OsString, path::Path, process::ExitCode};

const UNSUPPORTED: &str = "demo_status=unsupported: no build-specific ABI verified; export presence does not establish a callable signature";

#[derive(Debug, PartialEq)]
enum Mode {
    Help,
    Inventory,
    Pe(OsString),
    Demo,
}

fn parse(args: &[OsString]) -> Result<Mode, &'static str> {
    match args {
        [] => Ok(Mode::Help),
        [flag] if flag == "--help" || flag == "-h" => Ok(Mode::Help),
        [flag] if flag == "--inventory" => Ok(Mode::Inventory),
        [flag] if flag == "--demo" => Ok(Mode::Demo),
        [flag, path] if flag == "--pe" && !path.is_empty() => Ok(Mode::Pe(path.clone())),
        _ => Err("usage: shared-visual-probe [--inventory | --pe FILE | --demo | --help]"),
    }
}

fn report(path: &Path) -> Result<(), String> {
    // Read bytes only: no LoadLibrary/GetProcAddress or executable mapping.
    let file = std::fs::File::open(path).map_err(|e| format!("{path:?}: {e}"))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > 64 * 1024 * 1024 {
        return Err("PE file exceeds 64 MiB probe limit".into());
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("PE file exceeds 64 MiB probe limit".into());
    }
    let image = pe::parse(&bytes).map_err(|e| format!("{path:?}: {e}"))?;
    println!(
        "file={path:?} machine=0x{:04x} exports={}",
        image.machine,
        image.exports.len()
    );
    for (ordinal, hint) in [
        (114, "article: query thumbnail type"),
        (147, "article: create shared thumbnail visual"),
        (162, "article: query source size"),
        (
            163,
            "article: create shared virtual-desktop/multi-window visual",
        ),
        (
            164,
            "article: update shared virtual-desktop/multi-window visual",
        ),
    ] {
        if let Some(export) = image.exports.iter().find(|e| e.ordinal == ordinal) {
            println!(
                "ordinal={ordinal} present=true rva=0x{:x} names={:?} forwarder={:?} hint={hint:?}",
                export.rva, export.names, export.forwarder
            );
        } else {
            println!("ordinal={ordinal} present=false hint={hint:?}");
        }
    }
    for name in [
        "DwmRegisterThumbnail",
        "DwmUpdateThumbnailProperties",
        "DCompositionCreateDevice",
        "DCompositionCreateDevice2",
        "DCompositionCreateDevice3",
    ] {
        println!(
            "name={name:?} present={}",
            image
                .exports
                .iter()
                .any(|e| e.names.iter().any(|n| n == name))
        );
    }
    Ok(())
}

fn run(mode: Mode) -> Result<(), String> {
    match mode {
        Mode::Help => println!(
            "Read-only shared-DWM-visual feasibility probe.
--inventory   Read current Windows build and system DLL export tables.
--pe FILE     Inspect an on-disk PE export table (works off Windows too).
--demo        Refuse unsupported private-ABI execution (exit 2).
No DLLs are loaded. No windows, devices, hooks or visuals are created."
        ),
        Mode::Pe(path) => {
            report(Path::new(&path))?;
            println!("{UNSUPPORTED}");
        }
        Mode::Inventory => {
            #[cfg(windows)]
            windows::inventory()?;
            #[cfg(not(windows))]
            return Err(
                "OS inventory requires Windows; use --pe FILE for offline inspection".into(),
            );
            #[cfg(windows)]
            println!("{UNSUPPORTED}");
        }
        Mode::Demo => return Err(UNSUPPORTED.into()),
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match parse(&args).map_err(String::from).and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_requires_explicit_inventory_and_never_accepts_demo_overrides() {
        let args = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(parse(&[]), Ok(Mode::Help));
        assert_eq!(parse(&args(&["--inventory"])), Ok(Mode::Inventory));
        assert_eq!(
            parse(&args(&["--pe", "dwmapi.dll"])),
            Ok(Mode::Pe("dwmapi.dll".into()))
        );
        assert!(parse(&args(&["--pe"])).is_err());
        assert!(parse(&args(&["--demo", "--force"])).is_err());
        assert!(parse(&args(&["--inventory", "--demo"])).is_err());
        assert_eq!(run(Mode::Demo), Err(UNSUPPORTED.into()));
    }
}
