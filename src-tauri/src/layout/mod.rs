use crate::model::{AppError, BackendStatus, Command, Snapshot, SystemSnapshot, Transition};

/// Pure layout seam. No OS handles, Tauri types, or platform calls belong here.
pub struct Engine {
    snapshot: Snapshot,
}

impl Engine {
    pub fn new(backend: BackendStatus) -> Self {
        Self {
            snapshot: Snapshot {
                backend,
                ..Snapshot::default()
            },
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn reconcile(&mut self, _system: SystemSnapshot) -> Result<Transition, AppError> {
        Err(AppError::not_implemented("Layout reconciliation"))
    }

    pub fn dispatch(&mut self, _command: Command) -> Result<Transition, AppError> {
        Err(AppError::not_implemented("Layout commands"))
    }
}
