use crate::model::{AppError, Command, Snapshot};

#[tauri::command]
fn get_snapshot() -> Snapshot {
    Snapshot::default()
}

#[tauri::command]
fn execute(command: Command) -> Result<Snapshot, AppError> {
    let _ = command;
    Err(AppError::not_implemented("Desktop integration"))
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![get_snapshot, execute])
        .run(tauri::generate_context!())
        .expect("failed to run e-desktop");
}
