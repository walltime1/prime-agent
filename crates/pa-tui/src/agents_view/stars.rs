//! Starred sessions: the `app.agents.star` marks on top-level agent rows and the
//! `app.agents.starred` filter that lists only them. Stars persist as the starred
//! session files in `<agent dir>/starred-sessions.json`, so they survive restarts and
//! follow a session between its live and saved rows.
use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

use super::AgentsViewMode;
use crate::agents_view_forest::{AgentsViewRow, RowKind};

/// The store's file name under the agent dir.
pub(super) const STARS_FILE: &str = "starred-sessions.json";

/// The starred session files (normalized paths) and where they persist.
#[derive(Debug, Default)]
pub(super) struct Stars {
    /// The store file; `None` keeps the stars in memory only.
    path: Option<PathBuf>,
    files: BTreeSet<String>,
}

impl Stars {
    /// The stars kept at `path`; a missing file holds none.
    pub(super) fn load(path: PathBuf) -> std::io::Result<Self> {
        let files = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(error) => return Err(error),
        };
        Ok(Self {
            path: Some(path),
            files,
        })
    }

    /// Star `file`, or unstar it when starred, and persist the set. Returns whether `file`
    /// is starred now; a failed write leaves the set unchanged.
    pub(super) fn toggle(&mut self, file: &str) -> std::io::Result<bool> {
        let starred = !self.files.remove(file);
        if starred {
            self.files.insert(file.to_string());
        }
        if let Err(error) = self.save() {
            if starred {
                self.files.remove(file);
            } else {
                self.files.insert(file.to_string());
            }
            return Err(error);
        }
        Ok(starred)
    }

    /// Replace the store via a sibling temp file and rename, so a concurrent reader never
    /// sees a partial list.
    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&self.files).map_err(std::io::Error::other)?;
        let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })
    }
}

/// The star key of `row`: a top-level agent row's normalized session file.
pub(super) fn star_key(row: &AgentsViewRow) -> Option<String> {
    if row.kind != RowKind::Agent {
        return None;
    }
    row.summary
        .get("sessionFile")
        .and_then(Value::as_str)
        .filter(|file| !file.is_empty())
        .map(crate::agents_view_state::normalize_session_file_path)
}

impl AgentsViewMode {
    pub(super) fn is_starred(&self, row: &AgentsViewRow) -> bool {
        star_key(row).is_some_and(|file| self.stars.files.contains(&file))
    }

    /// The selected row's star key: only a top-level agent with a session file stars.
    pub(super) fn star_target(&self) -> Option<String> {
        self.rows.get(self.selected).and_then(star_key)
    }

    /// `app.agents.star` (default ctrl+p): star or unstar the selected session.
    pub(super) fn toggle_star_for_selected(&mut self) {
        let Some(file) = self.star_target() else {
            self.set_status("Only a saved top-level session can be starred");
            return;
        };
        match self.stars.toggle(&file) {
            Ok(true) => {
                self.actions.push("starred");
                self.set_status("Starred");
            }
            Ok(false) => {
                self.set_status("Unstarred");
                // An unstar under the starred filter drops the row from the list.
                if self.starred_only {
                    self.rebuild_rows();
                }
            }
            Err(error) => self.set_status(&format!("Could not save stars: {error}")),
        }
    }

    /// `app.agents.starred` (default ctrl+s): list only the starred sessions, or all again.
    pub(super) fn toggle_starred_only(&mut self) {
        self.starred_only = !self.starred_only;
        self.rebuild_rows();
        let status = match (self.starred_only, self.stars.files.is_empty()) {
            (true, true) => "No starred sessions yet (ctrl+p stars the selected one)",
            (true, false) => {
                self.actions.push("starred_filter");
                "Showing starred sessions (ctrl+s: all)"
            }
            (false, _) => "Showing all sessions",
        };
        self.set_status(status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stars_persist_across_loads_and_a_malformed_store_fails_to_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent").join(STARS_FILE);
        let mut stars = Stars::load(path.clone()).unwrap();
        assert!(stars.toggle("/x/a.jsonl").unwrap());
        assert!(stars.toggle("/x/b.jsonl").unwrap());
        assert!(!stars.toggle("/x/a.jsonl").unwrap());
        assert_eq!(
            Stars::load(path.clone()).unwrap().files,
            BTreeSet::from(["/x/b.jsonl".to_string()])
        );
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(
            Stars::load(path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}
