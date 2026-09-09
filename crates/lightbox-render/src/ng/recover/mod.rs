// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Device-lost recovery + degradation policy (spec §4.4/§6; tasks **E1**–**E8**).
//!
//! Owner: **E**. Detection → rebuild → re-warm → (on recurrence within the
//! window) degrade to preview-resolution CPU editing (the §4.4
//! degraded-but-usable contract). Explicit re-enable via prefs (E08).
//!
//! This module owns the **policy core**: it counts device-loss events against a
//! [`DegradePolicy`] and reports whether the engine should keep trying the GPU
//! (rebuild) or degrade to CPU-preview. The engine ([`crate::ng::engine`]) wires
//! this to the wgpu `device_lost` callback + uncaptured-error hook, drives the
//! rebuild via the [`crate::ng::DeviceProvider`] seam, and enforces the degraded
//! contract; the state machine here is a pure, thread-safe counter so its
//! boundary behaviour (K−1 losses stays GPU, Kth degrades) is unit-testable
//! without a GPU.

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use crate::ng::config::DegradePolicy;

/// The engine's current backend-availability state (spec §4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DegradeState {
    /// GPU rendering (the normal path). A single loss within the window rebuilds
    /// and stays here.
    Gpu,
    /// Degraded to preview-resolution CPU editing after repeated device loss
    /// within the policy window (the §4.4 degraded-but-usable contract).
    CpuPreviewOnly,
}

/// Interior mutable loss-tracking state.
struct Inner {
    /// Loss timestamps still inside the policy window (older ones are pruned).
    losses: Vec<Instant>,
    /// The current degrade state (sticky once `CpuPreviewOnly` until re-enabled).
    state: DegradeState,
}

/// Tracks device-loss events against a [`DegradePolicy`] and drives the
/// rebuild/degrade decision (spec §4.4; tasks **E1**/**E4**).
///
/// `on_device_lost` is called once per detected loss. Losses are counted within
/// a sliding [`DegradePolicy::window`]; once `max_losses` accumulate the machine
/// latches to [`DegradeState::CpuPreviewOnly`] until [`Self::reenable_gpu`] is
/// called (the explicit prefs re-enable, E08). Thread-safe (`&self` + interior
/// `Mutex`) so the engine's device-lost callback, which may fire on any
/// thread, and the render worker can both consult it.
pub struct RecoverStateMachine {
    policy: DegradePolicy,
    inner: Mutex<Inner>,
}

impl RecoverStateMachine {
    /// A recovery machine governed by `policy`.
    pub fn new(policy: DegradePolicy) -> RecoverStateMachine {
        RecoverStateMachine {
            policy,
            inner: Mutex::new(Inner {
                losses: Vec::new(),
                state: DegradeState::Gpu,
            }),
        }
    }

    /// Record a device-loss event; returns the resulting [`DegradeState`].
    ///
    /// `max_losses` accumulated within `window` ⇒ [`DegradeState::CpuPreviewOnly`]
    /// (task E4). Boundary: with `max_losses = K`, the `(K−1)`-th loss stays
    /// [`DegradeState::Gpu`] and the `K`-th degrades.
    pub fn on_device_lost(&self, reason: &str) -> DegradeState {
        self.record_at(Instant::now(), reason)
    }

    /// The current state.
    pub fn state(&self) -> DegradeState {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .state
    }

    /// Explicitly re-enable the GPU path (prefs, E08; task E4): clears the loss
    /// window and returns to [`DegradeState::Gpu`]. The engine follows this by
    /// rebuilding the GPU backend.
    pub fn reenable_gpu(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.losses.clear();
        inner.state = DegradeState::Gpu;
    }

    /// Losses currently counted inside the window (diagnostics/tests).
    pub fn losses_in_window(&self) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let window = self.policy.window;
        let now = Instant::now();
        inner.losses.retain(|t| now.duration_since(*t) <= window);
        inner.losses.len()
    }

    /// The configured degradation policy.
    pub fn policy(&self) -> DegradePolicy {
        self.policy
    }

    /// Record a loss at an explicit instant (the deterministic core of
    /// [`Self::on_device_lost`]; exercised directly by the window-pruning tests).
    fn record_at(&self, now: Instant, reason: &str) -> DegradeState {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let window = self.policy.window;
        // Prune losses that have aged out of the window, then record this one.
        inner.losses.retain(|t| now.duration_since(*t) <= window);
        inner.losses.push(now);
        let count = inner.losses.len();
        // `max_losses` defaults to 2 (§3.6); guard against a 0 policy so a single
        // loss still degrades rather than never degrading.
        let threshold = usize::from(self.policy.max_losses.max(1));
        tracing::warn!(
            target: "lightbox_render::recover",
            losses = count,
            threshold,
            reason,
            "device-lost recorded"
        );
        if count >= threshold {
            inner.state = DegradeState::CpuPreviewOnly;
        }
        inner.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn policy(max_losses: u8, window_ms: u64) -> DegradePolicy {
        DegradePolicy {
            max_losses,
            window: Duration::from_millis(window_ms),
        }
    }

    #[test]
    fn boundary_k_minus_one_stays_gpu_kth_degrades() {
        // max_losses = 2 (the §3.6 default): first loss stays GPU, second degrades.
        let sm = RecoverStateMachine::new(policy(2, 60_000));
        assert_eq!(sm.state(), DegradeState::Gpu);
        assert_eq!(
            sm.on_device_lost("loss 1"),
            DegradeState::Gpu,
            "K-1 stays GPU"
        );
        assert_eq!(sm.state(), DegradeState::Gpu);
        assert_eq!(
            sm.on_device_lost("loss 2"),
            DegradeState::CpuPreviewOnly,
            "Kth degrades"
        );
        assert_eq!(sm.state(), DegradeState::CpuPreviewOnly);
    }

    #[test]
    fn max_losses_one_degrades_on_first_loss() {
        let sm = RecoverStateMachine::new(policy(1, 60_000));
        assert_eq!(sm.on_device_lost("loss"), DegradeState::CpuPreviewOnly);
    }

    #[test]
    fn max_losses_three_needs_three() {
        let sm = RecoverStateMachine::new(policy(3, 60_000));
        assert_eq!(sm.on_device_lost("1"), DegradeState::Gpu);
        assert_eq!(sm.on_device_lost("2"), DegradeState::Gpu);
        assert_eq!(sm.on_device_lost("3"), DegradeState::CpuPreviewOnly);
    }

    #[test]
    fn reenable_restores_gpu() {
        let sm = RecoverStateMachine::new(policy(1, 60_000));
        assert_eq!(sm.on_device_lost("loss"), DegradeState::CpuPreviewOnly);
        sm.reenable_gpu();
        assert_eq!(sm.state(), DegradeState::Gpu);
        assert_eq!(sm.losses_in_window(), 0);
        // After re-enable the count restarts: one more loss stays GPU (max 2 here
        // would need two, but this policy is max 1 → degrades again).
        assert_eq!(
            sm.on_device_lost("loss again"),
            DegradeState::CpuPreviewOnly
        );
    }

    #[test]
    fn losses_outside_the_window_do_not_accumulate() {
        // A short window: the first loss ages out before the second, so the
        // second is counted alone and (max_losses = 2) stays on GPU.
        let sm = RecoverStateMachine::new(policy(2, 20));
        assert_eq!(sm.on_device_lost("old"), DegradeState::Gpu);
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(
            sm.on_device_lost("new"),
            DegradeState::Gpu,
            "the aged-out first loss must not push us over the threshold"
        );
        assert_eq!(sm.losses_in_window(), 1);
    }

    #[test]
    fn deterministic_window_pruning_via_record_at() {
        let sm = RecoverStateMachine::new(policy(2, 100));
        let t0 = Instant::now();
        // Two losses far apart relative to a 100 ms window: the first is pruned.
        assert_eq!(sm.record_at(t0, "1"), DegradeState::Gpu);
        assert_eq!(
            sm.record_at(t0 + Duration::from_millis(250), "2"),
            DegradeState::Gpu
        );
        // Two losses close together: both count → degrade.
        assert_eq!(
            sm.record_at(t0 + Duration::from_millis(260), "3"),
            DegradeState::CpuPreviewOnly
        );
    }
}
