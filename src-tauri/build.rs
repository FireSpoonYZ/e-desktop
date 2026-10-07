fn main() {
    #[cfg(feature = "desktop")]
    {
        let out_dir = std::env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR");
        clear_terminal_host(std::path::Path::new(&out_dir));
        tauri_build::build();
    }
}

#[cfg(any(feature = "desktop", test))]
fn clear_terminal_host(out_dir: &std::path::Path) {
    // Match tauri-build's OUT_DIR -> profile directory, including custom targets.
    let destination = out_dir
        .ancestors()
        .nth(3)
        .expect("OUT_DIR must be <profile>/build/<package>/out")
        .join("terminal-host");
    if let Err(error) = std::fs::remove_dir_all(&destination) {
        if error.kind() != std::io::ErrorKind::NotFound {
            panic!(
                "Failed to clear terminal resources at {} before Tauri copies them: {error}",
                destination.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::clear_terminal_host;
    use std::{fs, time::SystemTime};

    // rustc --edition=2024 --test src-tauri/build.rs -o target/build-rs-tests.exe
    #[test]
    fn clears_only_terminal_host_in_the_out_dir_profile() {
        let root = std::env::temp_dir().join(format!(
            "e-desktop-terminal-resources-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for profile in [
            "target/debug",
            "custom-target/release",
            "custom-target/x86_64-pc-windows-msvc/debug",
        ] {
            let profile = root.join(profile);
            let out_dir = profile.join("build/e-desktop-test/out");
            let terminal_host = profile.join("terminal-host");
            let prebuilds = terminal_host.join("node_modules/node-pty/prebuilds/win32-x64");
            let neighbor = profile.join("other-resource");
            fs::create_dir_all(&out_dir).unwrap();
            fs::create_dir_all(&prebuilds).unwrap();
            fs::create_dir_all(&neighbor).unwrap();
            fs::write(prebuilds.join("conpty.node"), "stale native addon").unwrap();
            fs::write(terminal_host.join("obsolete.txt"), "stale resource").unwrap();
            fs::write(neighbor.join("keep.txt"), "neighbor").unwrap();
            fs::write(out_dir.join("keep.txt"), "build output").unwrap();

            clear_terminal_host(&out_dir);
            assert!(!terminal_host.exists(), "{}", terminal_host.display());
            assert_eq!(fs::read(neighbor.join("keep.txt")).unwrap(), b"neighbor");
            assert_eq!(fs::read(out_dir.join("keep.txt")).unwrap(), b"build output");
            clear_terminal_host(&out_dir); // Already absent is a successful no-op.

            // A real filesystem error must fail the build, not be silently ignored.
            fs::write(&terminal_host, "not a directory").unwrap();
            assert!(std::panic::catch_unwind(|| clear_terminal_host(&out_dir)).is_err());
            assert_eq!(fs::read(&terminal_host).unwrap(), b"not a directory");
        }
        fs::remove_dir_all(root).unwrap();
    }
}
