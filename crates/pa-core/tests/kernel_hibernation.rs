// Pedantic-gate dispositions as src/lib.rs (large_futures/too_many_lines/casts).
#![allow(
    clippy::large_futures,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

//! Verifier integration tests for idle kernel hibernation: a kernel with no
//! cell, no host request, and no background `bash()` handle for the TTL
//! flushes its snapshot and exits; the next `ensure()` revives it with the
//! `__main__` namespace intact. The kernel Python is ambient; skipped when
//! absent. Every flag below is an observable, never a fixed sleep.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pa_core::kernel::provisioner::{
    IpythonKernelProvisioner, IpythonKernelProvisionerOptions, RestoreCallback,
};
use pa_core::kernel::shared::{ExecuteOptions, ExecuteStatus};
use pa_core::kernel::state_snapshot::RestoreResult;

/// The kernel Python with prime-agent-runtime installed (see
/// `kernel_python_skills_unavailable.rs`); skipped with a note when absent.
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
    let candidate = PathBuf::from(std::env::var("HOME").map_or_else(
        |_| "/home/ubuntu/.prime/agent/kernel-venv/bin/python".to_string(),
        |home| format!("{home}/.prime/agent/kernel-venv/bin/python"),
    ));
    if candidate.exists() {
        return Some(candidate);
    }
    eprintln!(
        "kernel python {} not found; skipping live hibernation test",
        candidate.display()
    );
    None
}

/// The bound on every observable below: generous for a CI runner, far
/// below a hang.
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(60);

/// A restore callback that records every restore and wakes a listener that
/// checks its predicate, so a test waits for an event instead of polling.
fn watching_restore(
    boot: u64,
    restored: Arc<AtomicUsize>,
    wake: tokio::sync::mpsc::UnboundedSender<u64>,
    watch: impl Fn(&RestoreResult) -> bool + Send + Sync + 'static,
) -> RestoreCallback {
    Arc::new(move |result: &RestoreResult| {
        restored.fetch_add(1, Ordering::SeqCst);
        if watch(result) {
            let _ = wake.send(boot);
        }
    })
}

/// Poll `condition` on a short interval until it holds or [`OBSERVE_TIMEOUT`]
/// passes; the condition is the readiness signal, the interval only its
/// sampling.
async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let polled = async {
        loop {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(OBSERVE_TIMEOUT, polled)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_kernel_hibernates_and_the_next_execute_revives_its_namespace() {
    let Some(python) = kernel_python() else {
        return;
    };
    let dir = tempfile::TempDir::new().unwrap();
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let snapshot = artifacts.join("kernel-state.dill");
    let restored_count = Arc::new(AtomicUsize::new(0));
    let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel();
    let watched_namespace = Arc::new(AtomicBool::new(false));
    let provisioner = IpythonKernelProvisioner::new(
        dir.path(),
        IpythonKernelProvisionerOptions {
            python: Some(python),
            snapshot_dir: Some(artifacts.clone()),
            // `Some(0)` would disable hibernation; a short TTL proves it.
            idle_hibernate_secs: Some(1),
            on_restore: Some(watching_restore(
                1,
                Arc::clone(&restored_count),
                wake_tx.clone(),
                {
                    let watched_namespace = Arc::clone(&watched_namespace);
                    move |result| {
                        watched_namespace.store(
                            result
                                .restored
                                .iter()
                                .any(|name| name == "hibernate_marker"),
                            Ordering::SeqCst,
                        );
                        true
                    }
                },
            )),
            ..Default::default()
        },
    );
    let first = provisioner.ensure(None, None).await.unwrap();
    let first_pid = first.process_id().expect("a running kernel has a pid");
    let written = first
        .execute(
            "hibernate_marker = 2401\n\ndef hibernate_double(n):\n    return n * 2",
            ExecuteOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(written.status, ExecuteStatus::Ok);
    // The kernel hibernates on its own: no stop, no dispose, no kill. The
    // old handle's own shutdown is part of the wait: the hibernation
    // claims the kernel out of the provisioner before shutting it down,
    // so a poll that only checks the provisioner can observe the transient
    // claim of a still-running kernel whose shutdown then backs off busy.
    wait_until("the idle kernel to hibernate", || !first.is_running()).await;
    assert!(
        !first.is_running(),
        "the hibernated kernel process must have exited"
    );
    assert!(
        snapshot.exists(),
        "hibernation flushes the namespace snapshot"
    );
    // The next execute boots a fresh kernel and restores the snapshot: the
    // revive is transparent to the caller.
    let revived = provisioner.ensure(None, None).await.unwrap();
    assert_ne!(
        revived.process_id(),
        Some(first_pid),
        "the revival is a new interpreter"
    );
    let read_back = revived
        .execute(
            "(hibernate_marker, hibernate_double(21))",
            ExecuteOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(read_back.status, ExecuteStatus::Ok);
    assert_eq!(read_back.result.as_deref(), Some("(2401, 42)"));
    assert_eq!(
        restored_count.load(Ordering::SeqCst),
        1,
        "every boot that found a snapshot revives it"
    );
    // The restore carried the hibernated namespace, not just a fresh one.
    assert!(
        watched_namespace.load(Ordering::SeqCst),
        "the revive restored the hibernation snapshot itself"
    );
    assert_eq!(
        wake_rx.try_recv(),
        Ok(1),
        "the revive fired on_restore once"
    );
    provisioner.dispose(None).await;
}

/// Every `ensure()` restarts the idle TTL: a kernel the session keeps using
/// stays resident well past one TTL, then hibernates once the use stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_restarts_the_idle_ttl() {
    let Some(python) = kernel_python() else {
        return;
    };
    let dir = tempfile::TempDir::new().unwrap();
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let provisioner = IpythonKernelProvisioner::new(
        dir.path(),
        IpythonKernelProvisionerOptions {
            python: Some(python),
            snapshot_dir: Some(artifacts),
            idle_hibernate_secs: Some(2),
            ..Default::default()
        },
    );
    let first = provisioner.ensure(None, None).await.unwrap();
    let pid = first.process_id();
    // Short cells every 200ms for twice the TTL: the pacing is the activity
    // under test, and any gap it leaves is a tenth of the TTL.
    let busy_until = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < busy_until {
        let manager = provisioner.ensure(None, None).await.unwrap();
        assert_eq!(manager.process_id(), pid, "a used kernel never hibernates");
        let cell = manager
            .execute("1 + 1", ExecuteOptions::default())
            .await
            .unwrap();
        assert_eq!(cell.status, ExecuteStatus::Ok);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    wait_until("the kernel to hibernate once unused", || {
        !first.is_running()
    })
    .await;
    provisioner.dispose(None).await;
}

/// Hibernation never touches a kernel with a live background `bash()`
/// handle, even when the TTL expires.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_work_blocks_hibernation() {
    let Some(python) = kernel_python() else {
        return;
    };
    let dir = tempfile::TempDir::new().unwrap();
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let provisioner = IpythonKernelProvisioner::new(
        dir.path(),
        IpythonKernelProvisionerOptions {
            python: Some(python),
            snapshot_dir: Some(artifacts),
            idle_hibernate_secs: Some(1),
            ..Default::default()
        },
    );
    let manager = provisioner.ensure(None, None).await.unwrap();
    let spawned = manager
        .execute(
            // Accessing `.pid` before any await makes it a background handle.
            "held = bash('sleep 20')\nheld.pid",
            ExecuteOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(spawned.status, ExecuteStatus::Ok);
    assert!(manager.has_background_work(), "the bash handle is live");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        provisioner.has_running_kernel(),
        "a live background bash() handle keeps the kernel resident past the TTL"
    );
    assert!(provisioner.manager().is_some());
    manager.kill();
    provisioner.dispose(None).await;
}

/// `dispose()` racing an armed hibernation leaves no kernel process behind:
/// whichever of the two tears the kernel down, the other finds it gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispose_racing_hibernation_leaves_no_kernel_process() {
    let Some(python) = kernel_python() else {
        return;
    };
    let dir = tempfile::TempDir::new().unwrap();
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir_all(&artifacts).unwrap();
    let provisioner = IpythonKernelProvisioner::new(
        dir.path(),
        IpythonKernelProvisionerOptions {
            python: Some(python),
            snapshot_dir: Some(artifacts),
            idle_hibernate_secs: Some(1),
            ..Default::default()
        },
    );
    let manager = provisioner.ensure(None, None).await.unwrap();
    let pid = manager.process_id().expect("a running kernel has a pid");
    // The dispose lands at an arbitrary point of the 1s arm: the assertion
    // below is about the end state, not about which side won.
    tokio::time::sleep(Duration::from_millis(500)).await;
    provisioner.dispose(None).await;
    assert!(
        provisioner.manager().is_none(),
        "a disposed provisioner serves no kernel"
    );
    assert!(!manager.is_running(), "the kernel is shut down");
    wait_until("the disposed kernel process to exit", || {
        !process_exists(pid)
    })
    .await;
}

/// Whether a process is still alive, cross-platform.
fn process_exists(pid: i32) -> bool {
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&format!(",\"{pid}\",")))
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }
}

/// An ephemeral session (no snapshot dir) never hibernates: there is
/// nothing to revive into.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ephemeral_session_never_hibernates() {
    let Some(python) = kernel_python() else {
        return;
    };
    let dir = tempfile::TempDir::new().unwrap();
    let provisioner = IpythonKernelProvisioner::new(
        dir.path(),
        IpythonKernelProvisionerOptions {
            python: Some(python),
            snapshot_dir: None,
            idle_hibernate_secs: Some(1),
            ..Default::default()
        },
    );
    let manager = provisioner.ensure(None, None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(provisioner.manager().is_some());
    manager.kill();
    provisioner.dispose(None).await;
}
