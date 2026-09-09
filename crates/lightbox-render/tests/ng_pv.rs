// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E05 Phase D, **process-version plumbing end-to-end** (E05.4, tasks D1/D2/D5),
//! at the `Engine` level on the CPU reference path.
//!
//! Proves that one recipe renders under two process versions in a single engine
//! session, the migrate-preview primitive E09/E10 build on (task D5), with:
//!
//! * `Engine::supported_pvs()` reflecting the compiler's registered set (D1);
//! * per-PV template *selection* (PV1 = `src.decoded → util.resize →
//!   xform.display`, PV999 = the divergent `src.decoded → xform.display`, task D2);
//! * correct `RenderOutput.pv` provenance on each output;
//! * **independent caching**, the content key folds in `pv` (spec §3.5), so
//!   PV1's warm cache never satisfies a PV999 request (no cross-PV pollution);
//! * an unregistered PV failing *typed* (never a silent fallback to latest).
//!
//! Runs entirely on the CPU backend (`ForceCpu`) over a synthetic
//! `SourceProvider`, so it needs no GPU adapter and is deterministic headless.

use std::sync::Arc;

use lightbox_jobs::CancelToken;
use lightbox_render::ng::source::DeviceHandles;
use lightbox_render::ng::{
    shipping_registry, BackendId, BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine,
    EngineConfig, Extent, GraphTemplate, OutFormat, OutputPayload, RecipeCompiler, RenderError,
    RenderPriority, RenderRequest, RenderScale, RenderState, RenderTarget, RenderTicket,
    SourceColorimetry, SourceError, SourceImage, SourceProvider, SourceQuality, SourceWant,
    PV_TEST_999,
};
use lightbox_render_testkit::corpus::{synth_source, CorpusKind};
use lightbox_types::{ImageId, ProcessVersion, PV_M0};

// ── seams ───────────────────────────────────────────────────────────────────

/// A `DeviceProvider` that is never asked for handles (the `ForceCpu` path).
struct NullDevice;
impl DeviceProvider for NullDevice {
    fn current(&self) -> DeviceHandles {
        unimplemented!("ForceCpu never asks the device for handles")
    }
    fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
        Box::pin(async { Err(DeviceError::Rebuild("no device in test".to_owned())) })
    }
}

/// A `SourceProvider` returning a fixed synthetic gradient (E02 seam).
struct SynthSource {
    extent: Extent,
}
impl SourceProvider for SynthSource {
    fn fetch(
        &self,
        _: ImageId,
        _: SourceWant,
        _: &CancelToken,
    ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
        let pixels = synth_source(CorpusKind::Gradient, self.extent.w, self.extent.h);
        Box::pin(async move {
            let full_extent = pixels.extent;
            Ok(SourceImage {
                pixels,
                colorimetry: SourceColorimetry::default(),
                full_extent,
                quality: SourceQuality::Full,
            })
        })
    }
}

const EXTENT: Extent = Extent { w: 64, h: 64 };

/// A CPU engine whose compiler holds both the shipping PV1 template and the
/// test-only divergent PV999 template, resolving against the shipping node set.
fn two_pv_engine() -> Engine {
    let mut compiler = RecipeCompiler::with_registry(Arc::new(shipping_registry()));
    compiler
        .register_template(PV_M0, GraphTemplate::pv1())
        .unwrap();
    compiler
        .register_template(PV_TEST_999, GraphTemplate::pv_test_999())
        .unwrap();
    Engine::with_compiler(
        Arc::new(NullDevice),
        Arc::new(SynthSource { extent: EXTENT }),
        compiler,
        EngineConfig {
            backend: BackendPref::ForceCpu,
            ..EngineConfig::default()
        },
    )
    .expect("engine builds on the CPU path")
}

fn request(pv: ProcessVersion) -> RenderRequest {
    RenderRequest {
        image: ImageId(1),
        recipe: lightbox_edit::Recipe::identity(PV_M0), // the *same* recipe under both PVs
        pv,
        roi: lightbox_render::ng::Roi {
            x: 0,
            y: 0,
            w: EXTENT.w,
            h: EXTENT.h,
        },
        scale: RenderScale::OneToOne,
        target: RenderTarget::Buffer {
            format: OutFormat::Rgba8Srgb,
        },
        priority: RenderPriority::Interactive,
        cancel: CancelToken::new(),
    }
}

fn render(engine: &Engine, pv: ProcessVersion) -> lightbox_render::ng::RenderOutput {
    let ticket = engine.submit(request(pv));
    match poll_until(engine, &ticket, |s| {
        matches!(s, RenderState::Complete(_) | RenderState::Failed(_))
    }) {
        RenderState::Complete(out) => out,
        other => panic!("render under pv{} did not complete: {other:?}", pv.0),
    }
}

fn poll_until<F>(engine: &Engine, ticket: &RenderTicket, pred: F) -> RenderState
where
    F: Fn(&RenderState) -> bool,
{
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let state = engine.poll(ticket);
        if pred(&state) || std::time::Instant::now() > deadline {
            return state;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

/// **D5: one recipe renders under two PVs in one session, independently cached +
/// with correct provenance.** Also exercises D1 (`supported_pvs`) and D2 (per-PV
/// selection + no cross-PV cache pollution).
#[test]
fn one_recipe_renders_under_two_pvs_independently() {
    let engine = two_pv_engine();

    // D1: the engine reports exactly the registered PVs.
    assert_eq!(engine.supported_pvs(), vec![PV_M0, PV_TEST_999]);

    // Render under PV1 (cold), then again (warm), then under PV999.
    let out_pv1 = render(&engine, PV_M0);
    assert_eq!(out_pv1.pv, PV_M0, "PV1 provenance");
    assert_eq!(out_pv1.backend, BackendId::Cpu);
    let evals_pv1_cold = engine.stats().nodes_evaluated;
    assert!(evals_pv1_cold >= 1, "PV1 cold evaluated some nodes");

    // A warm PV1 re-render hits the cache: zero additional evaluations.
    let _ = render(&engine, PV_M0);
    let evals_pv1_warm = engine.stats().nodes_evaluated;
    assert_eq!(
        evals_pv1_warm, evals_pv1_cold,
        "warm PV1 re-render is fully cache-served (0 new evals)"
    );

    // PV999 is a *different* pv → different content keys → its nodes are
    // re-evaluated; PV1's warm cache does not pollute it.
    let out_pv999 = render(&engine, PV_TEST_999);
    assert_eq!(out_pv999.pv, PV_TEST_999, "PV999 provenance");
    let evals_pv999 = engine.stats().nodes_evaluated;
    assert!(
        evals_pv999 > evals_pv1_warm,
        "PV999 re-evaluated nodes (no cross-PV cache pollution): {evals_pv999} !> {evals_pv1_warm}"
    );

    // Both produced real pixels of the expected extent.
    for out in [&out_pv1, &out_pv999] {
        let OutputPayload::Pixels(px) = &out.payload else {
            panic!("expected Pixels payload");
        };
        assert_eq!(px.extent, EXTENT);
    }
}

/// D1: an unregistered PV fails *typed* via poll, never a silent fallback to
/// the latest template.
#[test]
fn unregistered_pv_fails_typed() {
    let engine = two_pv_engine();
    let ticket = engine.submit(request(ProcessVersion(4242)));
    let state = poll_until(&engine, &ticket, |s| matches!(s, RenderState::Failed(_)));
    assert!(
        matches!(
            state,
            RenderState::Failed(RenderError::Compile(
                lightbox_render::ng::CompileError::UnsupportedPv(ProcessVersion(4242))
            ))
        ),
        "expected UnsupportedPv, got {state:?}"
    );
}
