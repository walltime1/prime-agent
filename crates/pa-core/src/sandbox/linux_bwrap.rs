//! Linux backend: `bubblewrap` gives the kernel a fresh namespace with only
//! the policy's roots visible.

use std::path::{Path, PathBuf};

use pa_types::sandbox::SandboxNetworkMode;

use super::policy::SandboxPolicy;
use super::{SandboxCommand, SandboxError};

const BWRAP: &str = "bwrap";

pub(super) fn build_command(
    policy: &SandboxPolicy,
    inner_argv: &[String],
    _scratch_dir: &Path,
) -> Result<SandboxCommand, SandboxError> {
    if !bwrap_on_path() {
        return Err(SandboxError::BackendUnavailable {
            backend: "bubblewrap",
            hint: "install it (apt install bubblewrap) or set sandbox.mode to \"off\"",
        });
    }
    Ok(SandboxCommand {
        program: PathBuf::from(BWRAP),
        args: build_args(policy, inner_argv),
    })
}

fn bwrap_on_path() -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(BWRAP).is_file()))
}

fn build_args(policy: &SandboxPolicy, inner_argv: &[String]) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--unshare-all".into(),
        "--die-with-parent".into(),
        "--new-session".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--tmpfs".into(),
        "/tmp".into(),
    ];
    match policy.network() {
        SandboxNetworkMode::None => {}
        SandboxNetworkMode::Full => args.push("--share-net".into()),
    }
    for root in policy.ro_roots() {
        if root.exists() {
            push_mount(&mut args, "--ro-bind", root);
        }
    }
    for root in policy.rw_roots() {
        if root.exists() {
            push_mount(&mut args, "--bind", root);
        }
    }
    for file in policy.rw_files() {
        if file.exists() {
            push_mount(&mut args, "--bind", file);
        }
    }
    for path in policy.protected_subpaths() {
        if path.exists() {
            push_mount(&mut args, "--ro-bind", path);
        }
    }
    args.push("--chdir".into());
    args.push(policy.cwd().display().to_string());
    args.push("--".into());
    args.extend(inner_argv.iter().cloned());
    args
}

fn push_mount(args: &mut Vec<String>, flag: &str, path: &Path) {
    args.push(flag.to_string());
    args.push(path.display().to_string());
    args.push(path.display().to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SandboxPolicy {
        SandboxPolicy {
            mode: pa_types::sandbox::SandboxMode::WorkspaceWrite,
            network: SandboxNetworkMode::None,
            cwd: PathBuf::from("/w/ws"),
            rw_roots: vec![PathBuf::from("/w/ws")],
            rw_files: Vec::new(),
            ro_roots: vec![PathBuf::from("/usr")],
            protected_subpaths: Vec::new(),
            scrub_env: true,
            protect_vcs: true,
            home: PathBuf::from("/home/u"),
        }
    }

    #[test]
    fn the_args_unshare_every_namespace_and_bind_the_policy_roots() {
        let args = build_args(&policy(), &["/usr/bin/python3".to_string()]);
        assert!(args.contains(&"--unshare-all".to_string()));
        assert!(!args.contains(&"--share-net".to_string()));
        assert!(args.windows(3).any(|w| w == ["--ro-bind", "/usr", "/usr"]));
        assert!(args.windows(2).any(|w| w == ["--chdir", "/w/ws"]));
        assert_eq!(args.last().unwrap(), "/usr/bin/python3");
    }

    #[test]
    fn full_network_adds_share_net() {
        let mut p = policy();
        p.network = SandboxNetworkMode::Full;
        let args = build_args(&p, &["/usr/bin/python3".to_string()]);
        assert!(args.contains(&"--share-net".to_string()));
    }

    #[test]
    fn protected_subpaths_are_remounted_read_only() {
        let mut policy = policy();
        let dir = tempfile::tempdir().unwrap();
        let protected = dir.path().join("hooks");
        std::fs::create_dir(&protected).unwrap();
        policy.set_protected_subpaths_for_test(vec![protected.clone()]);
        let args = build_args(&policy, &["python3".to_string()]);
        let shown = protected.display().to_string();
        assert!(args
            .windows(3)
            .any(|w| w == ["--ro-bind", shown.as_str(), shown.as_str()]));
    }
}
