//! External control (lane: persistence-ipc), in the style of `niri msg`: each connection
//! sends one JSON request per line and gets one JSON reply per line. Windows listens on
//! `\\.\pipe\e-desktop-<user>` (current user only); Unix on `$XDG_RUNTIME_DIR/e-desktop.sock`.
use std::{io, path::PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    config::{Config, ShortcutAction},
    model::{AppError, Command, ErrorCode, Snapshot},
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum IpcRequest {
    /// `{"action": <Command>}`: runs on the controller like a UI command.
    Action(Command),
    /// `{"shortcutAction": <ShortcutAction>}`: as if a shortcut bound to it was pressed.
    ShortcutAction(ShortcutAction),
    Query(Query),
    /// `{"eventStream": true}`: after the reply, one `{"snapshot": ...}` line per change.
    EventStream(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Query {
    Snapshot,
    Windows,
    Pages,
    FocusedWindow,
    Config,
}

fn invalid(message: String) -> AppError {
    AppError {
        code: ErrorCode::InvalidCommand,
        message,
        window_id: None,
    }
}

pub fn parse_request(line: &str) -> Result<IpcRequest, AppError> {
    let request: IpcRequest =
        serde_json::from_str(line).map_err(|e| invalid(format!("无效的 IPC 请求：{e}")))?;
    match &request {
        IpcRequest::EventStream(false) => Err(invalid("eventStream 只能为 true。".into())),
        IpcRequest::ShortcutAction(ShortcutAction::Unbind {}) => {
            Err(invalid("unbind 不是可执行的动作。".into()))
        }
        _ => Ok(request),
    }
}

pub fn query(snapshot: &Snapshot, config: &Config, query: Query) -> Value {
    match query {
        Query::Snapshot => json!(snapshot),
        Query::Windows => json!(snapshot.windows),
        Query::Pages => Value::Array(
            snapshot
                .monitors
                .iter()
                .map(|m| {
                    json!({
                        "monitorId": m.monitor.id,
                        "monitorName": m.monitor.name,
                        "activePage": m.active_page,
                        "pages": m.pages,
                    })
                })
                .collect(),
        ),
        Query::FocusedWindow => json!(
            snapshot
                .windows
                .iter()
                .find(|w| Some(&w.native.id) == snapshot.focused_window.as_ref())
        ),
        Query::Config => json!(config),
    }
}

/// One reply line without the newline: `{"ok": value}` or `{"error": AppError}`.
pub fn reply(result: Result<Value, AppError>) -> String {
    match result {
        Ok(value) => json!({ "ok": value }),
        Err(issue) => json!({ "error": issue }),
    }
    .to_string()
}

pub fn event(snapshot: &Snapshot) -> String {
    json!({ "snapshot": snapshot }).to_string()
}

fn user() -> String {
    let name = std::env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).unwrap_or_default();
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() { "user".into() } else { name }
}

#[cfg(windows)]
pub fn endpoint() -> PathBuf {
    PathBuf::from(format!(r"\\.\pipe\e-desktop-{}", user()))
}

#[cfg(unix)]
pub fn endpoint() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(directory) if !directory.is_empty() => PathBuf::from(directory).join("e-desktop.sock"),
        // macOS has no XDG runtime directory; its temp directory is already per user.
        _ => std::env::temp_dir().join(format!("e-desktop-{}.sock", user())),
    }
}

#[cfg(windows)]
pub type Stream = std::fs::File;
#[cfg(unix)]
pub type Stream = std::os::unix::net::UnixStream;

#[cfg(windows)]
pub fn connect() -> io::Result<Stream> {
    use std::os::windows::fs::OpenOptionsExt;
    const ERROR_PIPE_BUSY: i32 = 231;
    // Identification only: the server cannot act as this client.
    const SECURITY_IDENTIFICATION: u32 = 1 << 16;
    let mut attempts = 0;
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(endpoint())
        {
            // Every instance is busy for the moment between a connect and the next listen.
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 40 => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

#[cfg(unix)]
pub fn connect() -> io::Result<Stream> {
    Stream::connect(endpoint())
}

#[cfg(windows)]
pub use pipe::Listener;

#[cfg(windows)]
mod pipe {
    use std::{
        fs::File,
        io,
        os::windows::io::{AsRawHandle, FromRawHandle},
        ptr::null_mut,
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
            TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX},
        System::{
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
            },
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    fn checked(ok: i32) -> io::Result<()> {
        if ok != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn current_user_sid() -> io::Result<String> {
        unsafe {
            let mut token: HANDLE = null_mut();
            checked(OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY,
                &mut token,
            ))?;
            let mut length = 0;
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut length);
            // u64 storage keeps TOKEN_USER aligned.
            let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
            let result = checked(GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            ));
            CloseHandle(token);
            result?;
            let user = &*(buffer.as_ptr() as *const TOKEN_USER);
            let mut text = null_mut();
            checked(ConvertSidToStringSidW(user.User.Sid, &mut text))?;
            let length = (0..).take_while(|&i| *text.add(i) != 0).count();
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
            LocalFree(text.cast());
            Ok(sid)
        }
    }

    /// Named pipe server. One instance always waits for the next client.
    pub struct Listener {
        name: Vec<u16>,
        /// LocalAlloc'd descriptor granting the current user, and nobody else, access.
        security: PSECURITY_DESCRIPTOR,
        pending: File,
    }

    // The descriptor is immutable after creation and only read by CreateNamedPipeW.
    unsafe impl Send for Listener {}

    impl Listener {
        /// Fails when another process (another e-desktop) already owns the pipe name.
        pub fn bind() -> io::Result<Self> {
            let sddl = wide(&format!("D:P(A;;GA;;;{})", current_user_sid()?));
            let mut security = null_mut();
            checked(unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut security,
                    null_mut(),
                )
            })?;
            let name = wide(&super::endpoint().to_string_lossy());
            match create(&name, security, true) {
                Ok(pending) => Ok(Self {
                    name,
                    security,
                    pending,
                }),
                Err(e) => {
                    unsafe { LocalFree(security) };
                    Err(e)
                }
            }
        }

        pub fn accept(&mut self) -> io::Result<File> {
            let handle = self.pending.as_raw_handle() as HANDLE;
            let error = if unsafe { ConnectNamedPipe(handle, null_mut()) } != 0 {
                None
            } else {
                // A client that connected before this call is already connected.
                let error = io::Error::last_os_error();
                (error.raw_os_error() != Some(ERROR_PIPE_CONNECTED as i32)).then_some(error)
            };
            // A client that left before the connect completed still consumes this instance.
            let next = create(&self.name, self.security, false)?;
            let client = std::mem::replace(&mut self.pending, next);
            match error {
                Some(error) => Err(error),
                None => Ok(client),
            }
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            unsafe { LocalFree(self.security) };
        }
    }

    fn create(name: &[u16], security: PSECURITY_DESCRIPTOR, first: bool) -> io::Result<File> {
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security,
            bInheritHandle: 0,
        };
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX
                    | if first {
                        FILE_FLAG_FIRST_PIPE_INSTANCE
                    } else {
                        0
                    },
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                &attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_handle(handle as _) })
    }
}

/// Unix socket server; a stale socket file from a crashed run is replaced.
#[cfg(unix)]
pub struct Listener(std::os::unix::net::UnixListener);

#[cfg(unix)]
impl Listener {
    pub fn bind() -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let path = endpoint();
        if Stream::connect(&path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("{} 已被另一个实例使用", path.display()),
            ));
        }
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self(listener))
    }

    pub fn accept(&mut self) -> io::Result<Stream> {
        self.0.accept().map(|(stream, _)| stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Direction, MonitorState, NativeWindow, Page, Rect, WindowState};

    #[test]
    fn requests_parse_actions_shortcut_actions_queries_and_event_stream() {
        assert_eq!(
            parse_request(r#"{"action":{"type":"focusDirection","direction":"left"}}"#).unwrap(),
            IpcRequest::Action(Command::FocusDirection {
                direction: Direction::Left
            })
        );
        assert_eq!(
            parse_request(r#"{"shortcutAction":{"type":"overview"}}"#).unwrap(),
            IpcRequest::ShortcutAction(ShortcutAction::Overview {})
        );
        assert_eq!(
            parse_request(r#"{"shortcutAction":{"type":"page","number":2}}"#).unwrap(),
            IpcRequest::ShortcutAction(ShortcutAction::Page {
                number: 2,
                move_window: false
            })
        );
        assert_eq!(
            parse_request(r#"{"query":"focusedWindow"}"#).unwrap(),
            IpcRequest::Query(Query::FocusedWindow)
        );
        assert_eq!(
            parse_request(r#" {"eventStream":true} "#).unwrap(),
            IpcRequest::EventStream(true)
        );
        for line in [
            "",
            "{",
            "{}",
            r#"{"eventStream":false}"#,
            r#"{"query":"everything"}"#,
            r#"{"action":{"type":"invented"}}"#,
            r#"{"action":{"type":"refresh"},"query":"windows"}"#,
            r#"{"shortcutAction":{"type":"unbind"}}"#,
            r#"{"shortcutAction":{"type":"quit","extra":1}}"#,
            r#"{"unknown":1}"#,
        ] {
            let error = parse_request(line).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidCommand, "{line}");
        }
    }

    #[test]
    fn queries_and_replies_have_one_line_json_shapes() {
        let window = |id: &str| WindowState {
            native: NativeWindow {
                id: id.into(),
                title: format!("title {id}"),
                app_name: "app.exe".into(),
                process_id: 1,
                monitor_id: "m".into(),
                rect: Rect::default(),
                minimized: false,
                minimized_by_manager: false,
                resizable: true,
            },
            floating: false,
            fullscreen: false,
        };
        let mut snapshot = Snapshot {
            windows: vec![window("a"), window("b")],
            focused_window: Some("b".into()),
            ..Snapshot::default()
        };
        snapshot.monitors.push(MonitorState {
            monitor: crate::model::Monitor {
                id: "m".into(),
                name: "Main".into(),
                bounds: Rect::default(),
                work_area: Rect::default(),
                scale_factor: 1.0,
                primary: true,
            },
            pages: vec![Page {
                id: "p".into(),
                name: "Desktop 1".into(),
                columns: vec![],
                floating_windows: vec![],
                viewport_x: 0,
            }],
            active_page: "p".into(),
            viewport: Rect::default(),
        });
        let config = Config::default();
        assert_eq!(
            query(&snapshot, &config, Query::FocusedWindow)["native"]["id"],
            "b"
        );
        assert_eq!(
            query(&snapshot, &config, Query::Windows)[1]["native"]["title"],
            "title b"
        );
        let pages = query(&snapshot, &config, Query::Pages);
        assert_eq!(pages[0]["monitorId"], "m");
        assert_eq!(pages[0]["activePage"], "p");
        assert_eq!(pages[0]["pages"][0]["name"], "Desktop 1");
        assert_eq!(
            query(&snapshot, &config, Query::Config)["restoreLayout"],
            true
        );
        assert_eq!(
            query(&snapshot, &config, Query::Snapshot)["focusedWindow"],
            "b"
        );
        snapshot.focused_window = None;
        assert_eq!(query(&snapshot, &config, Query::FocusedWindow), Value::Null);
        assert_eq!(reply(Ok(Value::Null)), r#"{"ok":null}"#);
        let error: Value = serde_json::from_str(&reply(Err(invalid("bad".into())))).unwrap();
        assert_eq!(error["error"]["code"], "invalidCommand");
        assert_eq!(error["error"]["message"], "bad");
        let line = event(&snapshot);
        assert!(!line.contains('\n'));
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["snapshot"]["enabled"],
            false
        );
    }

    #[test]
    fn endpoint_is_per_user() {
        let path = endpoint().to_string_lossy().into_owned();
        #[cfg(windows)]
        assert!(path.starts_with(r"\\.\pipe\e-desktop-"), "{path}");
        #[cfg(unix)]
        assert!(path.ends_with(".sock"), "{path}");
    }

    /// Real pipe round trip on this machine; no desktop windows are involved. Skipped when
    /// a running e-desktop already owns the endpoint.
    #[cfg(windows)]
    #[test]
    fn pipe_round_trip_and_second_server_is_refused() {
        use std::io::{BufRead, BufReader, Write};
        let Ok(mut listener) = Listener::bind() else {
            return;
        };
        assert!(
            Listener::bind().is_err(),
            "the first instance owns the name"
        );
        let server = std::thread::spawn(move || {
            let stream = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let reply = reply(parse_request(&line).map(|_| Value::Null));
            (&stream)
                .write_all(format!("{reply}\n").as_bytes())
                .unwrap();
        });
        let stream = connect().unwrap();
        (&stream).write_all(b"{\"query\":\"windows\"}\n").unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        assert_eq!(line, "{\"ok\":null}\n");
        server.join().unwrap();
    }
}
