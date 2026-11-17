//! Workspace selection and directory helpers shared across CLI, TUI, core, and daemon.

use std::path::{Path, PathBuf};

pub const WORKSPACES_DIR_NAME: &str = "workspaces";
pub const ACTIVE_WORKSPACE_FILE_NAME: &str = "active_workspace";
pub const HARNESS_STATE_DIR_NAME: &str = "harness";
pub const SESSION_WORKSPACE_FILE_NAME: &str = "workspace";

const MAX_WORKSPACE_NAME_LEN: usize = 64;
/// Command keywords that select global state, and Windows device names a workspace cannot take.
const RESERVED_WORKSPACE_NAMES: &[&str] = &[
    "off", "clear", "global", "none", "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4",
    "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7",
    "lpt8", "lpt9",
];

/// Trim and lowercase a workspace name, then check that it is one directory segment under
/// `workspaces/`: `^[a-z0-9][a-z0-9_-]{0,63}$`, and not reserved. The returned normalized name
/// is the directory name.
///
/// # Errors
///
/// Returns `InvalidInput` if the normalized name breaks the pattern or is reserved.
pub fn validate_workspace_name(name: &str) -> std::io::Result<String> {
    let normalized = name.trim().to_ascii_lowercase();
    let is_letter_or_digit = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let mut chars = normalized.chars();
    let well_formed = normalized.len() <= MAX_WORKSPACE_NAME_LEN
        && chars.next().is_some_and(is_letter_or_digit)
        && chars.all(|c| is_letter_or_digit(c) || matches!(c, '-' | '_'));
    if !well_formed {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "invalid workspace name {name:?}: use 1-{MAX_WORKSPACE_NAME_LEN} lowercase letters, digits, '-' or '_', starting with a letter or digit"
            ),
        ));
    }
    if RESERVED_WORKSPACE_NAMES.contains(&normalized.as_str()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{normalized:?} is reserved"),
        ));
    }
    Ok(normalized)
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
    validate_workspace_name(trimmed).ok()
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
            let ws_harness_dir = workspace_harness_state_dir(agent_dir, &valid)?;
            std::fs::create_dir_all(&ws_harness_dir)?;
            write_atomically(&marker, &format!("{valid}\n"))?;
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

/// Replace `path` via a sibling temp file and rename, so a concurrent reader
/// sees the old or the new contents, never a torn write (an empty session
/// record means "explicitly global", so a torn read would switch scope).
fn write_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    let file_name = path.file_name().map_or_else(
        || std::ffi::OsString::from("workspace"),
        std::ffi::OsStr::to_os_string,
    );
    let mut temp_name = file_name;
    temp_name.push(format!(".tmp-{}", std::process::id()));
    let temp_path = path.with_file_name(temp_name);
    std::fs::write(&temp_path, contents)?;
    std::fs::rename(&temp_path, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp_path);
    })
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
        .filter(|name| validate_workspace_name(name).is_ok_and(|valid| valid == *name))
        .collect();
    names.sort();
    names
}

/// The directory holding one workspace's state (`<agent_dir>/workspaces/<name>`).
fn workspace_root_dir(agent_dir: &Path, name: &str) -> PathBuf {
    agent_dir.join(WORKSPACES_DIR_NAME).join(name)
}

/// Remove a workspace directory and everything in it. The default workspace cannot be deleted:
/// clear or switch the default first. Per-session records that name the workspace are not
/// rewritten, so such a session gets a fresh empty store under the old name the next time it
/// loads.
///
/// # Errors
///
/// Returns `NotFound` if the workspace does not exist, `InvalidInput` if `name` is invalid or is
/// the default workspace, or the filesystem error from removing the directory.
pub fn delete_workspace(agent_dir: &Path, name: &str) -> std::io::Result<()> {
    let valid = validate_workspace_name(name)?;
    let dir = workspace_root_dir(agent_dir, &valid);
    if !dir.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("workspace {valid:?} does not exist"),
        ));
    }
    if read_active_workspace(agent_dir).as_deref() == Some(valid.as_str()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "workspace {valid:?} is the default for new sessions: clear or switch the default first"
            ),
        ));
    }
    std::fs::remove_dir_all(dir)
}

/// Rename a workspace directory. When `from` is the default workspace, the default moves to `to`.
/// Per-session records that name `from` are not rewritten, so such a session gets a fresh empty
/// store under the old name the next time it loads.
///
/// # Errors
///
/// Returns `InvalidInput` if either name is invalid, `NotFound` if `from` does not exist,
/// `AlreadyExists` if `to` does, or the filesystem error from renaming the directory.
pub fn rename_workspace(agent_dir: &Path, from: &str, to: &str) -> std::io::Result<()> {
    let from = validate_workspace_name(from)?;
    let to = validate_workspace_name(to)?;
    let from_dir = workspace_root_dir(agent_dir, &from);
    if !from_dir.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("workspace {from:?} does not exist"),
        ));
    }
    let to_dir = workspace_root_dir(agent_dir, &to);
    if to_dir.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("workspace {to:?} already exists"),
        ));
    }
    std::fs::rename(&from_dir, &to_dir)?;
    if read_active_workspace(agent_dir).as_deref() == Some(from.as_str()) {
        write_active_workspace(agent_dir, Some(to.as_str()))?;
    }
    Ok(())
}

/// Session artifact directory implied by a conversation-log path
/// (`dirname(dirname(file))/session-artifacts/<id>`).
#[must_use]
pub fn session_artifact_dir_for_log(log: &Path) -> Option<PathBuf> {
    let id = log.file_stem()?.to_string_lossy().to_string();
    let artifacts_root = log.parent()?.parent()?.join("session-artifacts");
    Some(artifacts_root.join(id))
}

/// The workspace a session recorded: `None` when it never recorded one,
/// `Some(None)` when it explicitly runs in global state (an empty record).
#[must_use]
pub fn read_session_workspace_record(session_artifact_dir: &Path) -> Option<Option<String>> {
    let raw =
        std::fs::read_to_string(session_artifact_dir.join(SESSION_WORKSPACE_FILE_NAME)).ok()?;
    Some(validate_workspace_name(raw.trim()).ok())
}

/// Read the workspace recorded for a session artifact directory, if any.
#[must_use]
pub fn read_session_workspace(session_artifact_dir: &Path) -> Option<String> {
    read_session_workspace_record(session_artifact_dir).flatten()
}

/// Record the workspace a session runs in; `None` records global explicitly
/// (an empty record), so the session keeps global state across reloads
/// instead of falling back to the agent-dir default.
///
/// # Errors
///
/// Returns an error if `workspace` is invalid or the filesystem update fails.
pub fn write_session_workspace(
    session_artifact_dir: &Path,
    workspace: Option<&str>,
) -> std::io::Result<()> {
    let contents = match workspace {
        Some(name) => format!("{}\n", validate_workspace_name(name)?),
        None => String::new(),
    };
    std::fs::create_dir_all(session_artifact_dir)?;
    write_atomically(
        &session_artifact_dir.join(SESSION_WORKSPACE_FILE_NAME),
        &contents,
    )
}

/// The workspace a session runs in. Its own record wins (reloads and
/// resumes keep it), then the parent session's record (subagents and forks
/// inherit it), then the agent-dir default that seeds new sessions.
#[must_use]
pub fn resolve_session_workspace(
    agent_dir: &Path,
    session_artifact_dir: Option<&Path>,
    parent_session_log: Option<&Path>,
) -> Option<String> {
    if let Some(record) = session_artifact_dir.and_then(read_session_workspace_record) {
        return record;
    }
    let parent_artifact_dir = parent_session_log.and_then(session_artifact_dir_for_log);
    if let Some(record) = parent_artifact_dir
        .as_deref()
        .and_then(read_session_workspace_record)
    {
        return record;
    }
    read_active_workspace(agent_dir)
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
        assert!(workspace_harness_state_dir(&dir, "alpha").unwrap().is_dir());
        assert_eq!(list_workspaces(&dir), vec!["alpha"]);

        write_active_workspace(&dir, None).unwrap();
        assert_eq!(read_active_workspace(&dir), None);
        assert!(validate_workspace_name("../bad").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_workspace_resolves_own_then_parent_then_default() {
        let dir = std::env::temp_dir().join(format!("pa-ws-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let agent_dir = dir.join("agent");
        let parent_log = dir.join("sessions").join("parent.jsonl");
        let parent_artifacts = session_artifact_dir_for_log(&parent_log).unwrap();
        let own_artifacts = dir.join("session-artifacts").join("child");
        write_active_workspace(&agent_dir, Some("default-ws")).unwrap();

        let resolved = |own: Option<&Path>, parent: Option<&Path>| {
            resolve_session_workspace(&agent_dir, own, parent)
        };
        assert_eq!(resolved(None, None).as_deref(), Some("default-ws"));
        assert_eq!(
            resolved(Some(&own_artifacts), Some(&parent_log)).as_deref(),
            Some("default-ws"),
            "no records yet: the default seeds the session"
        );
        write_session_workspace(&parent_artifacts, Some("parent-ws")).unwrap();
        assert_eq!(
            resolved(Some(&own_artifacts), Some(&parent_log)).as_deref(),
            Some("parent-ws"),
            "a child inherits its parent's workspace"
        );
        write_session_workspace(&own_artifacts, None).unwrap();
        assert_eq!(
            resolved(Some(&own_artifacts), Some(&parent_log)),
            None,
            "an explicit global record wins over the parent and the default"
        );
        write_session_workspace(&own_artifacts, Some("own-ws")).unwrap();
        assert_eq!(
            resolved(Some(&own_artifacts), Some(&parent_log)).as_deref(),
            Some("own-ws")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_name_table_normalizes_and_rejects() {
        for (input, expected) in [
            ("  Alpha  ", "alpha"),
            ("MIXED_case-1", "mixed_case-1"),
            ("9lives", "9lives"),
        ] {
            assert_eq!(
                validate_workspace_name(input).unwrap(),
                expected,
                "input {input:?}"
            );
        }
        let longest = "a".repeat(MAX_WORKSPACE_NAME_LEN);
        assert_eq!(validate_workspace_name(&longest).unwrap(), longest);
        assert!(validate_workspace_name(&format!("{longest}a")).is_err());
        for rejected in [
            "", "../x", "a/b", "a\\b", "a.b", "trail.", "Foo Bar", "a\tb", "-lead", "_lead", "off",
            "Global", "NUL", "com1", "lpt9",
        ] {
            assert!(
                validate_workspace_name(rejected).is_err(),
                "input {rejected:?}"
            );
        }
    }

    #[test]
    fn delete_workspace_refuses_the_default_and_removes_the_rest() {
        let dir = std::env::temp_dir().join(format!("pa-ws-delete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_active_workspace(&dir, Some("alpha")).unwrap();
        write_active_workspace(&dir, Some("beta")).unwrap();

        assert_eq!(
            delete_workspace(&dir, "beta").unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        delete_workspace(&dir, "alpha").unwrap();
        assert_eq!(list_workspaces(&dir), vec!["beta"]);
        assert_eq!(
            delete_workspace(&dir, "alpha").unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );

        write_active_workspace(&dir, None).unwrap();
        delete_workspace(&dir, "beta").unwrap();
        assert!(list_workspaces(&dir).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_workspace_moves_the_directory_and_follows_the_default() {
        let dir = std::env::temp_dir().join(format!("pa-ws-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_active_workspace(&dir, Some("alpha")).unwrap();
        write_active_workspace(&dir, Some("gamma")).unwrap();

        rename_workspace(&dir, "alpha", "Beta").unwrap();
        assert_eq!(list_workspaces(&dir), vec!["beta", "gamma"]);
        assert_eq!(
            read_active_workspace(&dir).as_deref(),
            Some("gamma"),
            "renaming a non-default workspace keeps the default"
        );

        rename_workspace(&dir, "gamma", "delta").unwrap();
        assert_eq!(read_active_workspace(&dir).as_deref(), Some("delta"));

        assert_eq!(
            rename_workspace(&dir, "beta", "delta").unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            rename_workspace(&dir, "missing", "epsilon")
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert_eq!(list_workspaces(&dir), vec!["beta", "delta"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
