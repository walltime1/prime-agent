//! Settings types. All fields optional; JSON values of the wrong type load
//! as `None` (the TS access-time typechecks), never a load error.

use pa_types::sandbox::{SandboxMode, SandboxNetworkMode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MermaidRenderingMode {
    Off,
    Final,
    Streaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateChannel {
    Stable,
    Nightly,
}

/// Thinking levels in wire form ("off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingLevelSetting {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevelSetting {
    /// The reverse of [`ThinkingLevelSetting::model_level`]: a live switch (daemon
    /// `set_thinking_level`) persists a model-vocabulary level as the settings default.
    #[must_use]
    pub fn from_model_level(level: pa_types::ai::ModelThinkingLevel) -> ThinkingLevelSetting {
        match level {
            pa_types::ai::ModelThinkingLevel::Off => ThinkingLevelSetting::Off,
            pa_types::ai::ModelThinkingLevel::Minimal => ThinkingLevelSetting::Minimal,
            pa_types::ai::ModelThinkingLevel::Low => ThinkingLevelSetting::Low,
            pa_types::ai::ModelThinkingLevel::Medium => ThinkingLevelSetting::Medium,
            pa_types::ai::ModelThinkingLevel::High => ThinkingLevelSetting::High,
            pa_types::ai::ModelThinkingLevel::Xhigh => ThinkingLevelSetting::Xhigh,
            pa_types::ai::ModelThinkingLevel::Max => ThinkingLevelSetting::Max,
        }
    }

    /// The same level in the shared model vocabulary: the settings default
    /// feeds session thinking-level resolution.
    #[must_use]
    pub fn model_level(self) -> pa_types::ai::ModelThinkingLevel {
        match self {
            ThinkingLevelSetting::Off => pa_types::ai::ModelThinkingLevel::Off,
            ThinkingLevelSetting::Minimal => pa_types::ai::ModelThinkingLevel::Minimal,
            ThinkingLevelSetting::Low => pa_types::ai::ModelThinkingLevel::Low,
            ThinkingLevelSetting::Medium => pa_types::ai::ModelThinkingLevel::Medium,
            ThinkingLevelSetting::High => pa_types::ai::ModelThinkingLevel::High,
            ThinkingLevelSetting::Xhigh => pa_types::ai::ModelThinkingLevel::Xhigh,
            ThinkingLevelSetting::Max => pa_types::ai::ModelThinkingLevel::Max,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueModeSetting {
    All,
    #[serde(alias = "oneAtATime")]
    OneAtATime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportSetting {
    Auto,
    Sse,
    WebSocket,
    #[serde(rename = "websocket-cached")]
    WebSocketCached,
}

fn deserialize_threshold_percent<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<f64>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_f64()
        .filter(|percent| pa_types::compaction::is_valid_threshold_percent(*percent)))
}

/// `number | "unlimited"` autonomous limit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AutonomousLimitSetting {
    Number(f64),
    Unlimited(String),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    pub enabled: Option<bool>,
    /// Percentage ceiling for automatic compaction; invalid values behave as unset.
    #[serde(default, deserialize_with = "deserialize_threshold_percent")]
    pub threshold_percent: Option<f64>,
    pub reserve_tokens: Option<u64>,
    pub keep_recent_tokens: Option<u64>,
    pub agent_callable: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummarySettings {
    pub reserve_tokens: Option<u64>,
    pub skip_prompt: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoRefineSettings {
    pub enabled: Option<bool>,
    pub turn_interval: Option<u64>,
    pub compact: Option<bool>,
    pub cooldown_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderWaitSettings {
    pub enabled: Option<bool>,
    pub base_delay_ms: Option<u64>,
    pub max_delay_ms: Option<u64>,
    pub max_attempts: Option<u64>,
    pub max_wait_ms: Option<u64>,
    /// Park sessions for provider-reported resets beyond the bounded wait (default true).
    pub pause_until_reset: Option<bool>,
    /// Maximum single park duration (default 24h, clamped to 7d).
    pub max_pause_ms: Option<u64>,
    /// Maximum parks per quota episode (default 8).
    pub max_parks: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRetrySettings {
    pub timeout_ms: Option<u64>,
    pub max_retry_delay_ms: Option<u64>,
    pub wait_for_usage: Option<ProviderWaitSettings>,
}

/// Provider-failover settings (`retry.failover`): when another configured
/// provider serves the same model, a provider that exhausts its quick
/// retries hands the turn to the next one instead of failing it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryFailoverSettings {
    pub enabled: Option<bool>,
    /// Retries per provider before switching (default 5).
    pub max_retries: Option<u64>,
    /// First backoff delay, doubling each retry (default 1000ms).
    pub base_delay_ms: Option<u64>,
    /// Backoff ceiling per retry (default 30000ms).
    pub max_delay_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrySettings {
    pub enabled: Option<bool>,
    pub max_retries: Option<u64>,
    pub base_delay_ms: Option<u64>,
    pub failover: Option<RetryFailoverSettings>,
    pub provider: Option<ProviderRetrySettings>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSettings {
    pub show_images: Option<bool>,
    pub clear_on_shrink: Option<bool>,
    pub show_terminal_progress: Option<bool>,
    pub fullscreen_mouse: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSettings {
    pub auto_resize: Option<bool>,
    pub block_images: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingBudgetsSettings {
    pub minimal: Option<u64>,
    pub low: Option<u64>,
    pub medium: Option<u64>,
    pub high: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutonomousSettings {
    pub max_continuations: Option<AutonomousLimitSetting>,
    pub max_turns: Option<AutonomousLimitSetting>,
    pub max_tokens: Option<AutonomousLimitSetting>,
    pub timeout_ms: Option<AutonomousLimitSetting>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownSettings {
    pub code_block_indent: Option<String>,
    pub mermaid: Option<MermaidRenderingMode>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundledSkillsSettings {
    pub websearch: Option<bool>,
}

/// `webSearchProvider`: the service the bundled websearch skill queries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    #[default]
    Serper,
    /// You.com Eco Search.
    Eco,
}

impl WebSearchProvider {
    /// The provider's id: its settings value, its auth-store slot, and the
    /// kernel's `PRIME_AGENT_WEBSEARCH_PROVIDER` value.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            WebSearchProvider::Serper => crate::auth::SERPER_CREDENTIAL_ID,
            WebSearchProvider::Eco => crate::auth::ECO_CREDENTIAL_ID,
        }
    }

    /// The environment variable whose nonblank value overrides the stored key.
    #[must_use]
    pub fn api_key_env(self) -> &'static str {
        match self {
            WebSearchProvider::Serper => "SERPER_API_KEY",
            WebSearchProvider::Eco => "YOU_API_KEY",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarningSettings {
    pub anthropic_extra_usage: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTracesSettings {
    pub enabled: Option<bool>,
}

/// `factory` (the agent factory's opt-in gate): `factory.enabled` is
/// unset/false by default, and the `/factory on` client command persists
/// it. Both the daemon's `factory_activity` lane advertisement and the
/// kernel's factory gate read the same shared settings key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactorySettings {
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySettings {
    pub enabled: Option<bool>,
    pub notice_shown: Option<bool>,
    /// Local JSONL mirror at `<agentDir>/telemetry.jsonl` (default on:
    /// user-observable transparency).
    pub local_mirror: Option<bool>,
}

/// OS-level sandbox boundary (global settings only; project overrides are stripped at
/// load — see the scrub in `settings/manager.rs`). Unset means Off; the accessor is
/// [`Settings::sandbox_mode`].
///
/// Semantics (the sandbox layer in pa-core owns enforcement):
/// - `mode`: the posture; unset reads as off.
/// - `network`: the network posture; unset means deny network.
/// - `extra_read_roots` / `extra_write_roots`: explicit additional roots the boundary
///   allows; the workspace itself is always allowed for reads (and writes in
///   workspace-write mode).
/// - `scrub_env`: whether the kernel launch env is scrubbed (default: scrubbed).
/// - `protect_vcs`: whether `.git` and other VCS metadata are protected from writes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxSettings {
    /// The sandbox posture; unset reads as [`SandboxMode::Off`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<SandboxMode>,
    /// The network posture; unset means deny network.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<SandboxNetworkMode>,
    /// Additional roots the boundary allows for reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_read_roots: Option<Vec<String>>,
    /// Additional roots the boundary allows for writes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_write_roots: Option<Vec<String>>,
    /// Whether the kernel launch environment is scrubbed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrub_env: Option<bool>,
    /// Whether VCS metadata is protected from writes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protect_vcs: Option<bool>,
}

/// Parse the `PRIME_AGENT_SANDBOX_MODE` override value: the documented kebab-case
/// vocabulary (`off`, `read-only`, `workspace-write`, `danger-full-access`), matched
/// case- and whitespace-insensitively. Unknown values return `None`: an invalid
/// override falls back to settings, never panics.
fn parse_sandbox_mode_override(value: &str) -> Option<SandboxMode> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "off" => Some(SandboxMode::Off),
        "read-only" => Some(SandboxMode::ReadOnly),
        "workspace-write" => Some(SandboxMode::WorkspaceWrite),
        "danger-full-access" => Some(SandboxMode::DangerFullAccess),
        _ => None,
    }
}

/// The env override (`PRIME_AGENT_SANDBOX_MODE`), if set to a recognized value.
fn sandbox_mode_from_env() -> Option<SandboxMode> {
    let value = std::env::var("PRIME_AGENT_SANDBOX_MODE").ok()?;
    parse_sandbox_mode_override(&value)
}

impl Settings {
    /// The effective sandbox mode: the `PRIME_AGENT_SANDBOX_MODE` env override wins
    /// (unknown values are ignored), else `sandbox.mode`, else [`SandboxMode::Off`].
    #[must_use]
    pub fn sandbox_mode(&self) -> SandboxMode {
        sandbox_mode_from_env().unwrap_or_else(|| {
            self.sandbox
                .as_ref()
                .and_then(|sandbox| sandbox.mode)
                .unwrap_or(SandboxMode::Off)
        })
    }
}

/// The default time limit for user `python_repl` cells: 60 seconds.
pub const DEFAULT_PYTHON_CELL_TIMEOUT_MS: u64 = 60_000;

/// The settings document: every known field optional; unknown keys preserved.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub onboarding_shown: Option<bool>,
    pub onboarding_completed: Option<bool>,
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub subagent_default_model: Option<String>,
    pub update_channel: Option<UpdateChannel>,
    pub recent_models: Option<Vec<String>>,
    pub auxiliary_model: Option<String>,
    pub default_thinking_level: Option<ThinkingLevelSetting>,
    pub default_service_tier: Option<String>,
    pub rlm_max_depth: Option<u64>,
    /// `number | "off" | "none"`; raw JSON because the TS getter validates at access time.
    pub idle_eviction_minutes: Option<serde_json::Value>,
    pub transport: Option<TransportSetting>,
    pub steering_mode: Option<QueueModeSetting>,
    pub follow_up_mode: Option<QueueModeSetting>,
    pub theme: Option<String>,
    pub compaction: Option<CompactionSettings>,
    pub auto_refine: Option<AutoRefineSettings>,
    /// Master switch for harness creation and context loading; unset preserves enabled behavior.
    pub harness_refinements: Option<bool>,
    pub agent_traces: Option<AgentTracesSettings>,
    pub factory: Option<FactorySettings>,
    pub telemetry: Option<TelemetrySettings>,
    pub branch_summary: Option<BranchSummarySettings>,
    pub retry: Option<RetrySettings>,
    pub provider_backup_model: Option<String>,
    /// Model ("provider/model-id" or a bare model id) that serves turns attaching images when the
    /// session model does not accept image input. Default: none - image turns on a text-only model
    /// fail with a configuration hint instead of silently dropping the images.
    pub image_model: Option<String>,
    /// Default time limit for user `python_repl` cells, in milliseconds
    /// (default [`DEFAULT_PYTHON_CELL_TIMEOUT_MS`]; `0` disables). A cell
    /// running longer is interrupted; the model can pass a per-call
    /// `timeout` instead. Daemonless sessions never arm it.
    pub python_cell_timeout_ms: Option<u64>,
    pub autonomous: Option<AutonomousSettings>,
    /// Custom shell that runs every kernel `bash()` and user-bash command as
    /// `<shell> -c <command>`: an absolute path to an existing executable,
    /// validated once at session start (a bad value fails session creation
    /// naming this setting). Only global settings can select this executable.
    pub shell_path: Option<String>,
    pub quiet_startup: Option<bool>,
    pub shell_command_prefix: Option<String>,
    pub npm_command: Option<Vec<String>>,
    /// Execution reads this from global settings only; project overrides are ignored.
    pub mcp_servers: Option<serde_json::Map<String, serde_json::Value>>,
    /// Extra local MCP service-catalog files (~-relative allowed), merged after the
    /// compiled built-ins; first source wins per id, and no bundled id can be shadowed.
    /// Read from global settings only, like executable MCP server configuration.
    pub mcp_catalog_sources: Option<Vec<String>>,
    pub packages: Option<Vec<serde_json::Value>>,
    pub skills: Option<Vec<String>>,
    pub prompts: Option<Vec<String>>,
    pub themes: Option<Vec<String>>,
    pub enable_skill_commands: Option<bool>,
    pub bundled_skills: Option<BundledSkillsSettings>,
    /// Read from global settings only, like the stored keys it selects between.
    pub web_search_provider: Option<WebSearchProvider>,
    pub enable_builtin_skills: Option<bool>,
    pub terminal: Option<TerminalSettings>,
    pub images: Option<ImageSettings>,
    pub enabled_models: Option<Vec<String>>,
    /// Rust-only daemon-level model allowlist: a model outside fails loudly instead
    /// of resolving, with no fallback. `None` is unrestricted.
    pub allowed_models: Option<Vec<String>>,
    pub tree_filter_mode: Option<String>,
    /// `chatDetail`: the conversation-detail level Ctrl+O cycles and persists
    /// ("overview"/"details"/"all"); `None` reads as the `overview` startup level.
    pub chat_detail: Option<String>,
    pub thinking_budgets: Option<ThinkingBudgetsSettings>,
    pub editor_padding_x: Option<u64>,
    pub autocomplete_max_visible: Option<u64>,
    pub show_hardware_cursor: Option<bool>,
    /// Typing `:name` in the prompt editor suggests matching emoji (unset means on).
    pub emoji_shortcodes: Option<bool>,
    pub markdown: Option<MarkdownSettings>,
    pub warnings: Option<WarningSettings>,
    /// OS-level sandbox boundary; global settings only (project overrides are
    /// stripped). Unset means [`SandboxMode::Off`].
    pub sandbox: Option<SandboxSettings>,
    /// Session storage can restore executable kernel state, so only global
    /// settings (or an explicit CLI/environment override) may select it.
    pub session_dir: Option<String>,
    /// Log per-request provider timing phases to the diagnostic log
    /// (unset means OFF, exactly the TS default).
    pub request_timing: Option<bool>,
    /// Unknown keys survive load/save round-trips (forward compatibility).
    #[serde(flatten, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_settings_load_as_camel_case_and_skip_unset_fields() {
        let settings: Settings = serde_json::from_value(serde_json::json!({
            "sandbox": {
                "mode": "read-only",
                "network": "full",
                "extraReadRoots": ["/trusted/read"],
                "extraWriteRoots": ["/trusted/write"],
                "scrubEnv": true,
                "protectVcs": false
            }
        }))
        .unwrap();
        let sandbox = settings.sandbox.as_ref().unwrap();
        assert_eq!(sandbox.mode, Some(SandboxMode::ReadOnly));
        assert_eq!(sandbox.network, Some(SandboxNetworkMode::Full));
        assert_eq!(
            sandbox.extra_read_roots,
            Some(vec!["/trusted/read".to_string()])
        );
        assert_eq!(
            sandbox.extra_write_roots,
            Some(vec!["/trusted/write".to_string()])
        );
        assert_eq!(sandbox.scrub_env, Some(true));
        assert_eq!(sandbox.protect_vcs, Some(false));

        let serialized = serde_json::to_value(&settings).unwrap();
        let sandbox = serialized.get("sandbox").unwrap();
        assert_eq!(
            sandbox.get("extraReadRoots"),
            Some(&serde_json::json!(["/trusted/read"]))
        );
        assert_eq!(sandbox.get("scrubEnv"), Some(&serde_json::json!(true)));
        let minimal = Settings {
            sandbox: Some(SandboxSettings {
                mode: Some(SandboxMode::Off),
                ..SandboxSettings::default()
            }),
            ..Settings::default()
        };
        // The sandbox block serializes only the fields that are set.
        let minimal_values = serde_json::to_value(&minimal.sandbox).unwrap();
        let block = minimal_values.as_object().unwrap();
        assert_eq!(block.len(), 1);
        assert_eq!(block.get("mode"), Some(&serde_json::json!("off")));
    }

    #[test]
    fn sandbox_mode_override_parses_the_documented_values() {
        assert_eq!(parse_sandbox_mode_override("off"), Some(SandboxMode::Off));
        assert_eq!(
            parse_sandbox_mode_override("read-only"),
            Some(SandboxMode::ReadOnly)
        );
        assert_eq!(
            parse_sandbox_mode_override("workspace-write"),
            Some(SandboxMode::WorkspaceWrite)
        );
        assert_eq!(
            parse_sandbox_mode_override("danger-full-access"),
            Some(SandboxMode::DangerFullAccess)
        );
        assert_eq!(
            parse_sandbox_mode_override("  READ-ONLY  "),
            Some(SandboxMode::ReadOnly)
        );
        assert_eq!(parse_sandbox_mode_override("bogus"), None);
        assert_eq!(parse_sandbox_mode_override(""), None);
    }

    #[test]
    fn sandbox_mode_falls_back_to_settings_and_off() {
        // The env override is ambient-shell territory: when it is set, the test
        // skips rather than racing the real value against an in-process fixture.
        if std::env::var_os("PRIME_AGENT_SANDBOX_MODE").is_some() {
            return;
        }
        assert_eq!(Settings::default().sandbox_mode(), SandboxMode::Off);
        assert_eq!(
            Settings {
                sandbox: Some(SandboxSettings {
                    mode: Some(SandboxMode::ReadOnly),
                    ..SandboxSettings::default()
                }),
                ..Settings::default()
            }
            .sandbox_mode(),
            SandboxMode::ReadOnly
        );
    }
}
