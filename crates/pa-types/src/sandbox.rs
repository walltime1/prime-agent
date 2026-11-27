//! OS-level sandbox boundary types: the effective mode/backend/network vocabulary and
//! the violation event emitted when a boundary denies an operation.
//!
//! These are wire/domain types only: no commands, paths, or enforcement logic live
//! here. The enforcement lives in pa-core's sandbox layer; the privacy contract
//! (primitives only, never content) lives in pa-telemetry.

use serde::{Deserialize, Serialize};

/// The effective sandbox posture for a kernel launch.
///
/// Unset settings read as [`SandboxMode::Off`]; the env override
/// (`PRIME_AGENT_SANDBOX_MODE`) and the settings file are resolved by the
/// caller (pa-core), not by this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// No sandbox boundary (the default when unset).
    #[default]
    Off,
    /// Read-only: the kernel can read the workspace and whitelisted roots but
    /// cannot write or unlink anything outside explicit allows.
    ReadOnly,
    /// Workspace-write: reads and writes inside the workspace (plus explicit
    /// extra roots), denied outside it.
    WorkspaceWrite,
    /// Danger full access: no sandbox boundary (the posture is explicit, not
    /// merely unset).
    DangerFullAccess,
}

impl SandboxMode {
    /// The canonical kebab-case wire/telemetry slug.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

/// The network posture allowed inside the sandbox (`none` by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxNetworkMode {
    /// No network (deny by default).
    #[default]
    None,
    /// Full network access.
    Full,
}

impl SandboxNetworkMode {
    /// The canonical kebab-case wire/telemetry slug.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Full => "full",
        }
    }
}

/// The OS sandbox backend selected for a kernel launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxBackend {
    /// No sandbox boundary (the default; matches [`SandboxMode::Off`]).
    #[default]
    None,
    /// Seatbelt (macOS sandbox-exec profile).
    Seatbelt,
    /// Bubblewrap (Linux user-namespace sandbox).
    Bubblewrap,
}

impl SandboxBackend {
    /// The canonical kebab-case wire/telemetry slug.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Seatbelt => "seatbelt",
            Self::Bubblewrap => "bubblewrap",
        }
    }
}

/// One kind of sandbox denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxViolationKind {
    /// A file read the boundary refused.
    FileRead,
    /// A file write the boundary refused.
    FileWrite,
    /// A file unlink/delete the boundary refused.
    FileUnlink,
    /// A network operation the boundary refused.
    Network,
}

impl SandboxViolationKind {
    /// The canonical kebab-case wire slug.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FileRead => "file-read",
            Self::FileWrite => "file-write",
            Self::FileUnlink => "file-unlink",
            Self::Network => "network",
        }
    }
}

/// One sandbox denial. Serializes as a flat object (`kind`, `backend`,
/// `path`?, `detail`); `path` is omitted when the backend denies without
/// naming one.
///
/// `detail` is a short, non-content reason string; it never carries prompts,
/// tool arguments, file contents, or command output, all of which belong in
/// the session transcript, not in an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxViolationEvent {
    /// The operation the boundary denied.
    pub kind: SandboxViolationKind,
    /// The backend that denied it ([`SandboxBackend::None`] when no boundary is
    /// involved, e.g. a pre-launch refusal).
    pub backend: SandboxBackend,
    /// The path the denial targeted, when one was named.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A short, non-content denial detail.
    pub detail: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_mode_wire_values_and_round_trip() {
        let cases = [
            (SandboxMode::Off, "off"),
            (SandboxMode::ReadOnly, "read-only"),
            (SandboxMode::WorkspaceWrite, "workspace-write"),
            (SandboxMode::DangerFullAccess, "danger-full-access"),
        ];
        for (mode, wire) in cases {
            assert_eq!(mode.as_str(), wire);
            assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{wire}\""));
            assert_eq!(
                serde_json::from_str::<SandboxMode>(&format!("\"{wire}\"")).unwrap(),
                mode
            );
        }
    }

    #[test]
    fn sandbox_network_mode_wire_values_and_round_trip() {
        let cases = [
            (SandboxNetworkMode::None, "none"),
            (SandboxNetworkMode::Full, "full"),
        ];
        for (mode, wire) in cases {
            assert_eq!(mode.as_str(), wire);
            assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{wire}\""));
            assert_eq!(
                serde_json::from_str::<SandboxNetworkMode>(&format!("\"{wire}\"")).unwrap(),
                mode
            );
        }
    }

    #[test]
    fn sandbox_backend_wire_values_and_round_trip() {
        let cases = [
            (SandboxBackend::None, "none"),
            (SandboxBackend::Seatbelt, "seatbelt"),
            (SandboxBackend::Bubblewrap, "bubblewrap"),
        ];
        for (backend, wire) in cases {
            assert_eq!(backend.as_str(), wire);
            assert_eq!(
                serde_json::to_string(&backend).unwrap(),
                format!("\"{wire}\"")
            );
            assert_eq!(
                serde_json::from_str::<SandboxBackend>(&format!("\"{wire}\"")).unwrap(),
                backend
            );
        }
    }

    #[test]
    fn sandbox_violation_kind_wire_values_and_round_trip() {
        let cases = [
            (SandboxViolationKind::FileRead, "file-read"),
            (SandboxViolationKind::FileWrite, "file-write"),
            (SandboxViolationKind::FileUnlink, "file-unlink"),
            (SandboxViolationKind::Network, "network"),
        ];
        for (kind, wire) in cases {
            assert_eq!(kind.as_str(), wire);
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{wire}\""));
            assert_eq!(
                serde_json::from_str::<SandboxViolationKind>(&format!("\"{wire}\"")).unwrap(),
                kind
            );
        }
    }

    #[test]
    fn violation_event_round_trips_with_and_without_a_path() {
        let with_path = SandboxViolationEvent {
            kind: SandboxViolationKind::FileWrite,
            backend: SandboxBackend::Seatbelt,
            path: Some("/outside/workspace/file.txt".to_string()),
            detail: "write denied by profile".to_string(),
        };
        let with_path_json = serde_json::to_string(&with_path).unwrap();
        assert!(with_path_json.contains("\"path\":\"/outside/workspace/file.txt\""));
        let with_path_back: SandboxViolationEvent = serde_json::from_str(&with_path_json).unwrap();
        assert_eq!(with_path_back, with_path);

        let without_path = SandboxViolationEvent {
            kind: SandboxViolationKind::Network,
            backend: SandboxBackend::Bubblewrap,
            path: None,
            detail: "network access denied".to_string(),
        };
        let without_path_json = serde_json::to_string(&without_path).unwrap();
        assert!(!without_path_json.contains("\"path\""));
        let without_path_back: SandboxViolationEvent =
            serde_json::from_str(&without_path_json).unwrap();
        assert_eq!(without_path_back, without_path);
    }
}
