//! Stable SQLite addresses owned by the BONE application.
//!
//! These strings are part of the on-disk format. Keeping them here means the
//! generic store never needs to know what a workspace, session, or setting is.

use bone_store::{DocumentKey, JournalKey, LeaseKey};

use super::{SessionId, WorkspaceId};

pub(crate) const SETTINGS_NAMESPACE: &str = "settings";
pub(crate) const STATE_NAMESPACE: &str = "state";

pub(crate) fn global_settings() -> DocumentKey {
    DocumentKey::new(SETTINGS_NAMESPACE, "global")
}

pub(crate) fn workspace_registry() -> DocumentKey {
    DocumentKey::new(STATE_NAMESPACE, "workspace-registry")
}

pub(crate) fn workspace_settings(workspace_id: WorkspaceId) -> DocumentKey {
    DocumentKey::new(
        STATE_NAMESPACE,
        format!("workspace/{workspace_id}/settings"),
    )
}

pub(crate) fn session_document(workspace_id: WorkspaceId, session_id: SessionId) -> DocumentKey {
    DocumentKey::new(
        STATE_NAMESPACE,
        format!("workspace/{workspace_id}/session/{session_id}"),
    )
}

pub(crate) fn session_prefix(workspace_id: WorkspaceId) -> String {
    format!("workspace/{workspace_id}/session/")
}

pub(crate) fn session_journal(workspace_id: WorkspaceId, session_id: SessionId) -> JournalKey {
    JournalKey::new(format!(
        "workspace/{workspace_id}/session/{session_id}/events"
    ))
}

pub(crate) fn session_writer_lease(session_id: SessionId) -> LeaseKey {
    // The Store turns this stable logical name into a safe filename.
    LeaseKey::new(format!("session-{session_id}"))
}
