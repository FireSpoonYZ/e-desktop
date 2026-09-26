use e_desktop::{layout::Engine, model::*, platform::Backend};
use std::{collections::BTreeMap, error::Error};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
fn observe(backend: &mut Backend, pid: u32) -> Result<SystemSnapshot> {
    let mut system = backend.enumerate()?;
    system
        .windows
        .retain(|w| w.process_id == pid && w.title.starts_with("e-desktop Smoke "));
    system.windows.sort_by(|a, b| a.title.cmp(&b.title));
    if !system
        .windows
        .iter()
        .any(|w| Some(&w.id) == system.focused_window.as_ref())
    {
        system.focused_window = None;
    }
    Ok(system)
}
fn apply(
    backend: &mut Backend,
    engine: &mut Engine,
    command: Command,
    ids: &[String],
    evidence: &mut Vec<serde_json::Value>,
) -> Result<()> {
    let transition = engine.dispatch(command.clone())?;
    for action in &transition.actions {
        let id = match action {
            NativeAction::Placement { window_id, .. }
            | NativeAction::Focus { window_id }
            | NativeAction::Close { window_id }
            | NativeAction::Restore { window_id } => window_id,
        };
        assert!(
            ids.contains(id),
            "refuse to operate outside the disposable fixture PID"
        );
        if let Err(e) = backend.apply(std::slice::from_ref(action)) {
            if matches!(action, NativeAction::Focus { .. }) && e.code == ErrorCode::OperationDenied
            {
                evidence.push(serde_json::json!({"focusDenied": e.message}));
            } else {
                return Err(e.into());
            }
        }
    }
    evidence.push(serde_json::json!({"command":command,"actions":transition.actions}));
    Ok(())
}
fn checks(backend: &mut Backend, pid: u32, evidence: &mut Vec<serde_json::Value>) -> Result<()> {
    let original = observe(backend, pid)?;
    assert_eq!(
        original.windows.len(),
        3,
        "expected exactly three disposable fixture windows"
    );
    evidence.push(serde_json::json!({"original": &original}));
    let ids: Vec<_> = original.windows.iter().map(|w| w.id.clone()).collect();
    let monitor_id = original.windows[0].monitor_id.clone();
    let mut engine = Engine::new(backend.status());
    engine.set_viewports(
        original
            .monitors
            .iter()
            .map(|m| {
                let top = (44.0 * m.scale_factor).round() as u32;
                let left = (56.0 * m.scale_factor).round() as u32;
                (
                    m.id.clone(),
                    Rect {
                        x: m.work_area.x + left as i32,
                        y: m.work_area.y + top as i32,
                        width: m.work_area.width - left,
                        height: m.work_area.height - top,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
    );
    assert!(engine.reconcile(original.clone())?.actions.is_empty());
    apply(backend, &mut engine, Command::Enable, &ids, evidence)?;
    apply(
        backend,
        &mut engine,
        Command::FocusWindow {
            window_id: ids[0].clone(),
        },
        &ids,
        evidence,
    )?;
    apply(backend, &mut engine, Command::CycleWidth, &ids, evidence)?;
    apply(
        backend,
        &mut engine,
        Command::SetColumnWidth { width: 500 },
        &ids,
        evidence,
    )?;
    apply(
        backend,
        &mut engine,
        Command::AdjustColumnWidth { delta: 50 },
        &ids,
        evidence,
    )?;
    let resized = observe(backend, pid)?;
    assert_eq!(
        resized
            .windows
            .iter()
            .find(|w| w.id == ids[0])
            .unwrap()
            .rect
            .width,
        550
    );
    evidence.push(serde_json::json!({"explicitColumnWidth": 550}));
    apply(
        backend,
        &mut engine,
        Command::Scroll {
            monitor_id: monitor_id.clone(),
            delta: 160,
        },
        &ids,
        evidence,
    )?;
    let scrolled = engine
        .snapshot()
        .monitors
        .iter()
        .find(|m| m.monitor.id == monitor_id)
        .unwrap();
    assert!(
        scrolled
            .pages
            .iter()
            .find(|p| p.id == scrolled.active_page)
            .unwrap()
            .viewport_x
            > 0
    );
    apply(
        backend,
        &mut engine,
        Command::FocusWindow {
            window_id: ids[0].clone(),
        },
        &ids,
        evidence,
    )?;
    apply(
        backend,
        &mut engine,
        Command::MoveWindow {
            direction: Direction::Right,
        },
        &ids,
        evidence,
    )?;
    assert!(
        engine
            .snapshot()
            .monitors
            .iter()
            .flat_map(|m| &m.pages)
            .flat_map(|p| &p.columns)
            .any(|c| c.windows.len() == 2)
    );
    let stacked = observe(backend, pid)?;
    let before_height = stacked
        .windows
        .iter()
        .find(|w| w.id == ids[0])
        .unwrap()
        .rect
        .height;
    apply(
        backend,
        &mut engine,
        Command::AdjustWindowHeight { delta: 50 },
        &ids,
        evidence,
    )?;
    let resized = observe(backend, pid)?;
    assert_eq!(
        resized
            .windows
            .iter()
            .find(|w| w.id == ids[0])
            .unwrap()
            .rect
            .height,
        before_height + 50
    );
    apply(
        backend,
        &mut engine,
        Command::ResetWindowHeights,
        &ids,
        evidence,
    )?;
    let reset = observe(backend, pid)?;
    assert_eq!(
        reset
            .windows
            .iter()
            .find(|w| w.id == ids[0])
            .unwrap()
            .rect
            .height,
        before_height
    );
    evidence.push(serde_json::json!({"stackHeightAdjustedAndReset": true}));
    let page = engine
        .snapshot()
        .monitors
        .iter()
        .find(|m| m.monitor.id == monitor_id)
        .unwrap()
        .active_page
        .clone();
    apply(
        backend,
        &mut engine,
        Command::ToggleFloating,
        &ids,
        evidence,
    )?;
    let before = observe(backend, pid)?;
    let normal = before.windows.iter().find(|w| w.id == ids[0]).unwrap().rect;
    engine.reconcile(before)?;
    apply(
        backend,
        &mut engine,
        Command::AddPage {
            monitor_id: monitor_id.clone(),
        },
        &ids,
        evidence,
    )?;
    let hidden = observe(backend, pid)?;
    assert_eq!(hidden.windows.len(), 3);
    assert!(
        hidden
            .windows
            .iter()
            .all(|w| w.minimized && w.minimized_by_manager)
    );
    evidence.push(serde_json::json!({"hidden": &hidden}));
    engine.reconcile(hidden)?;
    apply(
        backend,
        &mut engine,
        Command::SwitchPage {
            monitor_id: monitor_id.clone(),
            page_id: page,
        },
        &ids,
        evidence,
    )?;
    let shown = observe(backend, pid)?;
    let floating = shown.windows.iter().find(|w| w.id == ids[0]).unwrap();
    assert!(!floating.minimized);
    assert_eq!(
        floating.rect, normal,
        "floating rectangle must survive iconic enumeration"
    );
    apply(
        backend,
        &mut engine,
        Command::ToggleFullscreen,
        &ids,
        evidence,
    )?;
    let full = observe(backend, pid)?;
    assert_eq!(
        full.windows.iter().find(|w| w.id == ids[0]).unwrap().rect,
        engine
            .snapshot()
            .monitors
            .iter()
            .find(|m| m.monitor.id == monitor_id)
            .unwrap()
            .viewport
    );
    apply(
        backend,
        &mut engine,
        Command::ToggleFullscreen,
        &ids,
        evidence,
    )?;
    apply(
        backend,
        &mut engine,
        Command::ToggleFloating,
        &ids,
        evidence,
    )?;
    let destinations: Vec<_> = engine
        .snapshot()
        .monitors
        .iter()
        .filter(|m| m.monitor.id != monitor_id)
        .map(|m| (m.monitor.id.clone(), m.active_page.clone(), m.viewport))
        .collect();
    for (destination, page, viewport) in destinations {
        apply(
            backend,
            &mut engine,
            Command::MoveWindowToPage {
                window_id: ids[0].clone(),
                page_id: page,
            },
            &ids,
            evidence,
        )?;
        let actual = observe(backend, pid)?;
        let moved = actual.windows.iter().find(|w| w.id == ids[0]).unwrap();
        assert_eq!(moved.monitor_id, destination);
        assert!(!moved.minimized);
        assert!(moved.rect.x >= viewport.x && moved.rect.y >= viewport.y);
        assert!(
            i64::from(moved.rect.x) + i64::from(moved.rect.width)
                <= i64::from(viewport.x) + i64::from(viewport.width)
        );
        assert!(
            i64::from(moved.rect.y) + i64::from(moved.rect.height)
                <= i64::from(viewport.y) + i64::from(viewport.height)
        );
        evidence.push(serde_json::json!({"crossMonitor":moved,"viewport":viewport}));
    }
    apply(backend, &mut engine, Command::Disable, &ids, evidence)?;
    backend.restore()?;
    let restored = observe(backend, pid)?;
    for initial in &original.windows {
        let actual = restored
            .windows
            .iter()
            .find(|w| w.id == initial.id)
            .unwrap();
        assert_eq!(
            actual.rect, initial.rect,
            "restore rectangle: {}",
            initial.title
        );
        assert_eq!(actual.minimized, initial.minimized);
        assert!(!actual.minimized_by_manager);
    }
    evidence.push(serde_json::json!({"restored": &restored}));
    backend.apply(&[NativeAction::Close {
        window_id: ids[2].clone(),
    }])?;
    for _ in 0..40 {
        if !observe(backend, pid)?
            .windows
            .iter()
            .any(|w| w.id == ids[2])
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        !observe(backend, pid)?
            .windows
            .iter()
            .any(|w| w.id == ids[2])
    );
    assert_eq!(
        backend
            .apply(&[NativeAction::Focus {
                window_id: ids[2].clone()
            }])
            .unwrap_err()
            .code,
        ErrorCode::WindowGone
    );
    evidence.push(serde_json::json!({"gracefulCloseAndStaleId":true}));
    for id in &ids[..2] {
        backend.apply(&[NativeAction::Close {
            window_id: id.clone(),
        }])?;
    }
    Ok(())
}
fn main() -> Result<()> {
    let pid: u32 = std::env::args()
        .nth(1)
        .ok_or("fixture PID required")?
        .parse()?;
    let output = std::env::args().nth(2).ok_or("evidence file required")?;
    let mut backend = Backend::new()?;
    let mut evidence = vec![];
    let result = checks(&mut backend, pid, &mut evidence);
    let restore = backend.restore();
    evidence.push(serde_json::json!({"passed":result.is_ok(),"failure":result.as_ref().err().map(|e|e.to_string()),"finalRestore":restore.as_ref().err().map(|e|e.to_string())}));
    std::fs::write(output, serde_json::to_string_pretty(&evidence)?)?;
    restore?;
    result
}
