//! Workspace selection and directory helpers shared across CLI, TUI, core, and daemon.

use std::path::{Path, PathBuf};

pub const WORKSPACES_DIR_NAME: &str = "workspaces";
pub const ACTIVE_WORKSPACE_FILE_NAME: &str = "active_workspace";
pub const HARNESS_STATE_DIR_NAME: &str = "harness";
pub const SESSION_WORKSPACE_FILE_NAME: &str = "workspace";

/// Validate a workspace name so it stays a single directory segment under `workspaces/`.
///
/// # Errors
///
/// Returns an error if `name` is empty, `.` or `..`, or contains path separators or control characters.
pub fn validate_workspace_name(name: &str) -> std::io::Result<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.chars().any(char::is_control)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid workspace name: {name:?}"),
        ));
    }
    Ok(trimmed)
}

/// Harness state directory for a named workspace (`<agent_dir>/workspaces/<name>/harness`).
///
/// # Errors
///
/// Returns an error if `workspace` fails [`validate_workspace_name`].
pub fn workspace_harness_state_dir(agent_dir: &Path, workspace: &str) -> std::io::Result<PathBuf> {
    let valid = validate_workspace_name(workspace)?;
    Ok(agent_dir
        .join(WORKSPACES_DIR_NAME)
        .join(valid)
        .join(HARNESS_STATE_DIR_NAME))
}

/// Read the active workspace name from `<agent_dir>/active_workspace`, if any.
#[must_use]
pub fn read_active_workspace(agent_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(agent_dir.join(ACTIVE_WORKSPACE_FILE_NAME)).ok()?;
    let trimmed = raw.trim();
    validate_workspace_name(trimmed).ok().map(str::to_string)
}

/// Persist (or clear, when `None`) the active workspace in `<agent_dir>/active_workspace`.
///
/// # Errors
///
/// Returns an error if `workspace` is invalid or the filesystem update fails.
pub fn write_active_workspace(agent_dir: &Path, workspace: Option<&str>) -> std::io::Result<()> {
    let marker = agent_dir.join(ACTIVE_WORKSPACE_FILE_NAME);
    match workspace {
        Some(name) => {
            let valid = validate_workspace_name(name)?;
            let ws_harness_dir = workspace_harness_state_dir(agent_dir, valid)?;
            std::fs::create_dir_all(&ws_harness_dir)?;
            std::fs::write(&marker, format!("{valid}\n"))?;
        }
        None => {
            if let Err(err) = std::fs::remove_file(&marker) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    return Err(err);
                }
            }
        }
    }
    Ok(())
}

/// List existing workspace names under `<agent_dir>/workspaces/`.
#[must_use]
pub fn list_workspaces(agent_dir: &Path) -> Vec<String> {
    let root = agent_dir.join(WORKSPACES_DIR_NAME);
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|ft| ft.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| validate_workspace_name(name).is_ok())
        .collect();
    names.sort();
    names
}

/// Session artifact directory implied by a conversation-log path
/// (`dirname(dirname(file))/session-artifacts/<id>`).
#[must_use]
pub fn session_artifact_dir_for_log(log: &Path) -> Option<PathBuf> {
    let id = log.file_stem()?.to_string_lossy().to_string();
    let artifacts_root = log.parent()?.parent()?.join("session-artifacts");
    Some(artifacts_root.join(id))
}

/// Read the workspace recorded for a session artifact directory, if any.
#[must_use]
pub fn read_session_workspace(session_artifact_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(session_artifact_dir.join(SESSION_WORKSPACE_FILE_NAME)).ok()?;
    let trimmed = raw.trim();
    validate_workspace_name(trimmed).ok().map(str::to_string)
}

/// Persist (or clear, when `None`) the workspace recorded for a session artifact directory.
///
/// # Errors
///
/// Returns an error if `workspace` is invalid or the filesystem update fails.
pub fn write_session_workspace(
    session_artifact_dir: &Path,
    workspace: Option<&str>,
) -> std::io::Result<()> {
    let marker = session_artifact_dir.join(SESSION_WORKSPACE_FILE_NAME);
    match workspace {
        Some(name) => {
            let valid = validate_workspace_name(name)?;
            std::fs::create_dir_all(session_artifact_dir)?;
            std::fs::write(&marker, format!("{valid}\n"))?;
        }
        None => {
            if let Err(err) = std::fs::remove_file(&marker) {
                if err.kind() != std::io::ErrorKind::NotFound {
                    return Err(err);
                }
            }
        }
    }
    Ok(())
}

/// Read the workspace recorded for a session given its conversation-log path.
#[must_use]
pub fn read_session_workspace_for_log(log: &Path) -> Option<String> {
    let artifact_dir = session_artifact_dir_for_log(log)?;
    read_session_workspace(&artifact_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_workspace_round_trip_and_clear() {
        let dir = std::env::temp_dir().join(format!("pa-ws-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(read_active_workspace(&dir), None);
        assert!(list_workspaces(&dir).is_empty());

        write_active_workspace(&dir, Some("alpha")).unwrap();
        assert_eq!(read_active_workspace(&dir).as_deref(), Some("alpha"));
        assert!(workspace_harness_state_dir(&dir, "alpha")
            .unwrap()
            .is_dir());
        assert_eq!(list_workspaces(&dir), vec!["alpha"]);

        write_active_workspace(&dir, None).unwrap();
        assert_eq!(read_active_workspace(&dir), None);
        assert!(validate_workspace_name("../bad").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
