use crate::model::Rect;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSlot {
    pub window_id: String,
    /// Destination client-area physical pixels, not screen coordinates.
    pub rect: Rect,
    /// Visible part of the placeholder, excluding scrolling ancestors and controls.
    pub clip: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PreviewState {
    Ready,
    Hidden,
    Unavailable,
    SourceGone,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewStatus {
    pub window_id: String,
    pub state: PreviewState,
    pub message: String,
}

/// The host owns the destination and invalidates old requests on every surface transition.
#[derive(Default)]
pub struct PreviewSession {
    generation: u64,
    current: Option<u64>,
}
impl PreviewSession {
    pub fn end(&mut self) {
        self.generation += 1;
        self.current = None;
    }
    pub fn begin(&mut self, available: bool) -> Option<u64> {
        self.end();
        self.current = available.then_some(self.generation);
        self.current
    }
    pub fn accepts(&self, generation: u64) -> bool {
        self.current == Some(generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dismiss_restore_and_reopen_reject_old_preview_requests() {
        let mut session = PreviewSession::default();
        assert!(!session.accepts(0));
        let first = session.begin(true).unwrap();
        assert!(session.accepts(first));
        session.end();
        assert!(!session.accepts(first));
        let next = session.begin(true).unwrap();
        assert!(!session.accepts(first));
        assert!(session.accepts(next));
        assert!(session.begin(false).is_none());
        assert!(!session.accepts(next));
    }
}
