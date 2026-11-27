//! OS-level confinement for the Python kernel process.
//!
//! The boundary wraps the kernel interpreter itself, so Python `open()`,
//! `os.remove()`, `subprocess.run()`, and every `bash()` the kernel starts
//! inherit exactly one policy. macOS uses `sandbox-exec` with a generated
//! Seatbelt profile; Linux uses `bubblewrap`. Both backends are deny-by-default:
//! reads and writes outside the policy's roots fail, and network is denied
//! unless the session asks for it.

mod env;
#[cfg(target_os = "linux")]
mod linux_bwrap;
mod policy;
#[cfg(target_os = "macos")]
mod seatbelt;

use std::path::{Path, PathBuf};

pub use env::scrub_secrets;
pub use policy::{SandboxInputs, SandboxPolicy};

use pa_types::sandbox::{SandboxBackend, SandboxMode};

/// The kernel command a backend produced: program plus arguments. The caller
/// keeps its own stdio setup and only swaps the executable and arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// Why a sandbox could not be built. Every variant is a hard failure: a
/// session that asked for a boundary never runs unbounded because of it.
#[derive(Debug)]
pub enum SandboxError {
    /// The platform has no backend at all.
    UnsupportedPlatform,
    /// The backend binary is missing (Linux without `bwrap`).
    BackendUnavailable {
        backend: &'static str,
        hint: &'static str,
    },
    /// The backend refused to build a command (bad root, unreadable path).
    BackendFailed {
        backend: &'static str,
        reason: String,
    },
    /// The session workspace is not an absolute path.
    RelativeWorkspace(PathBuf),
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SandboxError::UnsupportedPlatform => {
                write!(
                    f,
                    "sandbox.mode is set but this platform has no sandbox backend; set sandbox.mode to \"off\""
                )
            }
            SandboxError::BackendUnavailable { backend, hint } => {
                write!(
                    f,
                    "{backend} is required for sandbox.mode but was not found; {hint}"
                )
            }
            SandboxError::BackendFailed { backend, reason } => {
                write!(f, "{backend} refused to build the sandbox: {reason}")
            }
            SandboxError::RelativeWorkspace(path) => {
                write!(
                    f,
                    "sandbox.mode needs an absolute workspace, got {}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for SandboxError {}

/// The backend this build uses, for telemetry and diagnostics.
#[must_use]
pub fn backend() -> SandboxBackend {
    #[cfg(target_os = "macos")]
    {
        SandboxBackend::Seatbelt
    }
    #[cfg(target_os = "linux")]
    {
        SandboxBackend::Bubblewrap
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        SandboxBackend::None
    }
}

/// Resolve the session's policy from its inputs. `Ok(None)` means the mode
/// asks for no boundary (`off` / `danger-full-access`).
///
/// # Errors
///
/// Error when the workspace is relative or a configured root is malformed.
pub fn resolve(inputs: &SandboxInputs) -> Result<Option<SandboxPolicy>, SandboxError> {
    if matches!(
        inputs.mode,
        SandboxMode::Off | SandboxMode::DangerFullAccess
    ) {
        return Ok(None);
    }
    SandboxPolicy::resolve(inputs).map(Some)
}

/// Wrap the kernel interpreter's argument vector (`python -P -s -m rlm.repl`)
/// so the spawned process and its whole tree live inside `policy`.
///
/// # Errors
///
/// Error when the platform backend is unavailable or cannot build its command.
pub fn wrap_kernel(
    policy: &SandboxPolicy,
    python: &Path,
    kernel_args: &[String],
    scratch_dir: &Path,
) -> Result<SandboxCommand, SandboxError> {
    let mut inner = Vec::with_capacity(kernel_args.len() + 1);
    inner.push(python.display().to_string());
    inner.extend(kernel_args.iter().cloned());
    #[cfg(target_os = "macos")]
    {
        let _ = scratch_dir;
        Ok(seatbelt::build_command(policy, &inner))
    }
    #[cfg(target_os = "linux")]
    {
        linux_bwrap::build_command(policy, &inner, scratch_dir)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (policy, inner, scratch_dir);
        Err(SandboxError::UnsupportedPlatform)
    }
}
