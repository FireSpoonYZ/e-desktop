//! Local sidecar lifetime and explicitly registered terminal windows.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock, mpsc},
    time::{Duration, Instant},
};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    #[serde(rename = "type")]
    kind: String,
    protocol_version: u32,
    local_url: String,
    remote_port: u16,
    admin_token: String,
}

struct Host {
    child: Child,
    endpoint: Endpoint,
}
#[derive(Default)]
pub struct TerminalHost(Mutex<Option<Host>>);

static TITLES: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();
fn titles() -> &'static Mutex<BTreeSet<String>> {
    TITLES.get_or_init(Mutex::default)
}
pub fn eligible_title(title: &str) -> bool {
    titles().lock().is_ok_and(|set| set.contains(title))
}
pub fn unregister(window: &tauri::Window) {
    if let Some(id) = window.label().strip_prefix("terminal-session-") {
        titles()
            .lock()
            .unwrap()
            .remove(&format!("e-desktop terminal · {id}"));
    }
}

fn find_node() -> Result<PathBuf, String> {
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    let candidates: Vec<PathBuf> = if let Some(path) = std::env::var_os("E_DESKTOP_NODE") {
        vec![PathBuf::from(path)]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|path| path.join(name))
            .collect()
    };
    candidates.into_iter().filter(|path| path.is_absolute() && path.is_file())
        .find_map(|path| {
            let mut command = Command::new(&path);
            hide_console(&mut command);
            let output = command.arg("--version").output().ok()?;
            let major = String::from_utf8_lossy(&output.stdout).trim().trim_start_matches('v')
                .split('.').next()?.parse::<u32>().ok()?;
            (output.status.success() && major >= 22).then(|| path.canonicalize().ok()).flatten()
        })
        .ok_or_else(|| "Terminal host requires Node.js 22 or newer. Install Node, restart e-desktop, or set E_DESKTOP_NODE to an absolute node executable path.".into())
}
fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

fn stop(child: &mut Child) {
    if let Some(mut input) = child.stdin.take() {
        let _ = input.write_all(b"shutdown\n");
    }
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill");
        hide_console(&mut command);
        let _ = command
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .output();
    }
    #[cfg(unix)]
    {
        // PTY shells may call setsid(), so killing only the Node process group
        // is insufficient. Snapshot descendants before killing their parent.
        let processes = Command::new("ps").args(["-axo", "pid=,ppid="]).output();
        if let Ok(output) = processes {
            let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let mut words = line.split_whitespace();
                    Some((words.next()?.parse().ok()?, words.next()?.parse().ok()?))
                })
                .collect();
            for pid in descendant_pids(child.id(), &pairs).into_iter().rev() {
                let _ = Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .status();
            }
        }
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A force-killed e-desktop never runs `stop`; the job still ends the host and its shells
/// when the app's last handle closes, so no orphan keeps `node-pty` loaded.
#[cfg(windows)]
fn kill_with_app(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::*;
    static JOB: OnceLock<usize> = OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as _,
            std::mem::size_of_val(&limits) as u32,
        );
        job as usize
    });
    // Failure leaves the old behaviour: the host only stops through `stop`.
    unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) };
}

#[cfg(any(unix, test))]
fn descendant_pids(root: u32, processes: &[(u32, u32)]) -> Vec<u32> {
    let mut descendants = vec![root];
    let mut index = 0;
    // ponytail: shutdown-only O(n²) process scan; index parents if process counts demand it.
    while index < descendants.len() {
        for &(pid, parent) in processes {
            if parent == descendants[index] && !descendants.contains(&pid) {
                descendants.push(pid);
            }
        }
        index += 1;
    }
    descendants
}

impl TerminalHost {
    pub fn shutdown(&self) {
        if let Ok(mut host) = self.0.lock() {
            if let Some(mut host) = host.take() {
                stop(&mut host.child);
            }
        }
    }
    fn endpoint(&self, app: &tauri::AppHandle) -> Result<Endpoint, String> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| "Terminal host state unavailable")?;
        if let Some(host) = state.as_mut() {
            if host.child.try_wait().map_err(|e| e.to_string())?.is_none() {
                return Ok(host.endpoint.clone());
            }
            *state = None;
        }
        let entry = if cfg!(debug_assertions) {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../terminal-host/src/server.mjs")
        } else {
            app.path()
                .resource_dir()
                .map_err(|e| e.to_string())?
                .join("terminal-host/src/server.mjs")
        };
        if !entry.is_file() {
            return Err(format!(
                "Terminal host not installed at {}. Run npm run prepare:terminal-host before building Tauri.",
                entry.display()
            ));
        }
        let data = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("terminals");
        std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
        let mut command = Command::new(find_node()?);
        hide_console(&mut command);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .arg(entry)
            .arg("--data-dir")
            .arg(data)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Unable to start terminal host: {e}"))?;
        #[cfg(windows)]
        kill_with_app(&child);
        let stdout = child
            .stdout
            .take()
            .ok_or("Terminal host stdout unavailable")?;
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout)
                .take(16384)
                .read_line(&mut line)
                .map_err(|_| "Cannot read terminal host readiness".to_string())
                .and_then(|_| {
                    serde_json::from_str::<Endpoint>(&line)
                        .map_err(|_| "Terminal host exited before valid readiness. Check Node/dependencies and whether TCP port 7768 is already in use".to_string())
                });
            let _ = tx.send(result);
        });
        let ready = rx
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| "Terminal host did not become ready within 20 seconds".to_string())
            .and_then(|result| result)
            .and_then(|endpoint| {
                let port = endpoint
                    .local_url
                    .strip_prefix("ws://127.0.0.1:")
                    .and_then(|value| value.parse::<u16>().ok());
                if endpoint.kind != "ready"
                    || endpoint.protocol_version != 1
                    || port == Some(0)
                    || port.is_none()
                    || endpoint.admin_token.is_empty()
                {
                    Err("Invalid terminal host endpoint or protocol".into())
                } else {
                    Ok(endpoint)
                }
            });
        match ready {
            Ok(endpoint) => {
                *state = Some(Host {
                    child,
                    endpoint: endpoint.clone(),
                });
                Ok(endpoint)
            }
            Err(error) => {
                stop(&mut child);
                Err(error)
            }
        }
    }
}

#[tauri::command]
pub async fn terminal_endpoint(app: tauri::AppHandle) -> Result<Endpoint, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<TerminalHost>().endpoint(&app))
        .await
        .map_err(|e| e.to_string())?
}

fn session_window(id: &str) -> Result<(String, String, String), String> {
    if id.is_empty() || id.len() > 128 {
        return Err("Invalid terminal session ID".into());
    }
    // Opaque IDs must not become window labels or unescaped URL syntax.
    let key: String = id.bytes().map(|byte| format!("{byte:02x}")).collect();
    let mut url = tauri::Url::parse("http://localhost/index.html").expect("static URL");
    url.query_pairs_mut()
        .append_pair("surface", "terminal")
        .append_pair("sessionId", id);
    Ok((
        format!("terminal-session-{key}"),
        format!("index.html?{}", url.query().expect("query was added")),
        format!("e-desktop terminal · {key}"),
    ))
}

#[tauri::command]
pub async fn open_terminal_window(
    app: tauri::AppHandle,
    session_id: Option<String>,
) -> Result<(), String> {
    let (label, url, title) = if let Some(id) = session_id {
        session_window(&id)?
    } else {
        (
            "terminal-manager".into(),
            "index.html?surface=terminals".into(),
            "e-desktop terminals".into(),
        )
    };
    if let Some(window) = app.get_webview_window(&label) {
        window.unminimize().map_err(|e| e.to_string())?;
        return window.set_focus().map_err(|e| e.to_string());
    }
    // Only exact, registered own-process titles bypass the platform PID exclusion.
    // Never update these native titles from untrusted OSC shell output.
    if label != "terminal-manager" {
        titles().lock().unwrap().insert(title.clone());
    }
    if let Err(error) = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
        .title(&title)
        .inner_size(960.0, 640.0)
        .build()
    {
        titles().lock().unwrap().remove(&title);
        return Err(error.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shutdown_tree_includes_grandchildren_but_not_unrelated_processes() {
        assert_eq!(
            descendant_pids(10, &[(13, 11), (20, 1), (11, 10), (10, 1), (12, 10)]),
            vec![10, 11, 12, 13]
        );
    }
    #[test]
    fn session_ids_cannot_collide_with_manager_or_inject_url_parameters() {
        let (label, url, _) = session_window("manager&surface=topbar/中文").unwrap();
        assert!(label.starts_with("terminal-session-"));
        assert_ne!(label, "terminal-manager");
        let parsed = tauri::Url::parse(&format!("http://localhost/{url}")).unwrap();
        let pairs: Vec<_> = parsed.query_pairs().collect();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].1, "terminal");
        assert_eq!(pairs[1].1, "manager&surface=topbar/中文");
        assert!(session_window("").is_err());
        assert!(session_window(&"x".repeat(129)).is_err());
    }
    #[test]
    fn only_registered_terminal_titles_are_eligible() {
        let title = "e-desktop terminal · registry-test";
        assert!(!eligible_title(title));
        titles().lock().unwrap().insert(title.into());
        assert!(eligible_title(title));
        assert!(!eligible_title("e-desktop"));
        assert!(!eligible_title("e-desktop terminals"));
        titles().lock().unwrap().remove(title);
        assert!(!eligible_title(title));
    }
}
