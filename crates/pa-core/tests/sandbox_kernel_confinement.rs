// Pedantic-gate dispositions as src/lib.rs (large_futures/too_many_lines/casts).
#![allow(
    clippy::large_futures,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

//! Live verifier for the OS sandbox around the Python kernel: reads and
//! writes above the workspace, writes into VCS metadata, and outbound TCP
//! all fail inside the sandboxed kernel, while the workspace, the private
//! temp directory, `rlm.*` stdio IPC, and snapshot save/restore keep working.
//! Every check runs in its own kernel boot; all paths live under
//! `tempfile::TempDir`, never the real home or artifact directories.
//!
//! The kernel Python is ambient (the kernel venv, or `PA_CORE_KERNEL_PYTHON`);
//! a machine without one skips with a note, like the other live tests.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pa_core::kernel::bootstrap::KernelPythonSkill;
use pa_core::kernel::provisioner::{IpythonKernelProvisioner, IpythonKernelProvisionerOptions};
use pa_core::kernel::shared::{host_handler, ExecuteOptions, HostRequestHandlers};
use pa_core::sandbox::SandboxPolicy;
use pa_types::sandbox::{SandboxMode, SandboxNetworkMode};

/// The kernel Python with prime-agent-runtime installed; skipped when absent.
fn kernel_python() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("PA_CORE_KERNEL_PYTHON") {
        let explicit = PathBuf::from(explicit);
        assert!(
            explicit.exists(),
            "PA_CORE_KERNEL_PYTHON {} not found",
            explicit.display()
        );
        return Some(explicit);
    }
    let home = pa_types::platform::home_dir().expect("home dir");
    let candidate = home.join(".prime/agent/kernel-venv/bin/python");
    if candidate.exists() {
        return Some(candidate);
    }
    eprintln!(
        "kernel python {} not found; skipping live sandbox test",
        candidate.display()
    );
    None
}

/// A temp-dir layout: `ws` is the session workspace, `above.txt` a canary
/// one level above it, `artifacts` the session artifact dir.
struct Layout {
    /// Owns the temp tree; kept alive for the whole test.
    root: tempfile::TempDir,
    ws: PathBuf,
    canary: PathBuf,
    artifacts: PathBuf,
}

fn layout() -> Layout {
    let root = tempfile::tempdir().expect("temp root");
    let ws = root.path().join("ws");
    let artifacts = root.path().join("artifacts");
    std::fs::create_dir_all(&ws).expect("workspace");
    std::fs::create_dir_all(ws.join(".git/hooks")).expect("hooks dir");
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let canary = root.path().join("above.txt");
    std::fs::write(&canary, "canary\n").expect("canary");
    Layout {
        root,
        ws,
        canary,
        artifacts,
    }
}

fn policy(layout: &Layout, network: SandboxNetworkMode) -> SandboxPolicy {
    let python = kernel_python().expect("kernel python");
    // Build through the real resolver so the profile under test is exactly
    // the one a session gets.
    let inputs = pa_core::sandbox::SandboxInputs {
        mode: SandboxMode::WorkspaceWrite,
        network,
        settings: None,
        cwd: layout.ws.clone(),
        home: pa_types::platform::home_dir().expect("home dir"),
        venv: python
            .parent()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_path_buf),
        python,
        python_skills: Vec::new(),
        shell_path: None,
        global_harness_dir: None,
        workspace_harness_dir: None,
        snapshot_dir: Some(layout.artifacts.clone()),
        orphan_journal: None,
    };
    pa_core::sandbox::resolve(&inputs)
        .expect("policy resolves")
        .expect("workspace-write resolves a policy")
}

/// One kernel boot with `sandbox` and `host_handlers`, running `code` and
/// returning its combined stdout + stderr.
async fn run_cell_with_handlers(
    layout: &Layout,
    sandbox: Option<SandboxPolicy>,
    host_handlers: HostRequestHandlers,
    code: &str,
) -> String {
    let python = kernel_python().expect("kernel python");
    let provisioner = IpythonKernelProvisioner::new(
        &layout.ws,
        IpythonKernelProvisionerOptions {
            python: Some(python),
            sandbox,
            host_handlers,
            python_skills: Vec::<KernelPythonSkill>::new(),
            snapshot_dir: Some(layout.artifacts.clone()),
            ..Default::default()
        },
    );
    let kernel = provisioner.ensure(None, None).await.expect("kernel boots");
    let result = kernel
        .execute(
            code,
            ExecuteOptions {
                signal: None,
                on_stream: None,
                on_late_sent_agent_message: None,
                max_output_chars: None,
                internal: false,
                protocol_repair: false,
                cell_timeout_ms: Some(60_000),
            },
        )
        .await
        .expect("the cell runs");
    provisioner.dispose(None).await;
    let err = result
        .error
        .as_ref()
        .map(|e| format!("\nERROR: {}: {}", e.ename, e.evalue))
        .unwrap_or_default();
    format!("{}{}{}", result.stdout, result.stderr, err)
}

async fn run_cell(layout: &Layout, sandbox: Option<SandboxPolicy>, code: &str) -> String {
    run_cell_with_handlers(layout, sandbox, HostRequestHandlers::new(), code).await
}

#[tokio::test]
async fn reading_above_the_workspace_is_denied_in_python_and_bash() {
    let Some(_) = kernel_python() else { return };
    let layout = layout();
    let code = format!(
        r#"
from rlm.bash import bash
try:
    open({canary:?}).read()
    print("PY_ALLOWED")
except (PermissionError, FileNotFoundError, OSError):
    print("PY_DENIED")
h = await bash("cat {canary} 2>&1")
print("SH_ALLOWED" if "canary" in h.output else "SH_DENIED")
"#,
        canary = layout.canary.display().to_string()
    );
    let output = run_cell(
        &layout,
        Some(policy(&layout, SandboxNetworkMode::None)),
        &code,
    )
    .await;
    assert!(
        output.contains("PY_DENIED") && !output.contains("PY_ALLOWED"),
        "python open() must not read above the workspace: {output}"
    );
    assert!(
        output.contains("SH_DENIED") && !output.contains("SH_ALLOWED"),
        "bash must not read above the workspace either: {output}"
    );
}

#[tokio::test]
async fn writing_above_the_workspace_is_denied_in_python_and_bash() {
    let Some(_) = kernel_python() else { return };
    let layout = layout();
    let above = layout.root.path().join("written-above.txt");
    let code = format!(
        r#"
from rlm.bash import bash
try:
    open({above:?}, "w").write("x")
    print("PY_ALLOWED")
except (PermissionError, OSError):
    print("PY_DENIED")
h = await bash("echo x > {above} 2>&1")
print("SH_ALLOWED" if h.exit_code == 0 else "SH_DENIED")
"#,
        above = above.display().to_string()
    );
    let output = run_cell(
        &layout,
        Some(policy(&layout, SandboxNetworkMode::None)),
        &code,
    )
    .await;
    assert!(
        output.contains("PY_DENIED") && !output.contains("PY_ALLOWED"),
        "python open() must not write above the workspace: {output}"
    );
    assert!(
        output.contains("SH_DENIED") && !output.contains("SH_ALLOWED"),
        "bash must not write above the workspace either: {output}"
    );
    assert!(!above.exists(), "nothing may land above the workspace");
}

#[tokio::test]
async fn writing_into_git_hooks_is_denied() {
    let Some(_) = kernel_python() else { return };
    let layout = layout();
    let hook = layout.ws.join(".git/hooks/pre-commit");
    let code = format!(
        r#"
try:
    open({hook:?}, "w").write("hook")
    print("PY_ALLOWED")
except (PermissionError, OSError):
    print("PY_DENIED")
"#,
        hook = hook.display().to_string()
    );
    let output = run_cell(
        &layout,
        Some(policy(&layout, SandboxNetworkMode::None)),
        &code,
    )
    .await;
    assert!(
        output.contains("PY_DENIED") && !output.contains("PY_ALLOWED"),
        "writing a git hook must be refused: {output}"
    );
    assert!(!hook.exists(), "the hook must not exist");
}

#[tokio::test]
async fn outbound_tcp_is_denied_without_network() {
    let Some(_) = kernel_python() else { return };
    let layout = layout();
    let code = r#"
import socket
try:
    socket.create_connection(("1.1.1.1", 80), timeout=3).close()
    print("NET_ALLOWED")
except OSError:
    print("NET_DENIED")
"#;
    let output = run_cell(
        &layout,
        Some(policy(&layout, SandboxNetworkMode::None)),
        code,
    )
    .await;
    assert!(
        output.contains("NET_DENIED") && !output.contains("NET_ALLOWED"),
        "outbound TCP must fail without network: {output}"
    );
}

#[tokio::test]
async fn the_workspace_temp_ipc_and_snapshot_still_work() {
    let Some(_) = kernel_python() else { return };
    let layout = layout();
    let inside = layout.ws.join("inside.txt");

    let recorded_notes: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let notes_ref = Arc::clone(&recorded_notes);
    let mut handlers = HostRequestHandlers::new();
    handlers.register(
        "rlm.progress.note",
        host_handler(move |payload| {
            let notes_ref = Arc::clone(&notes_ref);
            async move {
                if let Some(msg) = payload
                    .data
                    .get("text")
                    .or_else(|| payload.data.get("message"))
                    .and_then(|v| v.as_str())
                {
                    notes_ref.lock().unwrap().push(msg.to_string());
                }
                Ok(serde_json::json!({ "accepted": true }))
            }
        }),
    );

    let code = format!(
        r#"
import os, tempfile
from rlm.bash import bash
import rlm

open({inside:?}, "w").write("inside")
print("WS_ALLOWED" if open({inside:?}).read() == "inside" else "WS_DENIED")

scratch = os.path.join(tempfile.gettempdir(), "prime-sandbox-probe.txt")
open(scratch, "w").write("scratch")
print("TMP_ALLOWED" if open(scratch).read() == "scratch" else "TMP_DENIED")

h = await bash("echo from-bash")
print("SH_ALLOWED" if "from-bash" in h.output else "SH_DENIED: " + repr((h.exit_code, h.output)))

await rlm.progress_note("sandbox-ipc-ok")
print("RLM_IPC_ALLOWED")

sandbox_snapshot_sentinel = "persisted-42"
"#,
        inside = inside.display().to_string()
    );
    let first = run_cell_with_handlers(
        &layout,
        Some(policy(&layout, SandboxNetworkMode::None)),
        handlers,
        &code,
    )
    .await;
    assert!(
        first.contains("WS_ALLOWED") && !first.contains("WS_DENIED"),
        "workspace read/write must succeed: {first}"
    );
    assert!(
        first.contains("TMP_ALLOWED") && !first.contains("TMP_DENIED"),
        "private $TMPDIR read/write must succeed: {first}"
    );
    assert!(
        first.contains("SH_ALLOWED") && !first.contains("SH_DENIED"),
        "bash() must succeed inside the sandbox: {first}"
    );
    assert!(
        first.contains("RLM_IPC_ALLOWED"),
        "rlm.* stdio IPC must succeed: {first}"
    );
    assert_eq!(
        *recorded_notes.lock().unwrap(),
        vec!["sandbox-ipc-ok".to_string()],
        "host handler must receive rlm.progress_note over stdio IPC"
    );

    // Second sandboxed boot on the same snapshot_dir must restore the variable
    // persisted when the first kernel disposed.
    let restore_code = r#"
print("SNAP_RESTORED" if sandbox_snapshot_sentinel == "persisted-42" else "SNAP_MISSING")
"#;
    let second = run_cell(
        &layout,
        Some(policy(&layout, SandboxNetworkMode::None)),
        restore_code,
    )
    .await;
    assert!(
        second.contains("SNAP_RESTORED") && !second.contains("SNAP_MISSING"),
        "snapshot save/restore must succeed inside the sandbox: {second}"
    );
}

/// The discriminating half: the same read succeeds with no sandbox, so the
/// tests above prove the boundary and not the filesystem.
#[tokio::test]
async fn without_the_sandbox_the_same_operations_succeed() {
    let Some(_) = kernel_python() else { return };
    let layout = layout();
    let code = format!(
        r#"
print("PY_ALLOWED" if open({canary:?}).read().strip() == "canary" else "PY_DENIED")
"#,
        canary = layout.canary.display().to_string()
    );
    let output = run_cell(&layout, None, &code).await;
    assert!(
        output.contains("PY_ALLOWED") && !output.contains("PY_DENIED"),
        "reading the canary without a sandbox must succeed: {output}"
    );
}
