// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Device-lost recovery + degradation policy (spec §4.4/§6; tasks **E1**–**E8**).
//!
//! Owner: **E**. Detection → rebuild → re-warm → (on recurrence within the
//! window) degrade to preview-resolution CPU editing (the §4.4
//! degraded-but-usable contract). Explicit re-enable via prefs (E08).

use crate::ng::config::DegradePolicy;

/// The engine's current backend-availability state (spec §4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DegradeState {
    /// GPU rendering (the normal path).
    Gpu,
    /// Degraded to preview-resolution CPU editing after repeated device loss.
    CpuPreviewOnly,
}

/// Tracks device-loss events against a [`DegradePolicy`] and drives the
/// rebuild/degrade transitions (spec §4.4). **E fills the internals.**
#[derive(Default)]
pub struct RecoverStateMachine {}

impl RecoverStateMachine {
    /// A recovery machine governed by `policy`.
    pub fn new(policy: DegradePolicy) -> RecoverStateMachine {
        let _ = policy;
        unimplemented!("E4 (E): RecoverStateMachine::new — degradation policy")
    }

    /// Record a device-loss event; returns the resulting [`DegradeState`]
    /// (`max_losses` within `window` ⇒ `CpuPreviewOnly`; task E4).
    pub fn on_device_lost(&self, reason: &str) -> DegradeState {
        let _ = reason;
        unimplemented!("E1/E4 (E): RecoverStateMachine::on_device_lost")
    }

    /// The current state.
    pub fn state(&self) -> DegradeState {
        unimplemented!("E4 (E): RecoverStateMachine::state")
    }

    /// Explicitly re-enable the GPU path (prefs, E08; task E4).
    pub fn reenable_gpu(&self) {
        unimplemented!("E4 (E): RecoverStateMachine::reenable_gpu")
    }
}
