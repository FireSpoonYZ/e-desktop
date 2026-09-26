//! EWMH client backend, not a window manager. Never connects in a Wayland session.
use crate::model::*;
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use x11rb::{
    CURRENT_TIME,
    connection::Connection,
    protocol::{
        randr::ConnectionExt as _,
        xproto::{
            Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, MapState, PropMode,
            Window,
        },
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

fn error(code: ErrorCode, message: impl Into<String>) -> AppError {
    AppError {
        code,
        message: message.into(),
        window_id: None,
    }
}
fn native_error(e: impl std::fmt::Display) -> AppError {
    error(ErrorCode::BackendUnavailable, format!("X11: {e}"))
}
// IDs identify an observed lifetime, never just a reusable XID or PID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Instance {
    window: Window,
    generation: u64,
}
impl Instance {
    fn id(self, session: &str) -> String {
        format!("x11:{session}:{:08x}:{:016x}", self.window, self.generation)
    }
    fn marker(self) -> [u32; 2] {
        [self.generation as u32, (self.generation >> 32) as u32]
    }
    fn verify(self, marker: &[u32]) -> Result<(), AppError> {
        if marker == self.marker() {
            Ok(())
        } else {
            Err(error(
                ErrorCode::WindowGone,
                "X11 window instance expired or its marker was removed",
            ))
        }
    }
}
fn resolve_instance(
    instances: &HashMap<Window, Instance>,
    session: &str,
    value: &str,
) -> Result<Instance, AppError> {
    instances
        .values()
        .find(|i| i.id(session) == value)
        .copied()
        .ok_or_else(|| error(ErrorCode::WindowGone, "Unknown or expired X11 window ID"))
}
static SESSION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct Saved {
    instance: Instance,
    rect: Rect,
    minimized: bool,
    states: Vec<Atom>,
}
struct X11 {
    conn: RustConnection,
    root: Window,
    atoms: HashMap<&'static str, Atom>,
    supported: HashSet<Atom>,
    desktop: u32,
    session: String,
    marker_atom: Atom,
    next_generation: u64,
    instances: HashMap<Window, Instance>,
    saved: HashMap<Window, Saved>,
    minimized: HashSet<Window>,
}
pub struct Backend {
    x11: Option<X11>,
    status: BackendStatus,
}
const ATOMS: &[&str] = &[
    "_NET_SUPPORTED",
    "_NET_SUPPORTING_WM_CHECK",
    "_NET_CLIENT_LIST",
    "_NET_ACTIVE_WINDOW",
    "_NET_CURRENT_DESKTOP",
    "_NET_WM_DESKTOP",
    "_NET_WORKAREA",
    "_NET_WM_NAME",
    "UTF8_STRING",
    "_NET_WM_PID",
    "_NET_WM_WINDOW_TYPE",
    "_NET_WM_WINDOW_TYPE_NORMAL",
    "_NET_WM_WINDOW_TYPE_DIALOG",
    "_NET_WM_STATE",
    "_NET_WM_STATE_HIDDEN",
    "_NET_WM_STATE_MAXIMIZED_HORZ",
    "_NET_WM_STATE_MAXIMIZED_VERT",
    "_NET_WM_STATE_FULLSCREEN",
    "_NET_FRAME_EXTENTS",
    "_NET_MOVERESIZE_WINDOW",
    "_NET_CLOSE_WINDOW",
    "WM_STATE",
    "WM_CHANGE_STATE",
    "WM_PROTOCOLS",
    "WM_DELETE_WINDOW",
    "WM_NORMAL_HINTS",
    "WM_SIZE_HINTS",
    "WM_CLASS",
    "WM_NAME",
];
impl Backend {
    pub fn new() -> Result<Self, AppError> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s.eq_ignore_ascii_case("wayland"))
        {
            return Ok(Self { x11: None, status: BackendStatus {
                kind: BackendKind::Unsupported, availability: BackendAvailability::UnsupportedSession,
                capabilities: Capabilities::default(), message: "Wayland compositor window-control protocol is not implemented; XWayland is not a complete desktop backend.".into(),
            }});
        }
        let (conn, screen) = x11rb::connect(None).map_err(native_error)?;
        let root = conn.setup().roots[screen].root;
        let mut atoms = HashMap::new();
        for name in ATOMS {
            let atom = conn
                .intern_atom(false, name.as_bytes())
                .map_err(native_error)?
                .reply()
                .map_err(native_error)?
                .atom;
            atoms.insert(*name, atom);
        }
        let session = format!(
            "{:x}-{:x}-{:x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(native_error)?
                .as_nanos(),
            SESSION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let marker_atom = conn
            .intern_atom(false, format!("_E_DESKTOP_INSTANCE_{session}").as_bytes())
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?
            .atom;
        let mut x = X11 {
            conn,
            root,
            atoms,
            supported: HashSet::new(),
            desktop: 0,
            session,
            marker_atom,
            next_generation: 1,
            instances: HashMap::new(),
            saved: HashMap::new(),
            minimized: HashSet::new(),
        };
        let wm = x
            .words(root, "_NET_SUPPORTING_WM_CHECK", AtomEnum::WINDOW.into())?
            .first()
            .copied()
            .ok_or_else(|| {
                error(
                    ErrorCode::UnsupportedSession,
                    "An EWMH window manager is required",
                )
            })?;
        if x.words(wm, "_NET_SUPPORTING_WM_CHECK", AtomEnum::WINDOW.into())?
            .first()
            != Some(&wm)
        {
            return Err(error(
                ErrorCode::UnsupportedSession,
                "Invalid EWMH window-manager handshake",
            ));
        }
        x.supported = x
            .words(root, "_NET_SUPPORTED", AtomEnum::ATOM.into())?
            .into_iter()
            .collect();
        if !x.supports("_NET_CLIENT_LIST") {
            return Err(error(
                ErrorCode::UnsupportedSession,
                "Window manager does not expose _NET_CLIENT_LIST",
            ));
        }
        x.desktop = x.current_desktop()?;
        let capabilities = Capabilities {
            enumerate: true,
            placement: x.supports("_NET_MOVERESIZE_WINDOW") && x.supports("_NET_WM_STATE"),
            focus: x.supports("_NET_ACTIVE_WINDOW"),
            close: true,
            minimize: true,
            clipping: false,
            global_shortcuts: false,
            focus_follows_pointer: false,
        };
        Ok(Self { x11: Some(x), status: BackendStatus { kind: BackendKind::X11,
            availability: BackendAvailability::Ready, capabilities,
            message: "X11/EWMH current desktop only. WM requests are verified and may be denied. Decorated-window clipping is unavailable; native desktop switches require restore. RandR scale is reported as 1 physical pixel per unit.".into() } })
    }
    pub fn status(&self) -> BackendStatus {
        self.status.clone()
    }
    fn connection(&mut self) -> Result<&mut X11, AppError> {
        self.x11
            .as_mut()
            .ok_or_else(|| error(ErrorCode::UnsupportedSession, self.status.message.clone()))
    }
    pub fn enumerate(&mut self) -> Result<SystemSnapshot, AppError> {
        self.connection()?.enumerate()
    }
    pub fn apply(&mut self, actions: &[NativeAction]) -> Result<(), AppError> {
        let x = self.connection()?;
        x.check_desktop()?;
        for action in actions {
            let (value, result) = match action {
                NativeAction::Placement {
                    window_id,
                    rect,
                    clip,
                    minimized,
                } => (
                    window_id,
                    (|| {
                        if clip.is_some() {
                            return Err(error(
                                ErrorCode::NotImplemented,
                                "Safe clipping of WM-owned decoration frames is unavailable",
                            ));
                        }
                        let w = x.resolve(window_id)?;
                        x.ensure_managed(w)?;
                        x.capture(w)?;
                        if *minimized {
                            if !x.is_minimized(w)? {
                                // Keep ownership even if verification fails after the WM acts.
                                x.minimized.insert(w);
                            }
                            x.set_minimized(w, true)?;
                        } else {
                            x.set_minimized(w, false)?;
                            x.minimized.remove(&w);
                            x.clear_layout_states(w)?;
                            x.place(w, *rect)?;
                        }
                        Ok(())
                    })(),
                ),
                NativeAction::Focus { window_id } => (
                    window_id,
                    (|| {
                        let w = x.resolve(window_id)?;
                        x.ensure_managed(w)?;
                        if !x.supports("_NET_ACTIVE_WINDOW") {
                            return Err(error(
                                ErrorCode::NotImplemented,
                                "WM does not support focus requests",
                            ));
                        }
                        x.capture(w)?;
                        x.set_minimized(w, false)?;
                        x.minimized.remove(&w);
                        x.message(w, "_NET_ACTIVE_WINDOW", [2, CURRENT_TIME, 0, 0, 0])?;
                        x.wait(
                            || {
                                x.verify_current(w)?;
                                Ok(
                                    x.words(x.root, "_NET_ACTIVE_WINDOW", AtomEnum::WINDOW.into())?
                                        .first()
                                        == Some(&w),
                                )
                            },
                            "WM did not activate the window",
                        )
                    })(),
                ),
                NativeAction::Close { window_id } => (
                    window_id,
                    (|| {
                        let w = x.resolve(window_id)?;
                        x.ensure_managed(w)?;
                        if x.supports("_NET_CLOSE_WINDOW") {
                            x.message(w, "_NET_CLOSE_WINDOW", [CURRENT_TIME, 2, 0, 0, 0])?;
                        } else {
                            if !x
                                .words(w, "WM_PROTOCOLS", AtomEnum::ATOM.into())?
                                .contains(&x.atom("WM_DELETE_WINDOW"))
                            {
                                return Err(error(
                                    ErrorCode::OperationDenied,
                                    "Application does not support graceful close",
                                ));
                            }
                            let event = ClientMessageEvent::new(
                                32,
                                w,
                                x.atom("WM_PROTOCOLS"),
                                [x.atom("WM_DELETE_WINDOW"), CURRENT_TIME, 0, 0, 0],
                            );
                            x.mutate(w, || {
                                x.conn
                                    .send_event(false, w, EventMask::NO_EVENT, event)
                                    .map_err(native_error)?
                                    .check()
                                    .map_err(native_error)
                            })?;
                        }
                        // A save-confirmation dialog may legitimately keep the client alive.
                        x.wait(|| match x.verify_current(w) { Err(e) if e.code == ErrorCode::WindowGone => Ok(true), Err(e) => Err(e), Ok(()) => Ok(!x.client_list()?.contains(&w)) }, "Close requested, but application remains open (possibly awaiting confirmation)")
                    })(),
                ),
                NativeAction::Restore { window_id } => (
                    window_id,
                    x.resolve(window_id).and_then(|w| x.restore_one(w)),
                ),
            };
            if let Err(mut e) = result {
                e.window_id = Some(value.clone());
                return Err(e);
            }
        }
        Ok(())
    }
    pub fn restore(&mut self) -> Result<(), AppError> {
        // Unsupported sessions have never acquired native restore ownership.
        let Some(x) = self.x11.as_mut() else {
            return Ok(());
        };
        let windows: Vec<_> = x.saved.keys().copied().collect();
        let mut errors = Vec::new();
        for w in windows {
            if let Err(e) = x.restore_one(w) {
                errors.push(format!("{}: {e}", x.window_id(w)));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(error(ErrorCode::OperationDenied, errors.join("; ")))
        }
    }
}
impl X11 {
    fn window_id(&self, w: Window) -> String {
        self.instances[&w].id(&self.session)
    }
    fn marker(&self, w: Window) -> Result<Vec<u32>, AppError> {
        let reply = self
            .conn
            .get_property(false, w, self.marker_atom, AtomEnum::CARDINAL, 0, 3)
            .map_err(native_error)?
            .reply()
            .map_err(|e| match e {
                x11rb::errors::ReplyError::X11Error(ref v)
                    if v.error_kind == x11rb::protocol::ErrorKind::Window =>
                {
                    error(ErrorCode::WindowGone, "X11 window was destroyed")
                }
                _ => native_error(e),
            })?;
        Ok(reply.value32().map(|v| v.collect()).unwrap_or_default())
    }
    fn verify(&self, instance: Instance) -> Result<(), AppError> {
        instance.verify(&self.marker(instance.window)?)
    }
    fn verify_current(&self, w: Window) -> Result<(), AppError> {
        let instance = self
            .instances
            .get(&w)
            .ok_or_else(|| error(ErrorCode::WindowGone, "Unknown X11 instance"))?;
        self.verify(*instance)
    }
    fn resolve(&self, value: &str) -> Result<Window, AppError> {
        let instance = resolve_instance(&self.instances, &self.session, value)?;
        self.verify(instance)?;
        Ok(instance.window)
    }
    fn observe(&mut self, w: Window) -> Result<(), AppError> {
        if let Some(instance) = self.instances.get(&w) {
            match self.verify(*instance) {
                Ok(()) => return Ok(()),
                Err(e) if e.code == ErrorCode::WindowGone => (),
                Err(e) => return Err(e),
            }
        }
        // A new lifetime must never inherit placement or manager-minimized ownership.
        self.saved.remove(&w);
        self.minimized.remove(&w);
        self.instances.remove(&w);
        let instance = Instance {
            window: w,
            generation: self.next_generation,
        };
        self.next_generation = self.next_generation.checked_add(1).ok_or_else(|| {
            error(
                ErrorCode::BackendUnavailable,
                "X11 instance sequence exhausted",
            )
        })?;
        self.conn
            .change_property32(
                PropMode::REPLACE,
                w,
                self.marker_atom,
                AtomEnum::CARDINAL,
                &instance.marker(),
            )
            .map_err(native_error)?
            .check()
            .map_err(native_error)?;
        self.verify(instance)?;
        self.instances.insert(w, instance);
        Ok(())
    }

    fn atom(&self, name: &str) -> Atom {
        self.atoms[name]
    }
    fn supports(&self, name: &str) -> bool {
        self.supported.contains(&self.atom(name))
    }
    fn words(&self, w: Window, name: &str, ty: Atom) -> Result<Vec<u32>, AppError> {
        let reply = self
            .conn
            .get_property(false, w, self.atom(name), ty, 0, 16384)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?;
        if reply.bytes_after != 0 {
            return Err(error(
                ErrorCode::BackendUnavailable,
                format!("X11 property {name} exceeds the safety limit"),
            ));
        }
        Ok(reply.value32().map(|v| v.collect()).unwrap_or_default())
    }
    fn text(&self, w: Window, name: &str) -> Result<Vec<u8>, AppError> {
        Ok(self
            .conn
            .get_property(false, w, self.atom(name), AtomEnum::ANY, 0, 16384)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?
            .value)
    }
    fn current_desktop(&self) -> Result<u32, AppError> {
        Ok(self
            .words(self.root, "_NET_CURRENT_DESKTOP", AtomEnum::CARDINAL.into())?
            .first()
            .copied()
            .unwrap_or(0))
    }
    fn check_desktop(&mut self) -> Result<(), AppError> {
        let current = self.current_desktop()?;
        if current != self.desktop && !self.saved.is_empty() {
            return Err(error(
                ErrorCode::OperationDenied,
                "Native X11 desktop changed; pause and restore before refreshing management",
            ));
        }
        self.desktop = current;
        Ok(())
    }
    fn client_list(&self) -> Result<Vec<Window>, AppError> {
        self.words(self.root, "_NET_CLIENT_LIST", AtomEnum::WINDOW.into())
    }
    fn eligible(&self, w: Window) -> Result<bool, AppError> {
        let attrs = self
            .conn
            .get_window_attributes(w)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?;
        if attrs.override_redirect {
            return Ok(false);
        }
        let desktop = self
            .words(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into())?
            .first()
            .copied()
            .unwrap_or(self.desktop);
        if desktop != self.desktop && desktop != u32::MAX {
            return Ok(false);
        }
        let ty = self.words(w, "_NET_WM_WINDOW_TYPE", AtomEnum::ATOM.into())?;
        if !ty.is_empty()
            && !ty.iter().any(|t| {
                *t == self.atom("_NET_WM_WINDOW_TYPE_NORMAL")
                    || *t == self.atom("_NET_WM_WINDOW_TYPE_DIALOG")
            })
        {
            return Ok(false);
        }
        let pid = self
            .words(w, "_NET_WM_PID", AtomEnum::CARDINAL.into())?
            .first()
            .copied()
            .unwrap_or(0);
        Ok(pid == 0 || pid != std::process::id())
    }
    fn ensure_managed(&self, w: Window) -> Result<(), AppError> {
        if !self.client_list()?.contains(&w) && !self.saved.contains_key(&w) {
            return Err(error(ErrorCode::WindowGone, "Window is not an EWMH client"));
        }
        if !self.eligible(w)? {
            return Err(error(
                ErrorCode::OperationDenied,
                "Window is not eligible on this native desktop",
            ));
        }
        Ok(())
    }
    fn extents(&self, w: Window) -> Result<[u32; 4], AppError> {
        let v = self.words(w, "_NET_FRAME_EXTENTS", AtomEnum::CARDINAL.into())?;
        Ok(if v.len() == 4 {
            [v[0], v[1], v[2], v[3]]
        } else {
            [0; 4]
        })
    }
    fn geometry(&self, w: Window) -> Result<Rect, AppError> {
        let g = self
            .conn
            .get_geometry(w)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?;
        let p = self
            .conn
            .translate_coordinates(w, self.root, 0, 0)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?;
        outer_rect(
            i32::from(p.dst_x),
            i32::from(p.dst_y),
            u32::from(g.width),
            u32::from(g.height),
            self.extents(w)?,
        )
    }
    fn is_minimized(&self, w: Window) -> Result<bool, AppError> {
        Ok(
            self.words(w, "WM_STATE", self.atom("WM_STATE"))?.first() == Some(&3)
                || self
                    .words(w, "_NET_WM_STATE", AtomEnum::ATOM.into())?
                    .contains(&self.atom("_NET_WM_STATE_HIDDEN")),
        )
    }
    fn capture(&mut self, w: Window) -> Result<(), AppError> {
        self.verify_current(w)?;
        if !self.saved.contains_key(&w) {
            let saved = Saved {
                instance: self.instances[&w],
                rect: self.geometry(w)?,
                minimized: self.is_minimized(w)?,
                states: self.words(w, "_NET_WM_STATE", AtomEnum::ATOM.into())?,
            };
            self.saved.insert(w, saved);
        }
        Ok(())
    }
    // Hold the server only across identity validation and the checked request,
    // never across WM processing/polling. This closes direct-request XID reuse races.
    fn mutate(
        &self,
        w: Window,
        operation: impl FnOnce() -> Result<(), AppError>,
    ) -> Result<(), AppError> {
        self.conn
            .grab_server()
            .map_err(native_error)?
            .check()
            .map_err(native_error)?;
        let result = self.verify_current(w).and_then(|()| operation());
        let release = self
            .conn
            .ungrab_server()
            .map_err(native_error)
            .and_then(|cookie| cookie.check().map_err(native_error));
        result.and(release)
    }
    fn message(&self, w: Window, name: &str, data: [u32; 5]) -> Result<(), AppError> {
        let event = ClientMessageEvent::new(32, w, self.atom(name), data);
        self.mutate(w, || {
            self.conn
                .send_event(
                    false,
                    self.root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    event,
                )
                .map_err(native_error)?
                .check()
                .map_err(native_error)
        })
    }
    fn wait(
        &self,
        mut predicate: impl FnMut() -> Result<bool, AppError>,
        message: &str,
    ) -> Result<(), AppError> {
        let deadline = Instant::now() + Duration::from_millis(800);
        loop {
            if predicate()? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(error(ErrorCode::OperationDenied, message));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn set_minimized(&self, w: Window, minimized: bool) -> Result<(), AppError> {
        self.verify_current(w)?;
        if self.is_minimized(w)? == minimized {
            return Ok(());
        }
        if minimized {
            self.message(w, "WM_CHANGE_STATE", [3, 0, 0, 0, 0])?;
        } else {
            self.mutate(w, || {
                self.conn
                    .map_window(w)
                    .map_err(native_error)?
                    .check()
                    .map_err(native_error)
            })?;
        }
        self.wait(
            || {
                self.verify_current(w)?;
                Ok(self.is_minimized(w)? == minimized)
            },
            "WM did not change minimized state",
        )
    }
    fn layout_states(&self) -> [Atom; 3] {
        [
            self.atom("_NET_WM_STATE_MAXIMIZED_HORZ"),
            self.atom("_NET_WM_STATE_MAXIMIZED_VERT"),
            self.atom("_NET_WM_STATE_FULLSCREEN"),
        ]
    }
    fn set_state(&self, w: Window, state: Atom, enabled: bool) -> Result<(), AppError> {
        self.verify_current(w)?;
        if self
            .words(w, "_NET_WM_STATE", AtomEnum::ATOM.into())?
            .contains(&state)
            == enabled
        {
            return Ok(());
        }
        self.message(w, "_NET_WM_STATE", [u32::from(enabled), state, 0, 2, 0])?;
        self.wait(
            || {
                self.verify_current(w)?;
                Ok(self
                    .words(w, "_NET_WM_STATE", AtomEnum::ATOM.into())?
                    .contains(&state)
                    == enabled)
            },
            "WM did not update window state",
        )
    }
    fn clear_layout_states(&self, w: Window) -> Result<(), AppError> {
        for state in self.layout_states() {
            self.set_state(w, state, false)?;
        }
        Ok(())
    }
    fn place(&self, w: Window, rect: Rect) -> Result<(), AppError> {
        if !self.supports("_NET_MOVERESIZE_WINDOW") {
            return Err(error(
                ErrorCode::NotImplemented,
                "WM does not support EWMH placement",
            ));
        }
        let (width, height) = client_size(rect, self.extents(w)?)?;
        // NorthWestGravity: x/y address the decoration frame; width/height address the client.
        self.message(
            w,
            "_NET_MOVERESIZE_WINDOW",
            [
                1 | (15 << 8) | (2 << 12),
                rect.x as u32,
                rect.y as u32,
                width,
                height,
            ],
        )?;
        self.wait(
            || {
                self.verify_current(w)?;
                Ok(self.geometry(w)? == rect)
            },
            "WM did not accept exact placement (size hints or policy may constrain it)",
        )
    }
    fn restore_one(&mut self, w: Window) -> Result<(), AppError> {
        let Some(saved) = self.saved.get(&w).cloned() else {
            return Ok(());
        };
        if let Err(e) = self.verify(saved.instance) {
            if e.code == ErrorCode::WindowGone {
                self.saved.remove(&w);
                self.minimized.remove(&w);
            }
            return Err(e);
        }
        if self.geometry(w)? != saved.rect {
            self.set_minimized(w, false)?;
            self.clear_layout_states(w)?;
            self.place(w, saved.rect)?;
        }
        for state in self.layout_states() {
            self.set_state(w, state, saved.states.contains(&state))?;
        }
        self.set_minimized(w, saved.minimized)?;
        self.saved.remove(&w);
        self.minimized.remove(&w);
        Ok(())
    }
    fn monitors(&self) -> Result<Vec<Monitor>, AppError> {
        let resources = self
            .conn
            .randr_get_screen_resources_current(self.root)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?;
        let primary = self
            .conn
            .randr_get_output_primary(self.root)
            .map_err(native_error)?
            .reply()
            .map_err(native_error)?
            .output;
        let work = self.words(self.root, "_NET_WORKAREA", AtomEnum::CARDINAL.into())?;
        let work = work
            .get(self.desktop as usize * 4..)
            .and_then(|v| v.get(..4))
            .map(|v| Rect {
                x: v[0] as i32,
                y: v[1] as i32,
                width: v[2],
                height: v[3],
            });
        let mut result: Vec<Monitor> = Vec::new();
        for output in resources.outputs {
            let info = self
                .conn
                .randr_get_output_info(output, resources.config_timestamp)
                .map_err(native_error)?
                .reply()
                .map_err(native_error)?;
            if info.crtc == 0 {
                continue;
            }
            let crtc = self
                .conn
                .randr_get_crtc_info(info.crtc, resources.config_timestamp)
                .map_err(native_error)?
                .reply()
                .map_err(native_error)?;
            if crtc.width == 0 || crtc.height == 0 {
                continue;
            }
            let bounds = Rect {
                x: i32::from(crtc.x),
                y: i32::from(crtc.y),
                width: u32::from(crtc.width),
                height: u32::from(crtc.height),
            };
            if let Some(existing) = result.iter_mut().find(|m| m.bounds == bounds) {
                existing.primary |= output == primary;
                continue;
            }
            result.push(Monitor {
                id: format!("x11-output:{output}"),
                name: String::from_utf8_lossy(&info.name).into_owned(),
                bounds,
                work_area: work.map(|w| intersection(bounds, w)).unwrap_or(bounds),
                scale_factor: 1.0,
                primary: output == primary,
            });
        }
        if result.is_empty() {
            return Err(error(
                ErrorCode::BackendUnavailable,
                "RandR reports no active outputs",
            ));
        }
        if !result.iter().any(|m| m.primary) {
            result[0].primary = true;
        }
        Ok(result)
    }
    fn enumerate(&mut self) -> Result<SystemSnapshot, AppError> {
        self.check_desktop()?;
        let monitors = self.monitors()?;
        let mut clients = self.client_list()?;
        for w in self.saved.keys() {
            if !clients.contains(w) {
                clients.push(*w);
            }
        }
        self.instances.retain(|w, _| clients.contains(w));
        let mut windows = Vec::new();
        for w in clients {
            // Only BadWindow proves closure; transport/property failures must not erase live clients.
            let attrs = match self
                .conn
                .get_window_attributes(w)
                .map_err(native_error)?
                .reply()
            {
                Ok(v) => v,
                Err(x11rb::errors::ReplyError::X11Error(e))
                    if e.error_kind == x11rb::protocol::ErrorKind::Window =>
                {
                    self.saved.remove(&w);
                    self.minimized.remove(&w);
                    self.instances.remove(&w);
                    continue;
                }
                Err(e) => return Err(native_error(e)),
            };
            self.observe(w)?;
            if !self.eligible(w)? {
                if self.saved.contains_key(&w) {
                    return Err(error(
                        ErrorCode::OperationDenied,
                        "A managed window left the current native desktop or changed eligibility; pause and restore",
                    ));
                }
                continue;
            }
            let minimized = self.is_minimized(w)?;
            if attrs.map_state != MapState::VIEWABLE && !minimized && !self.saved.contains_key(&w) {
                continue;
            }
            let rect = self.geometry(w)?;
            let monitor = monitors
                .iter()
                .max_by_key(|m| {
                    let r = intersection(rect, m.bounds);
                    u64::from(r.width) * u64::from(r.height)
                })
                .unwrap();
            let mut title = self.text(w, "_NET_WM_NAME")?;
            if title.is_empty() {
                title = self.text(w, "WM_NAME")?;
            }
            let class = self.text(w, "WM_CLASS")?;
            let app_name = class
                .split(|b| *b == 0)
                .rfind(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .unwrap_or_default();
            let hints = self.words(w, "WM_NORMAL_HINTS", self.atom("WM_SIZE_HINTS"))?;
            self.verify_current(w)?;
            windows.push(NativeWindow {
                id: self.window_id(w),
                title: String::from_utf8_lossy(&title)
                    .trim_end_matches('\0')
                    .into(),
                app_name,
                process_id: self
                    .words(w, "_NET_WM_PID", AtomEnum::CARDINAL.into())?
                    .first()
                    .copied()
                    .unwrap_or(0),
                monitor_id: monitor.id.clone(),
                rect,
                minimized,
                minimized_by_manager: self.minimized.contains(&w),
                resizable: resizable(&hints),
            });
        }
        let focused_window = self
            .words(self.root, "_NET_ACTIVE_WINDOW", AtomEnum::WINDOW.into())?
            .first()
            .copied()
            .filter(|w| {
                windows.iter().any(|v| {
                    self.instances
                        .get(w)
                        .is_some_and(|i| v.id == i.id(&self.session))
                })
            })
            .map(|w| self.window_id(w));
        Ok(SystemSnapshot {
            monitors,
            windows,
            focused_window,
        })
    }
}
fn intersection(a: Rect, b: Rect) -> Rect {
    let x = i64::from(a.x).max(i64::from(b.x));
    let y = i64::from(a.y).max(i64::from(b.y));
    let right = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width));
    let bottom = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height));
    Rect {
        x: x as i32,
        y: y as i32,
        width: (right - x).max(0) as u32,
        height: (bottom - y).max(0) as u32,
    }
}
fn outer_rect(x: i32, y: i32, width: u32, height: u32, e: [u32; 4]) -> Result<Rect, AppError> {
    let fail = || error(ErrorCode::OperationDenied, "Invalid frame extents");
    Ok(Rect {
        x: i32::try_from(i64::from(x) - i64::from(e[0])).map_err(|_| fail())?,
        y: i32::try_from(i64::from(y) - i64::from(e[2])).map_err(|_| fail())?,
        width: width
            .checked_add(e[0])
            .and_then(|v| v.checked_add(e[1]))
            .ok_or_else(fail)?,
        height: height
            .checked_add(e[2])
            .and_then(|v| v.checked_add(e[3]))
            .ok_or_else(fail)?,
    })
}
fn client_size(rect: Rect, e: [u32; 4]) -> Result<(u32, u32), AppError> {
    let width = i64::from(rect.width) - i64::from(e[0]) - i64::from(e[1]);
    let height = i64::from(rect.height) - i64::from(e[2]) - i64::from(e[3]);
    if !(1..=65535).contains(&width)
        || !(1..=65535).contains(&height)
        || i16::try_from(rect.x).is_err()
        || i16::try_from(rect.y).is_err()
    {
        return Err(error(
            ErrorCode::InvalidCommand,
            "Placement exceeds X11 geometry limits or frame size",
        ));
    }
    Ok((width as u32, height as u32))
}
fn resizable(h: &[u32]) -> bool {
    !(h.len() >= 9 && h[0] & (1 << 4 | 1 << 5) == (1 << 4 | 1 << 5) && h[5] == h[7] && h[6] == h[8])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsupported_session_is_not_an_empty_success() {
        let mut backend = Backend {
            x11: None,
            status: BackendStatus {
                kind: BackendKind::Unsupported,
                availability: BackendAvailability::UnsupportedSession,
                capabilities: Capabilities::default(),
                message: "Wayland control unavailable".into(),
            },
        };
        assert!(!backend.status().capabilities.enumerate);
        assert_eq!(
            backend.enumerate().unwrap_err().code,
            ErrorCode::UnsupportedSession
        );
        assert_eq!(
            backend.apply(&[]).unwrap_err().code,
            ErrorCode::UnsupportedSession
        );
        // A never-enabled unsupported session must still be able to quit.
        backend.restore().unwrap();
    }

    #[test]
    fn reused_xid_does_not_reuse_identity_or_restore_authority() {
        let old = Instance {
            window: 42,
            generation: 1,
        };
        let replacement = Instance {
            window: 42,
            generation: 2,
        };
        assert!(old.verify(&old.marker()).is_ok());
        // Same XID and same process: a recreated window has no private property.
        assert_eq!(old.verify(&[]).unwrap_err().code, ErrorCode::WindowGone);
        assert_eq!(
            old.verify(&replacement.marker()).unwrap_err().code,
            ErrorCode::WindowGone
        );
        assert_ne!(old.id("session"), replacement.id("session"));
        assert_ne!(old.id("session"), old.id("next-session"));
        let mut instances = HashMap::from([(old.window, old)]);
        assert_eq!(
            resolve_instance(&instances, "session", &old.id("session")).unwrap(),
            old
        );
        instances.insert(replacement.window, replacement);
        assert_eq!(
            resolve_instance(&instances, "session", &old.id("session"))
                .unwrap_err()
                .code,
            ErrorCode::WindowGone
        );
        assert_eq!(
            resolve_instance(&instances, "session", &replacement.id("session")).unwrap(),
            replacement
        );
        let saved = Saved {
            instance: old,
            rect: Rect::default(),
            minimized: true,
            states: vec![],
        };
        assert!(saved.instance.verify(&replacement.marker()).is_err());
        assert!(replacement.verify(&replacement.marker()).is_ok());
    }

    #[test]
    fn ids_geometry_and_size_hints() {
        let outer = outer_rect(-1900, 30, 800, 600, [5, 5, 30, 5]).unwrap();
        assert_eq!(
            outer,
            Rect {
                x: -1905,
                y: 0,
                width: 810,
                height: 635
            }
        );
        assert_eq!(client_size(outer, [5, 5, 30, 5]).unwrap(), (800, 600));
        assert!(client_size(Rect { width: 5, ..outer }, [5, 5, 30, 5]).is_err());
        assert!(outer_rect(0, 0, u32::MAX, 1, [1, 1, 1, 1]).is_err());
        let r = intersection(
            Rect {
                x: -100,
                y: 0,
                width: 200,
                height: 100,
            },
            Rect {
                x: 0,
                y: 0,
                width: 200,
                height: 100,
            },
        );
        assert_eq!(
            r,
            Rect {
                x: 0,
                y: 0,
                width: 100,
                height: 100
            }
        );
        assert!(resizable(&[]));
        assert!(!resizable(&[48, 0, 0, 0, 0, 640, 480, 640, 480]));
        assert!(resizable(&[48, 0, 0, 0, 0, 640, 480, 1280, 960]));
    }
}
