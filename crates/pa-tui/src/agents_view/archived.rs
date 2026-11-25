//! Archived sessions: the `app.agents.archive` marks on top-level agent rows and the
//! `app.agents.archived` toggle that shows them again. Archived sessions are hidden by
//! default so the list stays focused on live work, while their transcripts and cost records
//! stay on disk. The archive persists as the archived session files in
//! `<agent dir>/archived-sessions.json`, so it survives restarts and follows a session between
//! its live and saved rows.
use super::stars::{star_key, RowListUpdate, Stars};
use super::AgentsViewMode;
use crate::agents_view_forest::AgentsViewRow;

/// The store's file name under the agent dir.
pub(crate) const ARCHIVED_FILE: &str = "archived-sessions.json";

/// The archived session files (normalized paths): the same persisted set as the stars,
/// kept in its own file.
pub(super) type ArchivedSessions = Stars;

/// Archive the session at `session_file` (as written in a session summary): used by `/retire`,
/// which has no agents view. Already archived is a no-op.
///
/// # Errors
///
/// Error when the store is unreadable or cannot be written.
pub(crate) fn archive_session_file(
    store_path: std::path::PathBuf,
    session_file: &str,
) -> std::io::Result<()> {
    let mut archived = ArchivedSessions::load(store_path)?;
    let file = crate::agents_view_state::normalize_session_file_path(session_file);
    if !archived.contains(&file) {
        archived.toggle(&file)?;
    }
    Ok(())
}

impl AgentsViewMode {
    pub(super) fn is_archived(&self, row: &AgentsViewRow) -> bool {
        star_key(row).is_some_and(|file| self.archived.contains(&file))
    }

    /// Drop the archived top-level rows with every row nested under them, at any depth.
    pub(super) fn drop_archived_trees(&self, rows: &mut Vec<AgentsViewRow>) {
        let mut hidden_below: Option<usize> = None;
        rows.retain(|row| {
            if let Some(depth) = hidden_below {
                if row.depth > depth {
                    return false;
                }
                hidden_below = None;
            }
            if row.parent_identity.is_none() && self.is_archived(row) {
                hidden_below = Some(row.depth);
                return false;
            }
            true
        });
    }

    /// `app.agents.archive` (default ctrl+a): archive or unarchive the selected session.
    pub(super) fn toggle_archive_for_selected(&mut self) -> RowListUpdate {
        let Some(file) = self.rows.get(self.selected).and_then(star_key) else {
            self.set_status("Only a saved top-level session can be archived");
            return RowListUpdate::Unchanged;
        };
        match self.archived.toggle(&file) {
            Ok(true) => {
                self.actions.push("archived");
                self.set_status("Archived (ctrl+e shows archived sessions)");
                // An archive while archived rows are hidden drops the row from the list.
                if self.show_archived {
                    RowListUpdate::Unchanged
                } else {
                    RowListUpdate::NeedsRebuild
                }
            }
            Ok(false) => {
                self.set_status("Unarchived");
                RowListUpdate::Unchanged
            }
            Err(error) => {
                self.set_status(&format!("Could not save archived sessions: {error}"));
                RowListUpdate::Unchanged
            }
        }
    }

    /// `app.agents.archived` (default ctrl+e): show the archived sessions, or hide them again.
    pub(super) fn toggle_show_archived(&mut self) -> RowListUpdate {
        self.show_archived = !self.show_archived;
        let status = match (self.show_archived, self.archived.is_empty()) {
            (true, true) => "No archived sessions yet (ctrl+a archives the selected one)",
            (true, false) => {
                self.actions.push("archived_filter");
                "Showing archived sessions (ctrl+e: hide)"
            }
            (false, _) => "Hiding archived sessions",
        };
        self.set_status(status);
        RowListUpdate::NeedsRebuild
    }
}
