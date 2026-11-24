//! Idle kernel hibernation: a published kernel nobody has used for the idle
//! TTL flushes its namespace snapshot and exits; the next `ensure()` boots a
//! replacement gated on that flush and restores the snapshot.
//!
//! One watcher task per published kernel. It holds only weak references, so
//! it never keeps a provisioner or a kernel alive, and it stands down as soon
//! as its kernel is no longer the published one (a stop, kill, dispose, or a
//! defunct kernel replaced). The claim reads the activity stamp `ensure()`
//! writes under the same lock, so a caller handed the kernel is never
//! hibernated under it.

use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::time::Instant;

use super::ProvisionerInner;
use crate::kernel::cancellation::AbortSignal;
use crate::kernel::manager::{Inner as KernelInner, ReplKernelManager};
use crate::kernel::shared::KernelShutdownOptions;

/// The progress stage an `ensure()` reports when it joins an in-flight
/// hibernation, before the revival boot's own stages.
pub(super) const REVIVE_MESSAGE: &str = "Restarting the hibernated Python kernel...";

const IDLE_TTL_ENV: &str = "PRIME_AGENT_KERNEL_IDLE_TTL_SECS";

/// Idle TTL applied when neither the option nor the environment sets one:
/// long enough that a reading or thinking turn never looks idle, short
/// enough that an abandoned session releases its interpreter.
const DEFAULT_IDLE_TTL_SECS: u64 = 30 * 60;

/// Ceiling on a configured idle TTL (a day).
const MAX_IDLE_TTL_SECS: u64 = 24 * 60 * 60;

/// The idle TTL, or `None` when hibernation is disabled. A numeric
/// environment value wins over the option, the option over the default; `0`
/// disables, and anything longer than a day is clamped to a day.
fn idle_ttl(option: Option<u64>, env_value: Option<&str>) -> Option<Duration> {
    let secs = env_value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .or(option)
        .unwrap_or(DEFAULT_IDLE_TTL_SECS)
        .min(MAX_IDLE_TTL_SECS);
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// Start watching a just-published kernel for idleness. A no-op for an
/// ephemeral session (nothing to revive into) or a disabled TTL.
pub(super) fn watch_idle_kernel(inner: &Arc<ProvisionerInner>, manager: &ReplKernelManager) {
    if inner.options.snapshot_dir.is_none() {
        return;
    }
    let env_value = std::env::var(IDLE_TTL_ENV).ok();
    let Some(ttl) = idle_ttl(inner.options.idle_hibernate_secs, env_value.as_deref()) else {
        return;
    };
    tokio::spawn(hibernate_when_idle(
        Arc::downgrade(inner),
        Arc::downgrade(&manager.inner),
        ttl,
        inner.dispose_signal.clone(),
    ));
}

/// Sleep until the watched kernel has been unused for `ttl`, then flush and
/// shut it down. Returns early once the kernel is no longer the published
/// one or the provisioner is disposed.
async fn hibernate_when_idle(
    provisioner: Weak<ProvisionerInner>,
    kernel: Weak<KernelInner>,
    ttl: Duration,
    dispose: AbortSignal,
) {
    let (inner, manager, flushed, previous_stop) = loop {
        let deadline = {
            let Some(inner) = provisioner.upgrade() else {
                return;
            };
            let mut state = inner.lock_state();
            let watched = state
                .manager
                .as_ref()
                .filter(|live| std::ptr::eq(Arc::as_ptr(&live.inner), kernel.as_ptr()))
                .cloned();
            let Some(manager) = watched.filter(|_| !state.disposed) else {
                return;
            };
            let now = Instant::now();
            let deadline = state.last_activity + ttl;
            if deadline > now {
                deadline
            } else if !manager.is_running()
                || manager.background_bash_handle_count() > 0
                || manager.has_pending_host_requests()
                || inner.in_flight.load(std::sync::atomic::Ordering::SeqCst) > 0
            {
                // Busy work counts as use: the TTL restarts when it ends.
                state.last_activity = now;
                now + ttl
            } else {
                // Claim and arm the revival gate in ONE lock scope: an
                // `ensure()` after this boots a replacement that restores
                // only once this flush settled.
                let (flushed, gate) = tokio::sync::watch::channel(false);
                let previous_stop = state.pending_stop.replace(gate.clone());
                state.hibernating = Some(gate);
                state.manager = None;
                drop(state);
                break (inner, manager, flushed, previous_stop);
            }
        };
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {}
            () = dispose.cancelled() => return,
        }
    };
    // `only_when_idle` refuses (and leaves the kernel untouched) when a cell
    // raced the claim through a manager handle taken before it.
    let hibernated = manager
        .shutdown(KernelShutdownOptions {
            snapshot: true,
            drain_host_requests: true,
            only_when_idle: true,
        })
        .await
        .unwrap_or(false);
    if !hibernated && manager.is_running() {
        let hand_back = {
            let mut state = inner.lock_state();
            let hand_back = state.manager.is_none() && state.startup.is_none() && !state.disposed;
            if hand_back {
                state.manager = Some(manager.clone());
                state.last_activity = Instant::now();
            }
            hand_back
        };
        if hand_back {
            watch_idle_kernel(&inner, &manager);
        } else {
            // A replacement boot (or a dispose) already waits this gate:
            // the raced kernel must not outlive it.
            let snapshot = {
                let state = inner.lock_state();
                !state.disposed || state.dispose_snapshot
            };
            let _ = manager
                .shutdown(KernelShutdownOptions {
                    snapshot,
                    ..KernelShutdownOptions::default()
                })
                .await;
        }
    }
    // Chain like `stop_kernel()`: open this gate only after the stop gate it
    // superseded, so a revival never crosses an older stop's flush.
    if let Some(mut previous) = previous_stop {
        let _ = previous.wait_for(|done| *done).await;
    }
    {
        let mut state = inner.lock_state();
        if state
            .hibernating
            .as_ref()
            .is_some_and(|gate| gate.same_channel(&flushed.subscribe()))
        {
            state.hibernating = None;
        }
    }
    let _ = flushed.send(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_ttl_prefers_the_environment_then_the_option_then_the_default() {
        let resolved: Vec<Option<Duration>> = [
            (None, None),
            (Some(90), None),
            (Some(0), None),
            (Some(90), Some(" 5 ")),
            (Some(90), Some("0")),
            (None, Some("soon")),
            (Some(u64::MAX), None),
        ]
        .into_iter()
        .map(|(option, env_value)| idle_ttl(option, env_value))
        .collect();
        assert_eq!(
            resolved,
            vec![
                Some(Duration::from_secs(DEFAULT_IDLE_TTL_SECS)),
                Some(Duration::from_secs(90)),
                None,
                Some(Duration::from_secs(5)),
                None,
                Some(Duration::from_secs(DEFAULT_IDLE_TTL_SECS)),
                Some(Duration::from_secs(MAX_IDLE_TTL_SECS)),
            ]
        );
    }
}
