//! lane: rules-spawn-screenshot. Screenshot commands on the controller thread: resolve the
//! target here, then capture, save and copy on the backend's screenshot thread.
use super::*;
use crate::screenshot::Kind;

impl Controller {
    pub(super) fn screenshot(
        &mut self,
        sender: &RequestSender,
        kind: Kind,
        path: Option<String>,
    ) -> Result<(), AppError> {
        #[cfg(target_os = "windows")]
        {
            use crate::platform::screenshot::{Target, start};
            let snapshot = self.engine.snapshot();
            let target = match kind {
                Kind::Region => Target::Region,
                Kind::Screen => Target::Screen(
                    snapshot
                        .monitors
                        .iter()
                        .find(|m| Some(&m.monitor.id) == snapshot.active_monitor.as_ref())
                        .or(snapshot.monitors.first())
                        .map(|m| m.monitor.bounds)
                        .ok_or_else(|| {
                            error(ErrorCode::BackendUnavailable, "没有可截取的显示器。")
                        })?,
                ),
                Kind::Window => Target::Window(
                    self.backend
                        .as_ref()
                        .ok_or_else(|| {
                            error(ErrorCode::BackendUnavailable, "原生窗口后端尚未连接。")
                        })?
                        .screenshot_window(snapshot.focused_window.as_deref())?,
                ),
            };
            // An open palette or overview leaves the screen first; Dismiss hands focus back.
            let mut settle = false;
            for (surface, host) in [
                (Surface::Overview, &self.overview_host),
                (Surface::Commands, &self.commands_host),
            ] {
                if host.is_some() {
                    settle = true;
                    let _ = sender.try_send(Request::Dismiss(surface, IpcTrace::NONE));
                }
            }
            let sender = sender.clone();
            start(target, path, settle, move |issue| {
                let _ = sender.try_send(Request::Issue(issue));
            })
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (sender, kind, path);
            Err(crate::screenshot::unsupported())
        }
    }
}
