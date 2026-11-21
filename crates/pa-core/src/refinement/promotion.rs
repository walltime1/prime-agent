//! Workspace inspection and promotion: list a workspace's harness entries, and move one into
//! the global store so every session sees it. Promotion goes through the refinement applier,
//! so both sides leave a `RefinementResult` that `/refine rollback` can invert.

use std::path::Path;

use super::planner::{
    apply_refinement_proposal, kind_name, ApplyOptions, RefinementEdit, RefinementProposal,
};
use super::{
    append_global_refinement, executor::generate_refinement_id, factory_enabled,
    get_global_harness_state_dir, get_workspace_harness_state_dir, load_harness_state,
    try_update_harness_state, HarnessEntry, HarnessScope, RefinementAction, RefinementKind,
    RefinementResult,
};

/// The world-model database a workspace may carry beside its harness store.
const WORLD_MODEL_FILE_NAME: &str = "world_model.db";

/// What a promotion wrote: the global create/update and the workspace delete that undoes the
/// shadowing copy. Each id is a `/refine rollback` target in its own store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    pub kind: RefinementKind,
    pub global_refinement_id: String,
    pub workspace_refinement_id: String,
}

/// The `/workspace info` text: one line per workspace harness entry (`(review due)` marks an
/// entry past its 90-day review date) plus the world-model database when the workspace has one.
///
/// # Errors
///
/// Error when `workspace` is not a valid workspace name.
pub fn format_workspace_info(
    agent_dir: &Path,
    workspace: &str,
    now_millis: u64,
) -> anyhow::Result<String> {
    let harness_dir = get_workspace_harness_state_dir(agent_dir, workspace)?;
    let state = load_harness_state(&harness_dir, HarnessScope::Global);
    let entries: Vec<&HarnessEntry> = state.entries.values().flat_map(|e| e.values()).collect();
    let mut lines = vec![format!(
        "Workspace '{workspace}': {} harness entries",
        entries.len()
    )];
    for entry in entries {
        let due = if super::ranking::entry_review_due(entry, Some(now_millis)) {
            " (review due)"
        } else {
            ""
        };
        lines.push(format!(
            "- {} {}: {}{due}",
            kind_name(entry.kind),
            entry.id,
            entry.title
        ));
    }
    let world_model = harness_dir
        .parent()
        .map(|root| root.join(WORLD_MODEL_FILE_NAME))
        .and_then(|path| std::fs::metadata(path).ok());
    if let Some(metadata) = world_model {
        lines.push(format!(
            "World model: {WORLD_MODEL_FILE_NAME} present ({} bytes)",
            metadata.len()
        ));
    }
    Ok(lines.join("\n"))
}

/// Move the workspace entry `entry_id` into the global store and remove the workspace copy
/// (workspaces layer over global on read, so a leftover copy would shadow the promoted one).
/// An existing global entry with the same kind and id is replaced; its prior state is in the
/// recorded global refinement.
///
/// # Errors
///
/// Error when the workspace name is invalid, no entry or more than one entry has `entry_id`,
/// the factory opt-in is off for a factory entry, or a store cannot be updated. The global
/// store is written first, so a failure after that leaves a harmless duplicate, never a loss.
pub fn promote_workspace_entry(
    agent_dir: &Path,
    workspace: &str,
    entry_id: &str,
) -> anyhow::Result<Promotion> {
    let workspace_dir = get_workspace_harness_state_dir(agent_dir, workspace)?;
    let global_dir = get_global_harness_state_dir(agent_dir);
    let workspace_state = load_harness_state(&workspace_dir, HarnessScope::Global);
    let matches: Vec<&HarnessEntry> = workspace_state
        .entries
        .values()
        .filter_map(|entries| entries.get(entry_id))
        .collect();
    let entry = match matches.as_slice() {
        [] => anyhow::bail!("Workspace '{workspace}' has no harness entry '{entry_id}'."),
        [entry] => *entry,
        _ => anyhow::bail!(
            "Entry id '{entry_id}' exists under several kinds in workspace '{workspace}'; \
             rename one before promoting."
        ),
    };
    let global_has_entry = load_harness_state(&global_dir, HarnessScope::Global)
        .entries
        .get(&entry.kind)
        .is_some_and(|entries| entries.contains_key(entry_id));
    let reason = format!("Promote from workspace '{workspace}'");
    let publish = RefinementProposal {
        summary: format!(
            "Promote {} {entry_id} from workspace '{workspace}' to global",
            kind_name(entry.kind)
        ),
        rationale: reason.clone(),
        expected_outcome: "Every session sees the entry without the workspace.".to_string(),
        edits: vec![RefinementEdit {
            action: Some(if global_has_entry {
                RefinementAction::Update
            } else {
                RefinementAction::Create
            }),
            kind: Some(entry.kind),
            id: Some(entry.id.clone()),
            title: Some(entry.title.clone()),
            content: Some(entry.content.clone()),
            path: Some(entry.path.clone()),
            reference: Some(entry.reference.clone()),
            arguments: Some(entry.arguments.clone()),
            metadata: Some(entry.metadata.clone()),
            reason: Some(reason.clone()),
        }],
    };
    let retire = RefinementProposal {
        summary: format!(
            "Remove {} {entry_id} from workspace '{workspace}' after promotion",
            kind_name(entry.kind)
        ),
        rationale: reason.clone(),
        expected_outcome: "The promoted global entry is no longer shadowed.".to_string(),
        edits: vec![RefinementEdit {
            action: Some(RefinementAction::Delete),
            kind: Some(entry.kind),
            id: Some(entry.id.clone()),
            reason: Some(reason),
            ..Default::default()
        }],
    };
    let kind = entry.kind;
    let factory_allowed = factory_enabled(agent_dir);
    let mut global_result = apply_to_store(&global_dir, &publish, factory_allowed)?;
    let mut workspace_result = apply_to_store(&workspace_dir, &retire, factory_allowed)?;
    global_result.workspace = None;
    workspace_result.workspace = Some(workspace.to_string());
    append_global_refinement(&global_dir, &global_result)?;
    append_global_refinement(&workspace_dir, &workspace_result)?;
    Ok(Promotion {
        kind,
        global_refinement_id: global_result.id,
        workspace_refinement_id: workspace_result.id,
    })
}

/// Apply one single-edit proposal to the store at `harness_dir` under its lock, saving only when
/// the edit applied.
fn apply_to_store(
    harness_dir: &Path,
    proposal: &RefinementProposal,
    factory_enabled: bool,
) -> anyhow::Result<RefinementResult> {
    let (mut result, state_path) =
        try_update_harness_state(harness_dir, HarnessScope::Global, |state| {
            let result = apply_refinement_proposal(
                state,
                proposal,
                ApplyOptions {
                    id: generate_refinement_id(),
                    rollback_of: None,
                    scope: Some(HarnessScope::Global),
                    baseline_state: None,
                    factory_enabled,
                },
            );
            match result.applied_edits.iter().find(|edit| !edit.applied) {
                Some(edit) => anyhow::bail!(
                    "{}",
                    edit.error.as_deref().unwrap_or("the edit did not apply")
                ),
                None => Ok(result),
            }
        })?;
    result.harness_state_path = state_path.to_string_lossy().to_string();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::refinement::planner::rollback_proposal;
    use crate::refinement::{
        empty_harness_state, load_global_refinement_history, save_harness_state,
        REVIEW_AFTER_METADATA_KEY,
    };

    fn memory_entry(id: &str, title: &str, review_after: Option<&str>) -> HarnessEntry {
        let mut metadata = serde_json::Map::new();
        if let Some(stamp) = review_after {
            metadata.insert(REVIEW_AFTER_METADATA_KEY.to_string(), stamp.into());
        }
        HarnessEntry {
            id: id.to_string(),
            kind: RefinementKind::Memory,
            title: title.to_string(),
            content: format!("{title} content"),
            path: "general".to_string(),
            scope: Some(HarnessScope::Global),
            reference: serde_json::Map::new(),
            arguments: serde_json::Map::new(),
            metadata,
            source: "refine".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:00.000Z".to_string(),
            version: 1,
        }
    }

    fn seed_workspace(agent_dir: &Path, entries: &[HarnessEntry]) {
        let dir = get_workspace_harness_state_dir(agent_dir, "proj").unwrap();
        let mut state = empty_harness_state();
        for entry in entries {
            state
                .entries
                .get_mut(&entry.kind)
                .unwrap()
                .insert(entry.id.clone(), entry.clone());
        }
        save_harness_state(&dir, &state).unwrap();
    }

    fn memory_ids(dir: &Path) -> Vec<String> {
        load_harness_state(dir, HarnessScope::Global).entries[&RefinementKind::Memory]
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn promote_moves_the_entry_to_global_and_logs_both_stores() {
        let agent_dir = tempfile::tempdir().unwrap();
        seed_workspace(
            agent_dir.path(),
            &[
                memory_entry("keep", "Keep", None),
                memory_entry("lesson", "Lesson", None),
            ],
        );
        let promotion = promote_workspace_entry(agent_dir.path(), "proj", "lesson").unwrap();
        let global_dir = get_global_harness_state_dir(agent_dir.path());
        let workspace_dir = get_workspace_harness_state_dir(agent_dir.path(), "proj").unwrap();
        assert_eq!(memory_ids(&global_dir), ["lesson"]);
        assert_eq!(memory_ids(&workspace_dir), ["keep"]);
        let global_history = load_global_refinement_history(&global_dir);
        let workspace_history = load_global_refinement_history(&workspace_dir);
        assert_eq!(
            (
                global_history
                    .iter()
                    .map(|r| r.id.as_str())
                    .collect::<Vec<_>>(),
                workspace_history
                    .iter()
                    .map(|r| r.id.as_str())
                    .collect::<Vec<_>>(),
                workspace_history[0].workspace.as_deref(),
            ),
            (
                vec![promotion.global_refinement_id.as_str()],
                vec![promotion.workspace_refinement_id.as_str()],
                Some("proj"),
            )
        );
    }

    #[test]
    fn rolling_back_both_promotion_records_restores_the_workspace_entry() {
        let agent_dir = tempfile::tempdir().unwrap();
        seed_workspace(agent_dir.path(), &[memory_entry("lesson", "Lesson", None)]);
        promote_workspace_entry(agent_dir.path(), "proj", "lesson").unwrap();
        let global_dir = get_global_harness_state_dir(agent_dir.path());
        let workspace_dir = get_workspace_harness_state_dir(agent_dir.path(), "proj").unwrap();
        for dir in [&global_dir, &workspace_dir] {
            let recorded = load_global_refinement_history(dir).remove(0);
            try_update_harness_state(dir, HarnessScope::Global, |state| {
                apply_refinement_proposal(
                    state,
                    &rollback_proposal(&recorded),
                    ApplyOptions {
                        id: "rollback".to_string(),
                        rollback_of: Some(recorded.id.clone()),
                        scope: Some(HarnessScope::Global),
                        baseline_state: None,
                        factory_enabled: false,
                    },
                );
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(memory_ids(&global_dir), Vec::<String>::new());
        assert_eq!(memory_ids(&workspace_dir), ["lesson"]);
    }

    #[test]
    fn promote_rejects_an_unknown_entry_and_leaves_the_stores_untouched() {
        let agent_dir = tempfile::tempdir().unwrap();
        seed_workspace(agent_dir.path(), &[memory_entry("lesson", "Lesson", None)]);
        let error = promote_workspace_entry(agent_dir.path(), "proj", "nope").unwrap_err();
        assert_eq!(
            error.to_string(),
            "Workspace 'proj' has no harness entry 'nope'."
        );
        assert!(!get_global_harness_state_dir(agent_dir.path()).exists());
    }

    #[test]
    fn workspace_info_lists_entries_marks_review_due_and_reports_the_world_model() {
        let agent_dir = tempfile::tempdir().unwrap();
        seed_workspace(
            agent_dir.path(),
            &[
                memory_entry("fresh", "Fresh", Some("2099-01-01T00:00:00.000Z")),
                memory_entry("stale", "Stale", Some("2026-02-01T00:00:00.000Z")),
            ],
        );
        let root = agent_dir.path().join("workspaces").join("proj");
        std::fs::write(root.join("world_model.db"), [0u8; 16]).unwrap();
        let now = crate::cron::parse_iso_millis("2026-11-21T00:00:00.000Z").unwrap();
        assert_eq!(
            format_workspace_info(agent_dir.path(), "proj", now).unwrap(),
            "Workspace 'proj': 2 harness entries\n\
             - memory fresh: Fresh\n\
             - memory stale: Stale (review due)\n\
             World model: world_model.db present (16 bytes)"
        );
    }
}
