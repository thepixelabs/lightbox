// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E08 Phase E, the develop-panel framework + M1 panel slice (spec §6.5).
//!
//! Module map (spec §3 crate map):
//! * [`host`], E1: `PanelDef` registry + the dock renderer
//!   (collapsible/solo/scroll, multi-column rails, header drag-and-drop)
//!   with E2's §4 `SourceKind` filtering.
//! * [`layout`], the pure dock-layout model behind it: which panel sits
//!   in which rail and column, column widths, and the `prefs.toml`
//!   round trip.
//! * [`develop_ctx`], E3: the `EditBinding` adapter over E09's real edit
//!   session (`SessionEditBinding`, which is also the canvas's Phase-C
//!   `RecipeSource`) and `DevelopCtx` (whose gizmo-layer field is Phase
//!   F's tool-mounting seam, E6's interim `EyedropperMount` is gone).
//! * [`widgets`], E4: `param_slider` (+ the reusable `value_slider` core).
//! * [`basic`], E5/E6: the `develop.basic` WB + tone panel (also hosts
//!   E10 task D7's Presence section: clarity/texture/dehaze).
//! * [`curve`], E7: the `develop.curve` point tone-curve widget (math
//!   only).
//! * [`history`], E8: the `develop.history` history & snapshots panel.
//! * [`hsl`], E10 task C9: the `develop.hsl` 8-band HSL color mixer panel.
//! * [`grading`], E10 tasks C13: the `develop.grading` 3-way color-grading
//!   wheels panel.
//! * [`detail`], the `develop.detail` sharpening + noise-reduction panel,
//!   over `ParamId::Sharpen`/`ParamId::NoiseReduction`.
//! * [`bw`], E10 task D2: the `develop.bw` Treatment toggle + 8-band B&W
//!   mixer panel; also owns [`bw::is_monochrome`], the predicate `hsl`/
//!   `grading` check to grey themselves out in Monochrome.
//! * [`presets`], the preset-management panel: browse/apply/create/rename/
//!   delete/export/import over E09's `PresetStore`, plus a pure hover-only
//!   canvas preview.
//! * [`looks`], E10 tasks D10/D11: the `develop.looks` creative-look
//!   browser, install/browse (family-grouped)/apply/hover-preview an
//!   `installed_look` pack, an amount slider, and a missing-look badge.
//! * [`effects`], the `develop.effects` panel: post-crop vignetting and
//!   grain, the rail half of `lightbox_render`'s `fx.vignette` /
//!   `fx.grain` nodes (both leaves already round-tripped through the
//!   recipe and the `crs:` XMP mapping before either could render).
//! * [`optics`], the `develop.optics` panel: lens distortion and vignetting
//!   correction, chromatic aberration, and defringe, over
//!   `ParamId::{LensProfile, ChromaticAberration, Defringe, VignetteCorr}`.
//! * [`geometry`], E11 geometry TOOL UI: the `develop.geometry` panel
//!   (crop tool + aspect presets + straighten + flip + reset), driving
//!   `canvas::crop_gizmo` and the E11 engine's `ParamId::{Crop,Angle,Flip}`.
//! * [`histogram`], E10 task D13: the `develop.histogram` RGB-overlay
//!   histogram + clip-indicator triangles, fed by D12's live `HistogramPass`
//!   via `EditBinding::histogram`.
//!
//! **E9 (`info.rs`, the EXIF info panel) is the phase's named CUT-LINE**
//! (spec §8), deliberately not built; see `E08-deviations.md` Phase E.
//!
//! **How E10/E11/E12 mount a panel:** register a [`PanelDef`] on the app's
//! [`PanelHost`] (order slots between the E08 panels), drive scalar params
//! through [`widgets::param_slider`] and coarse leaves through
//! [`widgets::value_slider`] + your own decompose/recompose, and reach E09
//! only via [`develop_ctx::EditBinding`].

pub mod basic;
pub mod bw;
pub mod curve;
pub mod detail;
pub mod develop_ctx;
pub mod effects;
pub mod geometry;
pub mod grading;
pub mod histogram;
pub mod history;
pub mod host;
pub mod hsl;
pub mod layout;
pub mod looks;
pub mod optics;
pub mod presets;
pub mod widgets;

/// Restores the develop rails' arrangement from machine prefs: dock
/// layout, column widths, collapsed sections, panel-solo, and which panels
/// are switched off in Preferences. Call once at startup, **after** every
/// panel has registered (unknown ids are dropped and newly registered
/// panels appended, see [`layout::DockLayout::sync_registered`]).
pub fn restore_layout(host: &mut PanelHost, prefs: &lightbox_core::PrefsStore) {
    if let Some(saved) = prefs.machine().panel_layout.as_ref() {
        host.restore(saved);
    }
}

/// Writes the arrangement back when anything about it changed, a panel
/// dragged to another rail, a column resized, a section collapsed, solo
/// toggled, a panel switched off. One `prefs.toml` write per gesture (a
/// splitter drag marks itself dirty on release, not per frame), never one
/// per frame.
pub fn persist_layout_if_dirty(host: &mut PanelHost, prefs: &lightbox_core::PrefsStore) {
    if host.take_dirty() {
        let snapshot = host.snapshot();
        prefs.set_machine(|m| m.panel_layout = Some(snapshot));
    }
}

pub use develop_ctx::{DevelopCtx, SessionEditBinding};
pub use host::PanelHost;
pub use layout::DockSide;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::host::{PanelDef, PanelId, SourceReq};
    use crate::panels::layout::{DockSide, DropTarget};
    use lightbox_core::PrefsStore;

    const HISTOGRAM: PanelId = PanelId("test.histogram");
    const BASIC: PanelId = PanelId("test.basic");
    const CURVE: PanelId = PanelId("test.curve");

    fn stub(_ui: &mut eframe::egui::Ui, _ctx: &mut DevelopCtx<'_>) {}

    /// Three panels, as `lib.rs` registers the real ones at startup.
    fn fresh_host() -> PanelHost {
        let mut host = PanelHost::new();
        for (id, title) in [
            (HISTOGRAM, "Histogram"),
            (BASIC, "Basic"),
            (CURVE, "Tone Curve"),
        ] {
            host.register(PanelDef {
                id,
                title,
                source_req: SourceReq::Any,
                order: 20,
                build: stub,
            });
        }
        host
    }

    /// **Close the app, open it again, find it how you left it.**
    ///
    /// Drives the exact seam `lib.rs` uses, [`restore_layout`] at startup,
    /// [`persist_layout_if_dirty`] after the frame, against a real
    /// `prefs.toml` on disk, with a *second* `PrefsStore` standing in for
    /// the next launch. Covers all four things the rails remember:
    /// collapsed sections, switched-off panels, which rail/column a panel
    /// sits in, and panel-solo.
    #[test]
    fn collapsed_hidden_and_docked_state_all_survive_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let prefs = PrefsStore::open_machine(dir.path());

        let mut host = fresh_host();
        restore_layout(&mut host, &prefs); // first launch: nothing saved yet
        assert!(host.open_state(BASIC), "sections default to open");

        host.set_open(BASIC, false); // collapse a group
        host.set_hidden(CURVE, true); // switch a panel off
        host.move_panel(
            HISTOGRAM,
            DropTarget::NewColumn {
                side: DockSide::Left,
                at: 0,
            },
        ); // drag one to the left rail
        persist_layout_if_dirty(&mut host, &prefs);
        assert!(
            dir.path().join("prefs.toml").exists(),
            "the arrangement must reach the disk, not just the watch channel"
        );

        // Next launch: a brand-new store reading the same file, and a
        // brand-new host with the panels freshly registered.
        let next_launch = PrefsStore::open_machine(dir.path());
        assert_eq!(next_launch.load_notice(), None);
        let mut host = fresh_host();
        restore_layout(&mut host, &next_launch);

        assert!(
            !host.open_state(BASIC),
            "the collapsed group stayed collapsed"
        );
        assert!(host.open_state(HISTOGRAM), "an untouched group stayed open");
        assert!(host.is_hidden(CURVE), "the switched-off panel stayed off");
        assert_eq!(
            host.layout().columns(DockSide::Left)[0].panels,
            vec![HISTOGRAM],
            "the dragged panel stayed in the left rail"
        );

        // And a launch that changes nothing must not rewrite the file.
        let mut untouched = host;
        persist_layout_if_dirty(&mut untouched, &next_launch);
        assert!(
            !untouched.take_dirty(),
            "restoring is not a change worth writing back"
        );
    }
}
