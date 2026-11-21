//! Archived sessions: ctrl+a archives the selected top-level session and hides it, ctrl+e
//! shows the archived ones again.

use super::*;

fn listed_titles(mode: &AgentsViewMode) -> Vec<&str> {
    mode.rows
        .iter()
        .filter(|row| row.kind == RowKind::Agent)
        .map(|row| row.title.as_str())
        .collect()
}

#[test]
fn ctrl_a_archives_and_hides_the_selected_session_and_ctrl_e_shows_it_again() {
    let mut mode = fresh_mode(vec![
        roster_entry("alpha-agent", "idle", &parent_summary("alpha")),
        roster_entry("beta-agent", "idle", &parent_summary("beta")),
    ]);
    mode.selected = mode
        .rows
        .iter()
        .position(|row| row.title == "beta name")
        .expect("the beta row lists");
    mode.handle_key("ctrl+a");
    assert_eq!(listed_titles(&mode), ["alpha name"]);
    mode.handle_key("ctrl+e");
    assert_eq!(listed_titles(&mode), ["alpha name", "beta name"]);
    assert_eq!(mode.actions, ["archived", "archived_filter"]);
}

#[test]
fn archive_session_file_persists_once_under_the_normalized_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("agent")
        .join(super::super::archived::ARCHIVED_FILE);
    super::super::archived::archive_session_file(path.clone(), "/x/a.jsonl").unwrap();
    super::super::archived::archive_session_file(path.clone(), "/x/a.jsonl").unwrap();
    let archived = super::super::archived::ArchivedSessions::load(path).unwrap();
    assert!(
        archived.contains(&crate::agents_view_state::normalize_session_file_path(
            "/x/a.jsonl"
        ))
    );
}
