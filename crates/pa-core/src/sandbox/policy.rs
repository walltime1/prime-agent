//! One resolved policy for a session: which roots are readable, which are
//! writable, which stay read-only inside the workspace, and whether network
//! and secret environment variables are allowed.

use std::path::{Component, Path, PathBuf};

use pa_types::sandbox::{SandboxMode, SandboxNetworkMode};

use super::SandboxError;
use crate::settings::types::SandboxSettings;

/// Readable on every policy: the shell, the interpreter, and the dynamic
/// loader need them, and none holds user documents.
#[cfg(target_os = "linux")]
const SYSTEM_READ_ROOTS: [&str; 6] = ["/usr", "/bin", "/sbin", "/etc", "/lib", "/lib64"];

#[cfg(not(target_os = "linux"))]
const SYSTEM_READ_ROOTS: [&str; 9] = [
    "/usr",
    "/bin",
    "/sbin",
    "/etc",
    "/Library",
    "/System",
    "/private/var/db",
    "/private/var/select",
    "/opt/homebrew",
];

/// Read-only inside the workspace by default: VCS metadata and the
/// harness's own state must not be rewritten by the model.
const PROTECTED_SUBPATHS: [&str; 4] = [".git/hooks", ".git/config", ".prime", ".agents"];

/// The session facts a policy is resolved from. Paths are absolute.
#[derive(Debug, Clone)]
pub struct SandboxInputs {
    pub mode: SandboxMode,
    pub network: SandboxNetworkMode,
    pub settings: Option<SandboxSettings>,
    /// Session workspace (the kernel's cwd).
    pub cwd: PathBuf,
    /// The real user home directory.
    pub home: PathBuf,
    /// Interpreter running the kernel.
    pub python: PathBuf,
    /// Kernel venv root, when one is in use.
    pub venv: Option<PathBuf>,
    /// Directories holding installed Python skills.
    pub python_skills: Vec<PathBuf>,
    /// Kernel shell selected by `shellPath`, when one is.
    pub shell_path: Option<PathBuf>,
    /// Global harness store (`RLM_GLOBAL_HARNESS_STATE_DIR`).
    pub global_harness_dir: Option<PathBuf>,
    /// Workspace harness store (`RLM_WORKSPACE_HARNESS_STATE_DIR`).
    pub workspace_harness_dir: Option<PathBuf>,
    /// Per-session artifact directory holding the snapshot and stderr log.
    pub snapshot_dir: Option<PathBuf>,
    /// Optional orphan-process journal file (`PRIME_AGENT_INTERNAL_ORPHAN_PROCESS_JOURNAL`).
    pub orphan_journal: Option<PathBuf>,
}

/// A session's resolved boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    mode: SandboxMode,
    network: SandboxNetworkMode,
    /// Canonical workspace.
    cwd: PathBuf,
    /// Roots readable and writable.
    rw_roots: Vec<PathBuf>,
    /// Individual files readable and writable without exposing their parent
    /// directory (the orphan-process journal).
    rw_files: Vec<PathBuf>,
    /// Roots readable only.
    ro_roots: Vec<PathBuf>,
    /// Paths inside [`Self::cwd`] that stay read-only.
    protected_subpaths: Vec<PathBuf>,
    /// Remove known secret variables from the kernel environment.
    scrub_env: bool,
    /// The user's home directory: the one tree every mode keeps unreadable
    /// except for the session's own roots inside it.
    home: PathBuf,
}

impl SandboxPolicy {
    /// # Errors
    ///
    /// Error when the workspace is relative.
    pub(super) fn resolve(inputs: &SandboxInputs) -> Result<SandboxPolicy, SandboxError> {
        if !inputs.cwd.is_absolute() {
            return Err(SandboxError::RelativeWorkspace(inputs.cwd.clone()));
        }
        let cwd = canonical(&inputs.cwd);
        let home = canonical(&inputs.home);
        let settings = inputs.settings.clone().unwrap_or_default();
        let protect_vcs = settings.protect_vcs != Some(false);

        let extra_write = resolve_roots(settings.extra_write_roots.as_ref(), &inputs.home)?;
        let extra_read = resolve_roots(settings.extra_read_roots.as_ref(), &inputs.home)?;

        let mut ro_roots: Vec<PathBuf> = SYSTEM_READ_ROOTS
            .iter()
            .map(Path::new)
            .map(canonical)
            .collect();
        // The interpreter's own directory comes from its resolved path: a
        // venv `bin/python` is a shim whose real binary lives elsewhere.
        // Grant the binary's directory and its prefix (one level above `bin/`)
        // so stdlib and framework libs are readable, without ever widening to
        // `/` or `$HOME`.
        let real_python =
            std::fs::canonicalize(&inputs.python).unwrap_or_else(|_| inputs.python.clone());
        let python_bin = real_python.parent().map(Path::to_path_buf);
        let python_prefix = python_bin
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        let inherited: Vec<PathBuf> = python_bin
            .into_iter()
            .chain(python_prefix)
            .filter(|dir| !is_too_broad(dir, &home))
            .chain(inputs.venv.iter().cloned())
            .chain(inputs.python_skills.iter().cloned())
            .chain(
                inputs
                    .shell_path
                    .iter()
                    .filter_map(|shell| shell.parent().map(Path::to_path_buf)),
            )
            .chain(inputs.global_harness_dir.iter().cloned())
            .chain(inputs.workspace_harness_dir.iter().cloned())
            .chain(extra_read)
            .collect();
        for root in inherited {
            push_unique(&mut ro_roots, canonical(&root));
        }

        let mut writable = Vec::new();
        if inputs.mode == SandboxMode::WorkspaceWrite {
            push_unique(&mut writable, cwd.clone());
            for root in inputs.snapshot_dir.iter().cloned().chain(extra_write) {
                push_unique(&mut writable, canonical(&root));
            }
            // The kernel needs scratch space; prefer the private session dir
            // when there is one and fall back to the system temp directory.
            let scratch = inputs
                .snapshot_dir
                .as_ref()
                .map_or_else(std::env::temp_dir, |dir| dir.join("sandbox-tmp"));
            let scratch = canonical(&scratch);
            if scratch.is_absolute() {
                push_unique(&mut writable, scratch);
            }
        }

        let mut rw_files = Vec::new();
        if let Some(journal) = inputs
            .orphan_journal
            .as_ref()
            .filter(|journal| journal.is_absolute())
        {
            push_unique(&mut rw_files, canonical(journal));
        }

        let protected_subpaths = if protect_vcs {
            PROTECTED_SUBPATHS
                .iter()
                .map(|sub| canonical(&cwd.join(sub)))
                .collect()
        } else {
            Vec::new()
        };

        Ok(SandboxPolicy {
            mode: inputs.mode,
            network: inputs.network,
            cwd,
            rw_roots: writable,
            rw_files,
            ro_roots,
            protected_subpaths,
            scrub_env: settings.scrub_env != Some(false),
            home,
        })
    }

    #[must_use]
    pub fn mode(&self) -> SandboxMode {
        self.mode
    }

    #[must_use]
    pub fn network(&self) -> SandboxNetworkMode {
        self.network
    }

    #[must_use]
    pub fn scrub_env(&self) -> bool {
        self.scrub_env
    }

    /// The per-session scratch directory (`sandbox-tmp`) when configured, else
    /// the system temp directory.
    #[must_use]
    pub fn scratch_dir(&self) -> PathBuf {
        self.rw_roots
            .iter()
            .find(|root| root.ends_with("sandbox-tmp"))
            .cloned()
            .unwrap_or_else(std::env::temp_dir)
    }

    pub(super) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(super) fn home(&self) -> &Path {
        &self.home
    }

    pub(super) fn rw_roots(&self) -> &[PathBuf] {
        &self.rw_roots
    }

    pub(super) fn rw_files(&self) -> &[PathBuf] {
        &self.rw_files
    }

    pub(super) fn ro_roots(&self) -> &[PathBuf] {
        &self.ro_roots
    }

    pub(super) fn protected_subpaths(&self) -> &[PathBuf] {
        &self.protected_subpaths
    }

    #[cfg(test)]
    pub(super) fn set_network_for_test(&mut self, network: SandboxNetworkMode) {
        self.network = network;
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(super) fn set_protected_subpaths_for_test(&mut self, subpaths: Vec<PathBuf>) {
        self.protected_subpaths = subpaths;
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(super) fn fixture_for_test(
        cwd: PathBuf,
        rw_roots: Vec<PathBuf>,
        rw_files: Vec<PathBuf>,
        read_only_roots: Vec<PathBuf>,
        protected_subpaths: Vec<PathBuf>,
        home: PathBuf,
    ) -> Self {
        Self {
            mode: SandboxMode::WorkspaceWrite,
            network: SandboxNetworkMode::None,
            cwd,
            rw_roots,
            rw_files,
            ro_roots: read_only_roots,
            protected_subpaths,
            scrub_env: true,
            home,
        }
    }

    /// Directories whose metadata the sandbox must expose so path traversal
    /// works: every ancestor of every allowed root, up to `/`. Readable
    /// metadata on `$HOME` does not expose sibling file names' content.
    #[must_use]
    pub fn ancestor_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let roots = self
            .rw_roots
            .iter()
            .chain(self.rw_files.iter())
            .chain(self.ro_roots.iter())
            .chain(std::iter::once(&self.cwd));
        for root in roots {
            let mut current = root.parent();
            while let Some(dir) = current {
                if dir.as_os_str().is_empty() || dir.parent().is_none() {
                    break;
                }
                push_unique(&mut dirs, dir.to_path_buf());
                current = dir.parent();
            }
        }
        dirs
    }
}

/// Reject `/` and `$HOME` (or any ancestor of `$HOME`) when inferring a
/// readable prefix from a binary path.
fn is_too_broad(dir: &Path, home: &Path) -> bool {
    let dir = canonical(dir);
    dir.parent().is_none() || home.starts_with(&dir)
}

/// Canonicalize when possible; otherwise normalize lexically so a
/// not-yet-created root (`snapshot_dir`) still yields an absolute path.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| normalize_lexically(path))
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn push_unique(roots: &mut Vec<PathBuf>, root: PathBuf) {
    if !roots.contains(&root) {
        roots.push(root);
    }
}

fn resolve_roots(roots: Option<&Vec<String>>, home: &Path) -> Result<Vec<PathBuf>, SandboxError> {
    roots
        .into_iter()
        .flatten()
        .map(|root| {
            let trimmed = root.trim();
            if trimmed.is_empty() {
                return Err(SandboxError::BackendFailed {
                    backend: "the sandbox policy",
                    reason: "an empty sandbox root in settings".to_string(),
                });
            }
            let path = match trimmed.strip_prefix("~/") {
                Some(rest) => home.join(rest),
                None if trimmed == "~" => home.to_path_buf(),
                None => PathBuf::from(trimmed),
            };
            if !path.is_absolute() {
                return Err(SandboxError::BackendFailed {
                    backend: "the sandbox policy",
                    reason: format!("sandbox root {trimmed} is not absolute"),
                });
            }
            Ok(canonical(&path))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(cwd: PathBuf) -> SandboxInputs {
        SandboxInputs {
            mode: SandboxMode::WorkspaceWrite,
            network: SandboxNetworkMode::None,
            settings: None,
            cwd,
            home: PathBuf::from("/home/u"),
            python: PathBuf::from("/usr/bin/python3"),
            venv: None,
            python_skills: Vec::new(),
            shell_path: Some(PathBuf::from("/bin/bash")),
            global_harness_dir: None,
            workspace_harness_dir: None,
            snapshot_dir: None,
            orphan_journal: None,
        }
    }

    #[test]
    fn workspace_write_allows_the_workspace_and_protects_vcs() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("ws");
        std::fs::create_dir_all(cwd.join(".git/hooks")).unwrap();
        let policy = SandboxPolicy::resolve(&inputs(cwd.clone())).unwrap();
        let canon = cwd.canonicalize().unwrap();
        assert_eq!(policy.rw_roots[0], canon);
        assert_eq!(
            policy.protected_subpaths,
            vec![
                canon.join(".git/hooks"),
                canon.join(".git/config"),
                canon.join(".prime"),
                canon.join(".agents"),
            ]
        );
        assert!(!policy.ro_roots.contains(&PathBuf::from("/")));
        assert!(policy.scrub_env);
        assert_eq!(policy.network, SandboxNetworkMode::None);
    }

    #[test]
    fn protect_vcs_false_drops_the_carve_outs() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("ws");
        std::fs::create_dir_all(cwd.join(".git/hooks")).unwrap();
        let mut cfg = inputs(cwd);
        cfg.settings = Some(SandboxSettings {
            protect_vcs: Some(false),
            ..SandboxSettings::default()
        });
        let policy = SandboxPolicy::resolve(&cfg).unwrap();
        assert!(policy.protected_subpaths.is_empty());
    }

    #[test]
    fn read_only_grants_no_writable_roots() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = inputs(dir.path().to_path_buf());
        cfg.mode = SandboxMode::ReadOnly;
        let policy = SandboxPolicy::resolve(&cfg).unwrap();
        assert!(policy.rw_roots.is_empty());
    }

    #[test]
    fn a_relative_workspace_is_refused() {
        let error = SandboxPolicy::resolve(&inputs(PathBuf::from("relative/ws"))).unwrap_err();
        assert!(format!("{error}").contains("absolute workspace"));
    }

    #[test]
    fn extra_roots_expand_against_the_real_home() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = inputs(dir.path().to_path_buf());
        cfg.settings = Some(SandboxSettings {
            extra_write_roots: Some(vec!["~/scratch".into()]),
            ..SandboxSettings::default()
        });
        let policy = SandboxPolicy::resolve(&cfg).unwrap();
        assert!(policy.rw_roots.contains(&PathBuf::from("/home/u/scratch")));
    }

    #[test]
    fn ancestors_stop_at_the_filesystem_root() {
        let policy = SandboxPolicy {
            mode: SandboxMode::WorkspaceWrite,
            network: SandboxNetworkMode::None,
            cwd: PathBuf::from("/a/b/c"),
            rw_roots: vec![PathBuf::from("/a/b/c")],
            rw_files: vec![PathBuf::from("/var/run/journal.jsonl")],
            ro_roots: Vec::new(),
            protected_subpaths: Vec::new(),
            scrub_env: true,
            home: PathBuf::from("/home/u"),
        };
        let ancestors = policy.ancestor_dirs();
        assert!(ancestors.contains(&PathBuf::from("/a/b")));
        assert!(ancestors.contains(&PathBuf::from("/a")));
        assert!(ancestors.contains(&PathBuf::from("/var/run")));
        assert!(!ancestors.contains(&PathBuf::from("/")));
    }
}
