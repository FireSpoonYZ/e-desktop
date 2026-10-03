//! IPC server threads (lane: persistence-ipc). Actions go through the controller queue like UI
//! commands; queries and event streams read the published snapshot and never block the
//! controller or UI threads.
use std::{
    io::{BufRead, BufReader, Write},
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::Duration,
};

use serde_json::Value;

use super::{AppError, ErrorCode, IpcTrace, Request, RequestSender, error};
use crate::{
    config::Config,
    ipc::{self, IpcRequest},
    model::Snapshot,
};

const EVENT_POLL: Duration = Duration::from_millis(100);

fn config() -> &'static Mutex<Config> {
    static CONFIG: OnceLock<Mutex<Config>> = OnceLock::new();
    CONFIG.get_or_init(Mutex::default)
}

/// The controller publishes its effective configuration for the `config` query.
pub(super) fn publish_config(current: &Config) {
    let mut config = config().lock().unwrap_or_else(|e| e.into_inner());
    if *config != *current {
        *config = current.clone();
    }
}

pub(super) fn start(sender: RequestSender, snapshot: Arc<Mutex<Snapshot>>) -> Result<(), AppError> {
    let mut listener = ipc::Listener::bind().map_err(|e| {
        error(
            ErrorCode::BackendUnavailable,
            format!("IPC 端点 {} 不可用：{e}", ipc::endpoint().display()),
        )
    })?;
    std::thread::Builder::new()
        .name("ipc-server".into())
        .spawn(move || {
            loop {
                match listener.accept() {
                    Ok(stream) => {
                        let sender = sender.clone();
                        let snapshot = snapshot.clone();
                        let _ = std::thread::Builder::new()
                            .name("ipc-client".into())
                            .spawn(move || serve(stream, sender, snapshot));
                    }
                    Err(_) => std::thread::sleep(EVENT_POLL),
                }
            }
        })
        .map_err(|e| error(ErrorCode::BackendUnavailable, e.to_string()))?;
    Ok(())
}

fn enqueue(sender: &RequestSender, request: Request) -> Result<(), AppError> {
    #[cfg(target_os = "windows")]
    return sender.try_send(request);
    #[cfg(not(target_os = "windows"))]
    sender.try_send(request).map_err(|_| {
        error(
            ErrorCode::BackendUnavailable,
            "窗口控制器暂时忙碌或已退出，请稍后重试。",
        )
    })
}

fn handle(
    request: IpcRequest,
    sender: &RequestSender,
    snapshot: &Mutex<Snapshot>,
) -> Result<Value, AppError> {
    match request {
        IpcRequest::Action(command) => {
            let (tx, rx) = mpsc::sync_channel(1);
            let trace = IpcTrace::begin("pipe.action");
            enqueue(sender, Request::Command(command, Some(tx), trace))?;
            rx.recv()
                .map_err(|_| error(ErrorCode::BackendUnavailable, "窗口控制器已停止。"))??;
            Ok(Value::Null)
        }
        IpcRequest::ShortcutAction(action) => {
            enqueue(sender, Request::ShortcutAction(action))?;
            Ok(Value::Null)
        }
        IpcRequest::Query(query) => {
            let snapshot = snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let config = config().lock().unwrap_or_else(|e| e.into_inner()).clone();
            Ok(ipc::query(&snapshot, &config, query))
        }
        IpcRequest::EventStream(_) => unreachable!("event streams are served by stream_events"),
    }
}

fn serve(stream: ipc::Stream, sender: RequestSender, snapshot: Arc<Mutex<Snapshot>>) {
    let mut writer = &stream;
    for line in BufReader::new(&stream).lines() {
        let Ok(line) = line else {
            return;
        };
        if line.trim().is_empty() {
            continue;
        }
        let request = ipc::parse_request(&line);
        if let Ok(IpcRequest::EventStream(_)) = request {
            return stream_events(writer, &snapshot);
        }
        let reply = ipc::reply(request.and_then(|r| handle(r, &sender, &snapshot)));
        if writeln!(writer, "{reply}").is_err() {
            return;
        }
    }
}

/// Polls the published snapshot so a slow client never holds up the controller.
fn stream_events(mut writer: &ipc::Stream, snapshot: &Mutex<Snapshot>) {
    if writeln!(writer, "{}", ipc::reply(Ok(Value::Null))).is_err() {
        return;
    }
    let mut previous = String::new();
    loop {
        let current = snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let line = ipc::event(&current);
        if line != previous {
            if writeln!(writer, "{line}").is_err() {
                return;
            }
            previous = line;
        }
        std::thread::sleep(EVENT_POLL);
    }
}
