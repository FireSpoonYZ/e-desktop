//! lane: rules-spawn-screenshot. `spawn` / `spawnSh` shortcut actions and `spawnAtStartup`.
use std::process::{Command, Stdio};

use crate::model::{AppError, ErrorCode};

fn failed(label: &str, reason: impl std::fmt::Display) -> AppError {
    AppError {
        code: ErrorCode::OperationDenied,
        message: format!("启动 {label} 失败：{reason}"),
        window_id: None,
    }
}

/// Start `[program, args...]` detached. A console program gets its own console window.
pub fn spawn(command: &[String]) -> Result<(), AppError> {
    let (program, args) = command
        .split_first()
        .filter(|(program, _)| !program.trim().is_empty())
        .ok_or_else(|| failed("命令", "缺少程序"))?;
    let mut process = Command::new(program);
    process.args(args);
    start(process, program)
}

/// Run a command line through `cmd.exe /S /C` (Windows, hidden console) or `sh -c`.
pub fn spawn_sh(command: &str) -> Result<(), AppError> {
    if command.trim().is_empty() {
        return Err(failed("命令", "命令为空"));
    }
    #[cfg(windows)]
    let process = {
        use std::os::windows::process::CommandExt;
        let mut process = Command::new("cmd.exe");
        // /S strips exactly the outer quotes, so cmd sees the line verbatim.
        process
            .args(["/S", "/C"])
            .raw_arg(format!("\"{command}\""))
            .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        process
    };
    #[cfg(not(windows))]
    let process = {
        let mut process = Command::new("sh");
        process.arg("-c").arg(command);
        process
    };
    start(process, command)
}

fn start(mut process: Command, label: &str) -> Result<(), AppError> {
    let mut child = process
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| failed(label, e))?;
    // Reap the exit status off the controller thread (no zombies on Unix).
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };

    #[test]
    fn empty_or_missing_programs_are_reported() {
        for command in [vec![], vec![" ".to_string()]] {
            assert_eq!(
                spawn(&command).unwrap_err().code,
                ErrorCode::OperationDenied
            );
        }
        let missing = spawn(&["e-desktop-no-such-program-7d1c".into()]).unwrap_err();
        assert!(missing.message.contains("e-desktop-no-such-program-7d1c"));
        assert_eq!(
            spawn_sh(" \t").unwrap_err().code,
            ErrorCode::OperationDenied
        );
    }

    #[test]
    fn spawn_sh_passes_the_command_line_verbatim_to_the_shell() {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/spawn-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("out {}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // Quotes around a path with a space and a redirection must reach the shell intact.
        spawn_sh(&format!("echo a b> \"{}\"", path.display())).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let text = loop {
            match std::fs::read_to_string(&path) {
                Ok(text) if text.contains('\n') => break text,
                _ if Instant::now() > deadline => panic!("shell did not write {path:?}"),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        assert_eq!(text.trim(), "a b");
        std::fs::remove_file(path).unwrap();
    }
}
