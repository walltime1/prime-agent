//! `prime-agent telemetry [status|on|off]` end to end: the binary reports
//! the switch and why, persists the TS settings key, and names an
//! environment opt-out that still wins over a saved `on`.

use std::path::Path;
use std::process::Command;

fn run(sandbox: &Path, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_prime-agent"));
    command
        .args(args)
        .env("PRIME_AGENT_CODING_AGENT_DIR", sandbox.join("agent"))
        .env("HOME", sandbox)
        .env_remove("PI_OFFLINE")
        .env_remove("DO_NOT_TRACK")
        .env_remove("PRIME_AGENT_TELEMETRY")
        .current_dir(sandbox);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("spawn prime-agent");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

fn saved_switch(sandbox: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(sandbox.join("agent").join("settings.json"))
        .expect("settings.json written");
    serde_json::from_str::<serde_json::Value>(&text).expect("settings json")["telemetry"]["enabled"]
        .clone()
}

#[test]
fn status_on_and_off_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let sandbox = dir.path();

    let (code, out) = run(sandbox, &["telemetry"], &[]);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(
        out.trim(),
        "Telemetry is disabled in this build. Nothing is collected or sent."
    );

    let (code, out) = run(sandbox, &["telemetry", "off"], &[]);
    assert_eq!(code, Some(0), "{out}");
    assert_eq!(
        out.trim(),
        "Telemetry is disabled in this build; the setting was saved but has no effect."
    );
    assert_eq!(saved_switch(sandbox), serde_json::json!(false));

    let (code, _) = run(sandbox, &["telemetry", "on"], &[("DO_NOT_TRACK", "1")]);
    assert_eq!(code, Some(0));
    assert_eq!(saved_switch(sandbox), serde_json::json!(true));

    let (code, _) = run(sandbox, &["telemetry", "maybe"], &[]);
    assert_eq!(code, Some(1));
}
