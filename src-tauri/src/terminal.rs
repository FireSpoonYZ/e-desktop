//! Persistent background terminal service and explicitly registered terminal windows.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, hash_map::DefaultHasher},
    fs::{self, OpenOptions},
    hash::{Hash, Hasher},
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock, mpsc},
    time::{Duration, Instant},
};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

fn ready_kind() -> String {
    "ready".into()
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    #[serde(rename = "type", default = "ready_kind", skip_serializing)]
    kind: String,
    protocol_version: u32,
    build_id: String,
    local_url: String,
    remote_port: u16,
    admin_token: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct Host {
    pid: u32,
    #[serde(flatten)]
    endpoint: Endpoint,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Health {
    pid: u32,
    build_id: String,
    protocol_version: u32,
    live_sessions: u32,
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

fn local_port(endpoint: &Endpoint) -> Result<u16, String> {
    endpoint
        .local_url
        .strip_prefix("ws://127.0.0.1:")
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .ok_or_else(|| "Invalid terminal host endpoint or protocol".into())
}
fn validate_endpoint(endpoint: &Endpoint) -> Result<(), String> {
    local_port(endpoint)?;
    if endpoint.kind != "ready"
        || endpoint.protocol_version != 1
        || endpoint.admin_token.is_empty()
        || endpoint.admin_token.contains(['\r', '\n'])
    {
        return Err("Invalid terminal host endpoint or protocol".into());
    }
    Ok(())
}
fn host_request(host: &Host, method: &str, path: &str) -> Result<(u16, String), String> {
    let address = SocketAddr::from(([127, 0, 0, 1], local_port(&host.endpoint)?));
    let timeout = Duration::from_secs(2);
    let mut stream = TcpStream::connect_timeout(&address, timeout).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    write!(stream, "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", host.endpoint.admin_token)
        .map_err(|e| e.to_string())?;
    let mut response = String::new();
    stream
        .take(16384)
        .read_to_string(&mut response)
        .map_err(|e| e.to_string())?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or("Invalid host HTTP response")?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or("Invalid host HTTP status")?;
    Ok((status, body.into()))
}
fn health(host: &Host) -> Result<Health, String> {
    validate_endpoint(&host.endpoint)?;
    let (status, body) = host_request(host, "GET", "/health")?;
    if status != 200 {
        return Err("Terminal host health unavailable".into());
    }
    let health: Health = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    if health.pid != host.pid || health.protocol_version != 1 {
        return Err("Terminal host identity or protocol mismatch".into());
    }
    Ok(health)
}
fn pid_exists(pid: u32) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::*};
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut code = 0;
        let alive = GetExitCodeProcess(process, &mut code) != 0 && code == 259;
        CloseHandle(process);
        alive
    }
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}
fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill");
        hide_console(&mut command);
        let _ = command
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
    #[cfg(unix)]
    {
        // PTY shells may call setsid(); snapshot descendants before killing the parent.
        if let Ok(output) = Command::new("ps").args(["-axo", "pid=,ppid="]).output() {
            let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let mut words = line.split_whitespace();
                    Some((words.next()?.parse().ok()?, words.next()?.parse().ok()?))
                })
                .collect();
            for target in descendant_pids(pid, &pairs).into_iter().rev() {
                let _ = Command::new("kill")
                    .args(["-KILL", &target.to_string()])
                    .status();
            }
        }
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .status();
    }
}
fn stop(host: &Host) {
    let _ = host_request(host, "POST", "/shutdown");
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if !pid_exists(host.pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    kill_tree(host.pid);
}

fn source_paths(source: &Path, relative: &Path, paths: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries = fs::read_dir(source.join(relative))?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if matches!(entry.file_name().to_str(), Some("test" | ".git" | ".pi")) {
            continue;
        }
        let path = relative.join(entry.file_name());
        paths.push(path.clone());
        if entry.file_type()?.is_dir() {
            source_paths(source, &path, paths)?;
        }
    }
    Ok(())
}
fn build_id(source: &Path, paths: &[PathBuf]) -> Result<String, String> {
    let mut hash = DefaultHasher::new();
    for path in paths {
        let metadata = fs::metadata(source.join(path)).map_err(|e| e.to_string())?;
        path.hash(&mut hash);
        metadata.len().hash(&mut hash);
        metadata
            .modified()
            .map_err(|e| e.to_string())?
            .hash(&mut hash);
    }
    Ok(format!("{:016x}", hash.finish()))
}
fn relocate(source: &Path, root: &Path, id: &str, paths: &[PathBuf]) -> Result<PathBuf, String> {
    let destination = root.join(id);
    if destination.is_dir() {
        return Ok(destination);
    }
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let temp = root.join(format!(".{id}-{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        fs::create_dir(&temp)?;
        for path in paths {
            let from = source.join(path);
            let to = temp.join(path);
            if fs::metadata(&from)?.is_dir() {
                fs::create_dir(&to)?;
            } else {
                fs::copy(from, to)?;
            }
        }
        fs::rename(&temp, &destination)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temp);
    }
    result.map_err(|e| format!("Unable to relocate terminal host: {e}"))?;
    Ok(destination)
}
fn cleanup_copies(root: &Path, active: &str) {
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name != active
                && name.len() == 16
                && name.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}
fn save_record(path: &Path, host: &Host) -> Result<(), String> {
    let temp = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(host).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    drop(file);
    fs::rename(temp, path).map_err(|e| e.to_string())
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
    fn endpoint(&self, app: &tauri::AppHandle) -> Result<Endpoint, String> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| "Terminal host state unavailable")?;
        let source = if cfg!(debug_assertions) {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../terminal-host")
        } else {
            app.path()
                .resource_dir()
                .map_err(|e| e.to_string())?
                .join("terminal-host")
        };
        if !source.join("src/server.mjs").is_file() {
            return Err(format!(
                "Terminal host not installed at {}. Run npm run prepare:terminal-host before building Tauri.",
                source.display()
            ));
        }
        let mut paths = Vec::new();
        source_paths(&source, Path::new(""), &mut paths).map_err(|e| e.to_string())?;
        let id = build_id(&source, &paths)?;
        let root = app
            .path()
            .app_local_data_dir()
            .map_err(|e| e.to_string())?
            .join("terminal-host");
        let data = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("terminals");
        fs::create_dir_all(&data).map_err(|e| e.to_string())?;
        let record = data.join("host.json");
        let recorded = fs::read(&record)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Host>(&bytes).ok());
        for mut host in [state.take(), recorded].into_iter().flatten() {
            if let Ok(health) = health(&host) {
                host.endpoint.build_id = health.build_id;
                if host.endpoint.build_id == id || health.live_sessions > 0 {
                    // Live sessions win: older host code stays until its shells end.
                    save_record(&record, &host)?;
                    cleanup_copies(&root, &host.endpoint.build_id);
                    let endpoint = host.endpoint.clone();
                    *state = Some(host);
                    return Ok(endpoint);
                }
                stop(&host);
                break;
            }
            // Failed health proves no identity: never kill an unverified recorded PID.
        }
        let _ = fs::remove_file(&record);
        let runtime = relocate(&source, &root, &id, &paths)?;
        let mut command = Command::new(find_node()?);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::{
                CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
            };
            command.creation_flags(
                CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB,
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command
            .current_dir(&runtime)
            .arg(runtime.join("src/server.mjs"))
            .arg("--data-dir")
            .arg(&data)
            .arg("--build-id")
            .arg(&id)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(data.join("host.log"))
                    .map_err(|e| e.to_string())?,
            ));
        let spawned = command.spawn();
        #[cfg(windows)]
        let spawned = match spawned {
            Err(error) if error.raw_os_error() == Some(5) => {
                use std::os::windows::process::CommandExt;
                use windows_sys::Win32::System::Threading::{
                    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
                };
                command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
                command.spawn()
            }
            result => result,
        };
        let mut child = spawned.map_err(|e| format!("Unable to start terminal host: {e}"))?;
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or("Terminal host stdout unavailable")?;
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).take(16384).read_line(&mut line)
                .map_err(|_| "Cannot read terminal host readiness".to_string())
                .and_then(|_| serde_json::from_str::<Endpoint>(&line)
                    .map_err(|_| "Terminal host exited before valid readiness. Check Node/dependencies and whether TCP port 7768 is already in use".to_string()));
            let _ = tx.send(result);
        });
        let ready = rx
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| "Terminal host did not become ready within 20 seconds".to_string())
            .and_then(|result| result)
            .and_then(|endpoint| {
                validate_endpoint(&endpoint)?;
                if endpoint.build_id != id {
                    return Err("Invalid terminal host build ID".into());
                }
                Ok(endpoint)
            });
        // Dropping Child closes only our process handle; it never waits or kills.
        drop(child);
        match ready {
            Ok(endpoint) => {
                let host = Host {
                    pid,
                    endpoint: endpoint.clone(),
                };
                if let Err(error) = save_record(&record, &host) {
                    stop(&host);
                    return Err(error);
                }
                *state = Some(host);
                cleanup_copies(&root, &id);
                Ok(endpoint)
            }
            Err(error) => {
                kill_tree(pid);
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
