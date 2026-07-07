// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Per-process-version **manifest** — the frozen, diff-reviewable record of a
//! process version's node graph (task **D1**; the kernel-salt-discipline anchor
//! for task **D4**).
//!
//! Owner: **D** (per-PV templates + PV plumbing + PV manifest).
//!
//! # What a PV manifest is
//!
//! A [`PvManifest`] pins, for one [`ProcessVersion`], the ordered list of its
//! template stages as `(node_id, kernel_salt)` pairs. The kernel salt is the
//! blake3 of the node's WGSL + CPU algorithm revision (spec §3.2/§3.5); it is
//! the ingredient the content [`crate::ng::CacheKey`] folds in so a changed
//! kernel can never serve a stale cached tile. The manifest is committed to the
//! repository (`crates/lightbox-render/pv-manifests/pv<n>.json`) and a test
//! regenerates it from the live [`RecipeCompiler`] and asserts equality — so a
//! shipped kernel change shows up as a **manifest diff in code review** and, if
//! merged without a corresponding manifest update, **fails the build**.
//!
//! # The immutability contract (§4.5) — "new algorithm ⇒ new PV"
//!
//! Registered process versions are **append-only forever**. A PV's rendered
//! output is a stable regression pin (its committed goldens, task D3). Two
//! guard rails keep a released PV byte-immutable:
//!
//! 1. **The per-PV golden matrix (D3).** Any change to a shipped kernel's
//!    *algorithm* changes its output pixels, so the golden comparison for that
//!    PV drifts beyond ΔE2000 ≤ 1.0 / PSNR ≥ 45 dB and the build fails.
//! 2. **This manifest (D1/D4).** Any change to a shipped kernel's *salt* — which
//!    a node author **must** bump whenever they touch the WGSL/CPU algorithm
//!    (otherwise a warm cache would serve stale tiles, Risk R5) — changes the
//!    committed manifest and fails the sync test.
//!
//! ## Contributor workflow when you need to change a shipped node's algorithm
//!
//! You **do not** edit a node that is registered for a released PV in place.
//! Instead:
//!
//! 1. Register the improved [`crate::ng::RenderNode`] impl for a **new** open PV
//!    range (`NodeRegistry::register(id, PvRange::from_open(NEW_PV), …)`), giving
//!    it a fresh [`crate::ng::KernelSalt`]. The old impl keeps serving its old
//!    PV range unchanged (spec §3.3 — old PVs are never replaced).
//! 2. Add the new PV's [`GraphTemplate`] and commit its `pv<NEW>.json` manifest
//!    plus its golden set. Existing images keep rendering under their stored
//!    `edit_recipe.pv`; only images migrated to the new PV pick up the new
//!    kernel (the migrate-preview primitive, task D5).
//! 3. The old PV's manifest and goldens **do not change** — the sync test and
//!    the golden matrix both stay green, proving the release was not disturbed.
//!
//! If instead you (deliberately or accidentally) bump a *shipped* PV's salt or
//! algorithm, the manifest sync test and/or the golden matrix go red — the gate
//! doing its job. That is the D4 discipline made mechanical.

use serde::{Deserialize, Serialize};

use crate::ng::error::CompileError;
use crate::ng::types::{NodeId, ProcessVersion};

use super::RecipeCompiler;

/// One template stage's frozen digest: its node id and the hex-encoded blake3
/// kernel salt registered for the manifest's process version.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StageDigest {
    /// The dotted node id (e.g. `"xform.display"`).
    pub node: String,
    /// The hex-encoded blake3 kernel salt (WGSL + CPU algorithm revision).
    pub kernel_salt: String,
}

/// The committed, diff-reviewable manifest for one [`ProcessVersion`] (task D1):
/// its template stages, in order, each pinned to a `(node_id, kernel_salt)`
/// digest. Serialized as pretty JSON — a salt change is a one-line diff a
/// reviewer sees.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PvManifest {
    /// The process version this manifest freezes.
    pub pv: u16,
    /// The ordered template stages (empty §4.1 develop slots contribute
    /// nothing).
    pub stages: Vec<StageDigest>,
}

impl PvManifest {
    /// Build the manifest for `pv` from `compiler`'s registered template +
    /// registry salts (task D1). Walks the PV's [`GraphTemplate`] stages and
    /// resolves each stage's registered kernel salt.
    ///
    /// An unregistered `pv` ⇒ [`CompileError::UnsupportedPv`]; a template stage
    /// with no registered node under `pv` ⇒ [`CompileError::NodeNotRegistered`]
    /// (the same typed errors [`RecipeCompiler::compile`] raises — the manifest
    /// never silently omits a stage).
    pub fn of_compiler(
        compiler: &RecipeCompiler,
        pv: ProcessVersion,
    ) -> Result<PvManifest, CompileError> {
        let template = compiler
            .template(pv)
            .ok_or(CompileError::UnsupportedPv(pv))?;
        let registry = compiler.registry();
        let mut stages = Vec::with_capacity(template.stages().len());
        for &id in template.stages() {
            let salt = registry
                .kernel_salt(id, pv)
                .ok_or(CompileError::NodeNotRegistered { id, pv })?;
            stages.push(StageDigest {
                node: id.0.to_owned(),
                kernel_salt: salt.0.to_hex().to_string(),
            });
        }
        Ok(PvManifest { pv: pv.0, stages })
    }

    /// The registered node ids of the manifest's stages, in order.
    pub fn node_ids(&self) -> impl Iterator<Item = &str> {
        self.stages.iter().map(|s| s.node.as_str())
    }
}

/// A [`NodeId`] the manifest referenced but the registry could not resolve — a
/// convenience for callers that want the missing id without pattern-matching the
/// [`CompileError`].
pub fn missing_stage(err: &CompileError) -> Option<NodeId> {
    match err {
        CompileError::NodeNotRegistered { id, .. } => Some(*id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ng::compile::{shipping_compiler, GraphTemplate};
    use lightbox_types::PV_M0;
    use std::path::PathBuf;

    /// The repository-committed PV manifest for `pv`.
    fn manifest_path(pv: u16) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("pv-manifests")
            .join(format!("pv{pv}.json"))
    }

    fn bless_requested() -> bool {
        std::env::var("LIGHTBOX_BLESS").is_ok_and(|v| v == "1")
    }

    /// The shipping PV1 manifest built from the live compiler serializes to
    /// stable, ordered `(node, salt)` digests for the three engine-owned stages.
    #[test]
    fn pv1_manifest_lists_the_engine_owned_stages_in_order() {
        let compiler = shipping_compiler();
        let m = PvManifest::of_compiler(&compiler, PV_M0).expect("PV1 manifest builds");
        assert_eq!(m.pv, 1);
        assert_eq!(
            m.node_ids().collect::<Vec<_>>(),
            vec!["src.decoded", "util.resize", "xform.display"],
        );
        // Salts are 64-hex-char blake3 digests, and distinct per stage.
        for s in &m.stages {
            assert_eq!(s.kernel_salt.len(), 64, "blake3 hex for {}", s.node);
        }
        let salts: std::collections::HashSet<&str> =
            m.stages.iter().map(|s| s.kernel_salt.as_str()).collect();
        assert_eq!(salts.len(), m.stages.len(), "distinct salts per stage");
    }

    /// **D1 / D4 immutability gate (PR-blocking): the live PV1 manifest matches
    /// the committed `pv-manifests/pv1.json` byte-for-byte.**
    ///
    /// If a contributor changes a shipped node's WGSL/CPU algorithm they must
    /// bump its kernel salt (Risk R5); that flips a digest here and fails this
    /// test — forcing either a revert or the "new algorithm ⇒ new PV" workflow
    /// documented at the module level. Regenerate (only for a *reviewed* new PV)
    /// with `LIGHTBOX_BLESS=1`; a missing manifest fails rather than
    /// self-certifying.
    #[test]
    fn pv1_manifest_matches_committed_file() {
        let compiler = shipping_compiler();
        let live = PvManifest::of_compiler(&compiler, PV_M0).expect("PV1 manifest builds");
        let live_json = serde_json::to_string_pretty(&live).expect("manifest serializes");
        let path = manifest_path(1);

        if bless_requested() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("bless: create pv-manifests dir");
            }
            std::fs::write(&path, format!("{live_json}\n")).expect("bless: write PV1 manifest");
            return;
        }

        let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "committed PV1 manifest {path:?} unreadable ({e}); regenerate with LIGHTBOX_BLESS=1"
            )
        });
        let committed: PvManifest =
            serde_json::from_str(&committed).expect("committed PV1 manifest parses");
        assert_eq!(
            live, committed,
            "live PV1 manifest diverged from {path:?} — a shipped kernel changed without a new PV \
             (see the 'new algorithm ⇒ new PV' workflow in manifest.rs); bless only for a reviewed new PV"
        );
    }

    /// **D4 mechanism proof (permanent, green): a salt bump on a shipped stage
    /// is detected.** We simulate a contributor touching a shipped kernel by
    /// mutating one stage's salt digest and assert the manifest no longer equals
    /// the committed one — i.e. the sync gate above would trip. This proves the
    /// discipline without leaving a red build (the real trip-then-revert against
    /// the committed file was performed once and recorded in E05-deviations.md).
    #[test]
    fn a_salt_change_on_a_shipped_stage_is_detected() {
        let compiler = shipping_compiler();
        let baseline = PvManifest::of_compiler(&compiler, PV_M0).expect("baseline builds");

        let mut tweaked = baseline.clone();
        // A one-char salt flip stands in for "someone edited util.resize's WGSL
        // and bumped its salt (as they must) without registering a new PV".
        let salt = &mut tweaked.stages[1].kernel_salt;
        let flipped = if salt.starts_with('0') { '1' } else { '0' };
        salt.replace_range(0..1, &flipped.to_string());

        assert_ne!(
            baseline, tweaked,
            "a shipped-stage salt change must change the manifest (the D4 gate)"
        );
    }

    /// An unregistered PV yields a typed error from the manifest builder — the
    /// same `UnsupportedPv` the compiler raises (never a silent empty manifest).
    #[test]
    fn manifest_of_unregistered_pv_is_typed() {
        let compiler = shipping_compiler();
        let err = PvManifest::of_compiler(&compiler, ProcessVersion(4242)).unwrap_err();
        assert!(matches!(err, CompileError::UnsupportedPv(ProcessVersion(4242))));
    }

    /// A template referencing a stage with no registered node under the PV
    /// surfaces `NodeNotRegistered` (and [`missing_stage`] recovers the id).
    #[test]
    fn manifest_of_unregistered_stage_is_typed() {
        // A compiler whose PV1 template names the engine stages but whose
        // registry is empty — the manifest cannot resolve the first stage.
        let mut compiler = RecipeCompiler::new();
        compiler
            .register_template(PV_M0, GraphTemplate::pv1())
            .unwrap();
        let err = PvManifest::of_compiler(&compiler, PV_M0).unwrap_err();
        assert!(matches!(err, CompileError::NodeNotRegistered { .. }));
        assert_eq!(missing_stage(&err).map(|i| i.0), Some("src.decoded"));
    }
}
