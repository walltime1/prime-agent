//! Starred sessions: ctrl+p stars the selected top-level session, ctrl+s lists only the
//! starred ones.

use super::*;

#[test]
fn ctrl_s_stars_the_selected_session_and_alt_s_lists_only_starred() {
    let mut mode = fresh_mode(vec![
        roster_entry("alpha-agent", "idle", &parent_summary("alpha")),
        roster_entry("beta-agent", "idle", &parent_summary("beta")),
    ]);
    mode.selected = mode
        .rows
        .iter()
        .position(|row| row.title == "beta name")
        .expect("the beta row lists");
    mode.handle_key("ctrl+p");
    mode.handle_key("ctrl+s");
    let titles: Vec<&str> = mode
        .rows
        .iter()
        .filter(|row| row.kind == RowKind::Agent)
        .map(|row| row.title.as_str())
        .collect();
    assert_eq!(titles, ["beta name"]);
    assert_eq!(mode.actions, ["starred", "starred_filter"]);
}
