//! macOS backend: a generated Seatbelt profile passed inline to
//! `/usr/bin/sandbox-exec` (never a temp file, so nothing leaks).
//!
//! Roots travel as `-D name=value` parameters, so a path containing quotes,
//! spaces, or newlines cannot forge profile syntax. Seatbelt evaluates rules
//! last-match-wins, so the generated denials precede the re-allows they must
//! override.

use std::fmt::Write as _;
use std::path::PathBuf;

use pa_types::sandbox::SandboxNetworkMode;

use super::policy::SandboxPolicy;
use super::SandboxCommand;

/// Only this binary: a PATH-supplied `sandbox-exec` would be attacker input.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

const BASE_POLICY: &str = include_str!("seatbelt_base_policy.sbpl");

pub(super) fn build_command(policy: &SandboxPolicy, inner_argv: &[String]) -> SandboxCommand {
    let ancestors = policy.ancestor_dirs();
    let protected_parents = protected_parent_dirs(policy);
    let profile = build_profile(policy, ancestors.len(), protected_parents.len());
    let mut args = vec!["-p".to_string(), profile];
    let mut param = |name: String, value: String| {
        args.push("-D".to_string());
        args.push(format!("{name}={value}"));
    };
    param("HOME".to_string(), policy.home().display().to_string());
    param("CWD".to_string(), policy.cwd().display().to_string());
    for (index, root) in policy.ro_roots().iter().enumerate() {
        param(format!("RO_{index}"), root.display().to_string());
    }
    for (index, root) in policy.rw_roots().iter().enumerate() {
        param(format!("RW_{index}"), root.display().to_string());
    }
    for (index, file) in policy.rw_files().iter().enumerate() {
        param(format!("RW_FILE_{index}"), file.display().to_string());
    }
    for (index, dir) in ancestors.iter().enumerate() {
        param(format!("ANC_{index}"), dir.display().to_string());
    }
    for (index, path) in policy.protected_subpaths().iter().enumerate() {
        param(format!("PROT_{index}"), path.display().to_string());
    }
    for (index, dir) in protected_parents.iter().enumerate() {
        param(format!("PROT_PARENT_{index}"), dir.display().to_string());
    }
    args.extend(inner_argv.iter().cloned());
    SandboxCommand {
        program: PathBuf::from(SANDBOX_EXEC),
        args,
    }
}

/// Parent directories strictly inside `policy.cwd` that hold a protected
/// subpath (e.g. `<cwd>/.git` for `<cwd>/.git/hooks`), so the sandbox cannot
/// rename or unlink the parent to replace the protected subtree.
fn protected_parent_dirs(policy: &SandboxPolicy) -> Vec<PathBuf> {
    let mut parents = Vec::new();
    for path in policy.protected_subpaths() {
        let mut current = path.parent();
        while let Some(dir) = current {
            if dir == policy.cwd() || !dir.starts_with(policy.cwd()) {
                break;
            }
            let dir_buf = dir.to_path_buf();
            if !parents.contains(&dir_buf) {
                parents.push(dir_buf);
            }
            current = dir.parent();
        }
    }
    parents
}

fn build_profile(
    policy: &SandboxPolicy,
    ancestor_count: usize,
    protected_parent_count: usize,
) -> String {
    let mut profile = BASE_POLICY.to_string();
    // Every generated rule names its root through a `-D` parameter, so no
    // path text is ever parsed as SBPL.
    // $HOME is denied first so only the session's explicit roots inside it
    // (e.g. kernel venv, uv interpreter, workspace) are re-allowed below
    // (Seatbelt is last-match-wins).
    let _ = writeln!(
        profile,
        "(deny file-read* file-write* (subpath (param \"HOME\")))"
    );
    for index in 0..policy.ro_roots().len() {
        let _ = writeln!(
            profile,
            "(allow file-read* (subpath (param \"RO_{index}\")))"
        );
    }
    for index in 0..policy.rw_roots().len() {
        let _ = writeln!(
            profile,
            "(allow file-read* file-write* (subpath (param \"RW_{index}\")))"
        );
    }
    for index in 0..policy.rw_files().len() {
        let _ = writeln!(
            profile,
            "(allow file-read* file-write* (literal (param \"RW_FILE_{index}\")))"
        );
    }
    // Literal metadata on ancestor directories only, so traversal through
    // $HOME works without exposing sibling entries under $HOME.
    for index in 0..ancestor_count {
        let _ = writeln!(
            profile,
            "(allow file-read-metadata (literal (param \"ANC_{index}\")))"
        );
    }
    // Never unlink the workspace root itself.
    let _ = writeln!(
        profile,
        "(deny file-write-unlink (literal (param \"CWD\")))"
    );
    // Protected paths go back to read-only, and neither they nor their
    // intermediate parents inside cwd can be unlinked out from under them.
    for index in 0..policy.protected_subpaths().len() {
        let _ = writeln!(
            profile,
            "(deny file-write* (subpath (param \"PROT_{index}\")))"
        );
        let _ = writeln!(
            profile,
            "(deny file-write-unlink (literal (param \"PROT_{index}\")))"
        );
    }
    for index in 0..protected_parent_count {
        let _ = writeln!(
            profile,
            "(deny file-write-unlink (literal (param \"PROT_PARENT_{index}\")))"
        );
    }
    match policy.network() {
        SandboxNetworkMode::None => profile.push_str("(deny network*)\n"),
        SandboxNetworkMode::Full => profile.push_str("(allow network*)\n"),
    }
    profile
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SandboxPolicy {
        SandboxPolicy::fixture_for_test(
            PathBuf::from("/w/ws"),
            vec![PathBuf::from("/w/ws")],
            vec![PathBuf::from("/var/run/orphan.jsonl")],
            vec![PathBuf::from("/usr")],
            vec![PathBuf::from("/w/ws/.git/hooks")],
            PathBuf::from("/home/u"),
        )
    }

    #[test]
    fn the_profile_denies_home_and_network_and_reallows_the_roots() {
        let p = policy();
        let profile = build_profile(&p, p.ancestor_dirs().len(), protected_parent_dirs(&p).len());
        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
        let deny_at = profile
            .find("(deny file-read* file-write* (subpath (param \"HOME\")))")
            .unwrap();
        let allow_at = profile
            .find("(allow file-read* file-write* (subpath (param \"RW_0\")))")
            .unwrap();
        assert!(
            allow_at > deny_at,
            "allowed roots must override the home deny (last-match-wins)"
        );
        assert!(profile.contains("(allow file-read-metadata (literal (param \"ANC_0\")))"));
        assert!(profile.contains("(allow file-read* file-write* (literal (param \"RW_FILE_0\")))"));
        assert!(profile.contains("(deny file-write-unlink (literal (param \"CWD\")))"));
        assert!(profile.contains("(deny file-write-unlink (literal (param \"PROT_PARENT_0\")))"));
        assert!(profile.contains("(deny network*)"));
    }

    #[test]
    fn roots_travel_as_parameters_not_literals() {
        let command = build_command(&policy(), &["/usr/bin/python3".to_string()]);
        assert_eq!(command.program, PathBuf::from("/usr/bin/sandbox-exec"));
        assert_eq!(command.args[0], "-p");
        assert!(command.args.iter().any(|arg| arg == "HOME=/home/u"));
        assert!(command.args.iter().any(|arg| arg == "CWD=/w/ws"));
        assert!(command.args.iter().any(|arg| arg == "RW_0=/w/ws"));
        assert!(command
            .args
            .iter()
            .any(|arg| arg == "RW_FILE_0=/var/run/orphan.jsonl"));
        assert!(command.args.iter().any(|arg| arg == "RO_0=/usr"));
        assert!(command
            .args
            .iter()
            .any(|arg| arg == "PROT_PARENT_0=/w/ws/.git"));
        assert_eq!(command.args.last().unwrap(), "/usr/bin/python3");
        assert!(
            !command.args[1].contains("/w/ws"),
            "no root path may be interpolated into the profile text"
        );
    }

    #[test]
    fn a_full_network_policy_allows_network() {
        let mut policy = policy();
        policy.set_network_for_test(SandboxNetworkMode::Full);
        let profile = build_profile(&policy, 0, 0);
        assert!(profile.contains("(allow network*)"));
        assert!(!profile.contains("(deny network*)"));
    }
}
