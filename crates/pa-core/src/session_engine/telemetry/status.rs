//! The telemetry on/off switch and its explanation: one resolution shared by
//! the live client switch, the CLI opt-out check, `/telemetry`, and
//! `prime-agent telemetry`.

use std::path::Path;

use pa_telemetry::parse_bool_override;

use crate::settings::SettingsManager;

/// What decides whether telemetry is on, in precedence order (TS
/// `isTelemetryEnabled`): `PI_OFFLINE` / `DO_NOT_TRACK` force off,
/// `PRIME_AGENT_TELEMETRY` forces either way, then settings
/// `telemetry.enabled`, then the default (on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetrySwitch {
    /// Nothing set: on.
    Default,
    /// Settings `telemetry.enabled` (any scope false turns it off).
    Settings(bool),
    /// An environment variable decides.
    Env { var: &'static str, enabled: bool },
}

impl TelemetrySwitch {
    #[must_use]
    pub fn enabled(self) -> bool {
        match self {
            Self::Default => true,
            Self::Settings(enabled) | Self::Env { enabled, .. } => enabled,
        }
    }

    /// Why it is on or off, in plain words.
    #[must_use]
    pub fn reason(self) -> String {
        match self {
            Self::Default => "on by default".to_string(),
            Self::Settings(true) => "turned on in settings".to_string(),
            Self::Settings(false) => "turned off in settings".to_string(),
            Self::Env {
                var: "PRIME_AGENT_TELEMETRY",
                enabled,
            } => format!(
                "forced {} by PRIME_AGENT_TELEMETRY={}",
                if enabled { "on" } else { "off" },
                if enabled { "1" } else { "0" }
            ),
            Self::Env { var, .. } => format!("forced off by {var}"),
        }
    }
}

/// Resolve the switch from the environment and `settings`. Telemetry is
/// stubbed off in this build: an environment opt-out still reports as one (it
/// rides the daemon create wire), and anything that would enable it reads as off.
#[must_use]
pub fn telemetry_switch(_settings: &SettingsManager) -> TelemetrySwitch {
    match switch_from(|var| std::env::var(var).ok(), Some(false)) {
        switch @ TelemetrySwitch::Env { enabled: false, .. } => switch,
        TelemetrySwitch::Env { enabled: true, .. }
        | TelemetrySwitch::Settings(_)
        | TelemetrySwitch::Default => TelemetrySwitch::Settings(false),
    }
}

/// The environment variables that decide the switch (see [`TelemetrySwitch`]).
pub const TELEMETRY_SWITCH_VARS: [&str; 3] =
    ["PI_OFFLINE", "DO_NOT_TRACK", "PRIME_AGENT_TELEMETRY"];

fn switch_from(env: impl Fn(&str) -> Option<String>, setting: Option<bool>) -> TelemetrySwitch {
    let [offline, do_not_track, forced] = TELEMETRY_SWITCH_VARS;
    for var in [offline, do_not_track] {
        if parse_bool_override(env(var).as_deref()) == Some(true) {
            return TelemetrySwitch::Env {
                var,
                enabled: false,
            };
        }
    }
    if let Some(enabled) = parse_bool_override(env(forced).as_deref()) {
        return TelemetrySwitch::Env {
            var: forced,
            enabled,
        };
    }
    setting.map_or(TelemetrySwitch::Default, TelemetrySwitch::Settings)
}

/// Where events go: the platform endpoint in release builds, nowhere in
/// debug builds (every `cargo test` and dev run), so tests and local
/// development never reach production analytics.
#[must_use]
pub fn telemetry_endpoint() -> Option<&'static str> {
    None
}

/// The `status` report. Telemetry is stubbed out, so this always says that
/// nothing is collected or sent.
#[must_use]
pub fn telemetry_status_text(_settings: &SettingsManager, _agent_dir: &Path) -> String {
    "Telemetry is disabled in this build. Nothing is collected or sent.".to_string()
}

/// Persist `telemetry.enabled` (the TS key: global `settings.json`
/// `{"telemetry": {"enabled": <bool>}}`) and report that it has no effect,
/// since telemetry is stubbed out in this build.
///
/// # Errors
///
/// Returns an error when the global settings file cannot be written.
pub fn set_telemetry_enabled_text(
    settings: &mut SettingsManager,
    _agent_dir: &Path,
    enabled: bool,
) -> anyhow::Result<String> {
    settings.set_telemetry_enabled(enabled)?;
    Ok("Telemetry is disabled in this build; the setting was saved but has no effect.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |var| {
            pairs
                .iter()
                .find(|(name, _)| *name == var)
                .map(|(_, value)| (*value).to_string())
        }
    }

    #[test]
    fn the_switch_follows_the_ts_precedence() {
        assert_eq!(switch_from(env(&[]), None), TelemetrySwitch::Default);
        assert_eq!(
            switch_from(env(&[]), Some(false)),
            TelemetrySwitch::Settings(false)
        );
        assert_eq!(
            switch_from(env(&[("PRIME_AGENT_TELEMETRY", "1")]), Some(false)),
            TelemetrySwitch::Env {
                var: "PRIME_AGENT_TELEMETRY",
                enabled: true
            }
        );
        assert_eq!(
            switch_from(
                env(&[("PRIME_AGENT_TELEMETRY", "1"), ("DO_NOT_TRACK", "1")]),
                None
            ),
            TelemetrySwitch::Env {
                var: "DO_NOT_TRACK",
                enabled: false
            }
        );
        assert_eq!(
            switch_from(env(&[("PI_OFFLINE", "true")]), Some(true)).reason(),
            "forced off by PI_OFFLINE"
        );
        assert_eq!(
            switch_from(env(&[("PRIME_AGENT_TELEMETRY", "0")]), None).reason(),
            "forced off by PRIME_AGENT_TELEMETRY=0"
        );
        // Falsy DO_NOT_TRACK is no override.
        assert_eq!(
            switch_from(env(&[("DO_NOT_TRACK", "0")]), None),
            TelemetrySwitch::Default
        );
    }

    #[test]
    fn on_and_off_round_trip_through_the_ts_settings_key() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        let mut settings = SettingsManager::create(dir.path(), &agent_dir);
        assert_eq!(settings.telemetry_enabled_setting(), None);

        set_telemetry_enabled_text(&mut settings, &agent_dir, false).unwrap();
        let written: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(agent_dir.join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(written["telemetry"]["enabled"], false);
        let reread = SettingsManager::create(dir.path(), &agent_dir);
        assert_eq!(reread.telemetry_enabled_setting(), Some(false));
        assert!(!reread.get_telemetry_enabled());

        set_telemetry_enabled_text(&mut settings, &agent_dir, true).unwrap();
        let reread = SettingsManager::create(dir.path(), &agent_dir);
        assert_eq!(reread.telemetry_enabled_setting(), Some(true));
        assert!(reread.get_telemetry_enabled());
    }

    #[test]
    fn a_ts_written_switch_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("settings.json"),
            r#"{ "telemetry": { "enabled": false, "noticeShown": true } }"#,
        )
        .unwrap();
        let settings = SettingsManager::create(dir.path(), &agent_dir);
        assert_eq!(settings.telemetry_enabled_setting(), Some(false));
    }

    #[test]
    fn status_says_telemetry_is_disabled_in_this_build() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        let settings = SettingsManager::create(dir.path(), &agent_dir);
        assert_eq!(
            telemetry_status_text(&settings, &agent_dir),
            "Telemetry is disabled in this build. Nothing is collected or sent."
        );
    }
}
