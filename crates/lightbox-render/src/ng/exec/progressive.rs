// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The progressive ladder — `BestAvailable` tier → `PreviewReady` → preview-res
//! → full-res on idle (spec §4.3; task **C6**).
//!
//! Owner: **C**. First paint must be fast: the ladder shows the best already-
//! available source tier passed through the display transform only (the
//! `PreviewReady`/`PreviewTier` badge) before the full pipeline has run, then
//! upgrades to a preview-resolution render, then to full resolution once the UI
//! goes idle. This module sequences those tiers, emitting the [`RenderState`]
//! transitions the shell samples and quality-badges — the C6 gate asserts the
//! three-stage sequence, and the harness records the first-`PreviewReady`
//! wall-clock (the ≤ 100 ms warm-store target is measured on the reference
//! machine; local actuals are indicative — see `E05-deviations.md`).

use std::time::{Duration, Instant};

use lightbox_jobs::CancelToken;

use crate::ng::engine::{OutputQuality, RenderOutput, RenderState};
use crate::ng::error::RenderError;

/// One rung of the ladder: a closure producing a [`RenderOutput`] at a given
/// fidelity tier. Boxed so tiers can capture distinct render setups.
pub type TierFn<'a> = Box<dyn FnOnce() -> Result<RenderOutput, RenderError> + 'a>;

/// The three ladder rungs (spec §4.3): a display-only preview over the best
/// available source tier, a preview-resolution render, and the full-resolution
/// render.
pub struct Ladder<'a> {
    /// Display-transform-only over the `BestAvailable` source tier (fastest).
    pub preview_tier: TierFn<'a>,
    /// Full pipeline at preview/fit resolution.
    pub preview_res: TierFn<'a>,
    /// Full pipeline at full resolution (runs on idle).
    pub full_res: TierFn<'a>,
}

/// The outcome of running the ladder (task C6).
#[derive(Clone, Debug)]
pub struct LadderReport {
    /// The fidelity tiers emitted, in order (the C6 three-stage sequence).
    pub qualities: Vec<OutputQuality>,
    /// Wall-clock from ladder start to the first `PreviewReady` (indicative;
    /// the ≤ 100 ms gate of record is on the reference machine).
    pub first_preview: Duration,
    /// Whether the ladder ran to the full-resolution rung (vs cancelled early).
    pub completed: bool,
}

impl<'a> Ladder<'a> {
    /// Run the ladder, calling `on_state` for every emitted [`RenderState`]
    /// transition and returning the tier sequence + first-preview latency
    /// (task C6). Cancellation between rungs stops the ladder early (a newer
    /// edit supersedes an in-flight full-res upgrade).
    pub fn run(
        self,
        cancel: &CancelToken,
        on_state: &mut dyn FnMut(RenderState),
    ) -> Result<LadderReport, RenderError> {
        let start = Instant::now();
        on_state(RenderState::Rendering {
            tiles_done: 0,
            tiles_total: 1,
        });

        let mut qualities = Vec::with_capacity(3);

        // Rung 1: display-only preview over the best available tier.
        let tier = force_quality((self.preview_tier)()?, OutputQuality::PreviewTier);
        let first_preview = start.elapsed();
        qualities.push(tier.quality);
        on_state(RenderState::PreviewReady(tier));

        if cancel.is_cancelled() {
            return Ok(LadderReport {
                qualities,
                first_preview,
                completed: false,
            });
        }

        // Rung 2: preview-resolution render.
        let pr = force_quality((self.preview_res)()?, OutputQuality::PreviewRes);
        qualities.push(pr.quality);
        on_state(RenderState::PreviewReady(pr));

        if cancel.is_cancelled() {
            return Ok(LadderReport {
                qualities,
                first_preview,
                completed: false,
            });
        }

        // Rung 3: full-resolution render (on idle).
        let full = force_quality((self.full_res)()?, OutputQuality::FullRes);
        qualities.push(full.quality);
        on_state(RenderState::Complete(full));

        Ok(LadderReport {
            qualities,
            first_preview,
            completed: true,
        })
    }
}

/// Stamp the tier's fidelity badge (the ladder owns the quality, not the tier
/// closure — the closure just renders pixels).
fn force_quality(mut out: RenderOutput, quality: OutputQuality) -> RenderOutput {
    out.quality = quality;
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::colorimetry::OutputColorimetry;
    use crate::ng::engine::{BackendId, OutputPayload};
    use crate::ng::tile::{PixelBuf, PixelFormat};
    use crate::ng::types::Extent;
    use lightbox_types::PV_M0;

    fn fake_output() -> RenderOutput {
        RenderOutput {
            payload: OutputPayload::Pixels(PixelBuf::new_zeroed(
                PixelFormat::Rgba8Unorm,
                Extent { w: 2, h: 2 },
            )),
            colorimetry: OutputColorimetry::default(),
            backend: BackendId::Cpu,
            pv: PV_M0,
            quality: OutputQuality::FullRes,
        }
    }

    /// **C6 gate:** the ladder emits exactly the three-stage RenderState
    /// sequence PreviewReady(PreviewTier) → PreviewReady(PreviewRes) →
    /// Complete(FullRes).
    #[test]
    fn ladder_emits_three_stage_sequence() {
        let ladder = Ladder {
            preview_tier: Box::new(|| Ok(fake_output())),
            preview_res: Box::new(|| Ok(fake_output())),
            full_res: Box::new(|| Ok(fake_output())),
        };
        let cancel = CancelToken::new();
        let mut states = Vec::new();
        let report = ladder
            .run(&cancel, &mut |s| states.push(s))
            .expect("ladder runs");

        assert_eq!(
            report.qualities,
            vec![
                OutputQuality::PreviewTier,
                OutputQuality::PreviewRes,
                OutputQuality::FullRes
            ]
        );
        assert!(report.completed);

        // The emitted RenderState sequence: Rendering, PreviewReady, PreviewReady,
        // Complete — with matching quality badges.
        assert!(matches!(states[0], RenderState::Rendering { .. }));
        assert!(matches!(
            states[1],
            RenderState::PreviewReady(RenderOutput {
                quality: OutputQuality::PreviewTier,
                ..
            })
        ));
        assert!(matches!(
            states[2],
            RenderState::PreviewReady(RenderOutput {
                quality: OutputQuality::PreviewRes,
                ..
            })
        ));
        assert!(matches!(
            states[3],
            RenderState::Complete(RenderOutput {
                quality: OutputQuality::FullRes,
                ..
            })
        ));
    }

    #[test]
    fn cancel_between_rungs_stops_before_full_res() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let ladder = Ladder {
            preview_tier: Box::new(|| Ok(fake_output())),
            preview_res: Box::new(|| panic!("preview-res must not run after cancel")),
            full_res: Box::new(|| panic!("full-res must not run after cancel")),
        };
        let mut states = Vec::new();
        let report = ladder.run(&cancel, &mut |s| states.push(s)).unwrap();
        assert!(!report.completed);
        assert_eq!(report.qualities, vec![OutputQuality::PreviewTier]);
    }
}
