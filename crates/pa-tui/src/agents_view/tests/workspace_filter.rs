//! The ctrl+f workspace filter: all, then each workspace in the rows (global last), then all.

use super::*;

#[test]
fn ctrl_f_cycles_all_then_each_workspace_then_all() {
    let mut alpha = parent_summary("alpha");
    alpha["workspace"] = serde_json::json!("alpha");
    let mut mode = fresh_mode(vec![
        roster_entry("alpha-agent", "idle", &alpha),
        roster_entry("global-agent", "idle", &parent_summary("global")),
    ]);
    let mut filters = Vec::new();
    for _ in 0..3 {
        mode.handle_key("ctrl+f");
        filters.push(mode.workspace_filter.clone());
    }
    assert_eq!(
        filters,
        [Some("alpha".to_string()), Some("global".to_string()), None]
    );
}
