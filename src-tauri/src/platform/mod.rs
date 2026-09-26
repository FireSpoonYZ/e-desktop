use crate::model::{AppError, BackendStatus, NativeAction, SystemSnapshot};

/// Baseline only. Integration replaces this with cfg-selected static Backend exports.
/// Platform workers implement the same inherent methods; no trait/plugin registry.
pub struct Backend;

impl Backend {
    pub fn new() -> Result<Self, AppError> {
        Err(AppError::not_implemented("Native backend"))
    }

    pub fn status(&self) -> BackendStatus {
        BackendStatus::default()
    }

    pub fn enumerate(&mut self) -> Result<SystemSnapshot, AppError> {
        Err(AppError::not_implemented("Native enumeration"))
    }

    pub fn apply(&mut self, _actions: &[NativeAction]) -> Result<(), AppError> {
        Err(AppError::not_implemented("Native actions"))
    }

    pub fn restore(&mut self) -> Result<(), AppError> {
        Err(AppError::not_implemented("Native restoration"))
    }
}
