// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The on-canvas gizmo layer (E08 Phase F, spec §6.6/§8 F1-F3): the
//! [`Gizmo`] trait, [`GizmoLayer`] (active-gizmo stack, input routing where
//! a gizmo hit wins over pan/zoom, drag capture, a paint pass above the
//! composited image, keymap sub-context push/pop), and the WB-eyedropper
//! reference gizmo ([`WbEyedropper`]).
//!
//! **Read `canvas/gizmo.md` first** if you are building a new gizmo
//! (E11 crop/geometry, E12 mask pins), it is the frozen author guide:
//! interaction conventions, hit-tolerance numbers, the gesture→effect
//! mapping, and the context push/pop rules, with the eyedropper worked
//! through end to end.
//!
//! # The contracts, in one screen
//!
//! * **Geometry lives in image space** (raw, unrotated pixels, the same
//!   space [`ViewXform`] maps; see `xform.rs`). It survives zoom, pan,
//!   window resize, and (post-E11) orientation changes for free.
//! * **Hit-testing happens in screen space** (logical points), against the
//!   frame's [`ViewXform`], the ONE authority both compositing and gizmos
//!   share (spec §6.4). Handle-style gizmos use a ≥ [`HIT_TOLERANCE_PT`]
//!   radius; area-style gizmos (the eyedropper) hit their whole region.
//! * **A gizmo hit wins over pan/zoom**: `EditorCanvas::ready_ui` calls
//!   [`GizmoLayer::route`] *before* its own drag-pan / double-click-zoom
//!   handling and skips both when the layer claims the pointer.
//! * **Esc = `Cancelled`, Enter = `Done`**, routed as keymap *actions*
//!   (`gizmo.cancel`/`gizmo.commit`, registered against
//!   [`keymap::CTX_GIZMO`] since Phase D), not raw key events: the Phase-D
//!   dispatcher consumes the chords before any widget sees them, so the
//!   layer receives [`GizmoLayer::cancel_active`]/
//!   [`GizmoLayer::commit_active`] calls from `lib.rs::handle_action`
//!   instead. (The spec's draft `GizmoEvent::Key(&egui::Event)` variant is
//!   therefore [`GizmoEvent::Cancel`]/[`GizmoEvent::Commit`] here, see
//!   `E08-deviations.md` Phase F.)
//! * **One drag = one `EditBinding` gesture**: a param-editing gizmo brackets
//!   its drag with [`GizmoEffect::EditBegin`] → [`GizmoEffect::Edit`]* →
//!   [`GizmoEffect::EditEnd`]; `lib.rs` routes those 1:1 onto
//!   `EditBinding::{begin_gesture, preview, end_gesture}` (E09's one-
//!   coalesced-history-step discipline).
//! * **Zero-copy paint**: gizmos never read pixels. Magnification (the
//!   eyedropper's loupe chip) is drawn by re-sampling the already-registered
//!   composited texture with a small UV rect ([`GizmoPaintCtx::texture`])
//!   no `map_async`, no CPU readback, per this crate's top-of-`lib.rs`
//!   zero-copy invariants.
//!
//! [`keymap::CTX_GIZMO`]: crate::keymap::CTX_GIZMO

use eframe::egui;
use lightbox_edit::{ParamDelta, ParamId};

use crate::canvas::xform::ViewXform;
use crate::keymap::ContextId;

/// Screen-space hit tolerance (logical points) for handle-style gizmos
/// the §6.6 "≥ 8 pt" convention, frozen. Area-style gizmos (the
/// eyedropper) don't use it: their whole image region is the hit target.
///
/// `#[allow(dead_code)]`: no handle-style gizmo exists at M1 (the
/// eyedropper is area-style); E11's crop handles and E12's mask pins are
/// the consumers, the constant is part of the frozen convention surface
/// (`canvas/gizmo.md`) so their hit math starts from the same number.
#[allow(dead_code)]
pub const HIT_TOLERANCE_PT: f32 = 8.0;

/// Stable gizmo identity (`"wb.eyedropper"`, later `"geom.crop"`,
/// `"mask.pin"`, …).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct GizmoId(pub &'static str);

/// A gizmo-local handle id, which part of the gizmo a hit landed on
/// (crop edge #3, mask pin #7, …). Meaning is private to the gizmo; the
/// layer only threads it through the drag lifecycle unchanged.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct HitId(pub u32);

/// Input delivered to the active gizmo (spec §6.6). All positions are
/// **image space** (raw pixels); the gizmo maps back to screen through the
/// accompanying [`ViewXform`] when it needs point-space math.
///
/// `#[allow(dead_code)]`: the `hit`/`delta_px` payload fields are read by
/// no M1 gizmo (the area-style eyedropper has one handle and tracks
/// positions, not deltas), they are the frozen drag-lifecycle contract
/// for E11/E12's handle-style gizmos (crop edges, mask pins), which the
/// layer already populates correctly.
#[allow(dead_code)]
#[derive(Copy, Clone, Debug)]
pub enum GizmoEvent {
    /// Pointer position this frame, `None` when the pointer is outside the
    /// composited image (or off the canvas). Sent every routed frame while
    /// no drag is captured.
    Hover {
        /// Image-space pointer position.
        image_px: Option<egui::Vec2>,
    },
    /// A drag began on `hit` ([`Gizmo::hit`] claimed the press position).
    DragStart {
        /// The handle claimed at the drag origin.
        hit: HitId,
        /// Image-space drag origin.
        image_px: egui::Vec2,
    },
    /// Drag continuation (every frame with pointer movement). `image_px` is
    /// clamped into the image bounds; `delta_px` is derived from the
    /// clamped positions, so it flattens at the edges (see `gizmo.md`).
    Drag {
        /// The captured handle.
        hit: HitId,
        /// Image-space pointer position (clamped into bounds).
        image_px: egui::Vec2,
        /// Image-space movement since the previous frame.
        delta_px: egui::Vec2,
    },
    /// The captured drag released.
    DragEnd {
        /// The handle that was captured.
        hit: HitId,
    },
    /// A click (press + release without drag) landed on the gizmo's hit
    /// region, inside the image.
    Click {
        /// Image-space click position.
        image_px: egui::Vec2,
    },
    /// `gizmo.cancel` (Esc by default), the gizmo should emit
    /// [`GizmoEffect::Cancelled`] once it has fully unwound (a multi-stage
    /// gizmo may consume one Cancel per stage; the layer pops it only when
    /// `Cancelled`/`Done` is actually emitted).
    Cancel,
    /// `gizmo.commit` (Enter by default), emit [`GizmoEffect::Done`] when
    /// finished.
    Commit,
}

/// What a gizmo asks the app to do (spec §6.6). Effects accumulate in the
/// layer during routing and are drained once per frame by
/// `lib.rs` ([`GizmoLayer::take_effects`]), which owns the mapping onto
/// `EditBinding` / panel callbacks.
///
/// `#[allow(dead_code)]`: the `EditBegin`/`Edit`/`EditEnd` bracket has no
/// M1 emitter (the eyedropper picks, it doesn't drag params), E11/E12's
/// param-editing gizmos are the consumers; `lib.rs` already routes all
/// three onto the `EditBinding` gesture lifecycle so those epics only
/// write the gizmo side.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum GizmoEffect {
    /// Start ONE `EditBinding` gesture labeled `ParamId` (drag lifecycle:
    /// emit from `DragStart`). Pair with exactly one [`GizmoEffect::EditEnd`].
    ///
    /// **Deviation note (recorded):** the spec's §6.6 enum carries only
    /// `Edit(ParamDelta)`; the begin/end markers were added because E09's
    /// gesture bracket (`begin_gesture(ParamId)`/`end_gesture`) cannot be
    /// routed from deltas alone, see `E08-deviations.md` Phase F.
    EditBegin(ParamId),
    /// A live preview delta inside the current gesture (drag lifecycle:
    /// emit from `Drag`). Routed to `EditBinding::preview`, re-renders the
    /// same frame via the C3 recipe_rev submit key.
    Edit(ParamDelta),
    /// End the gesture: ONE coalesced durable history step (emit from
    /// `DragEnd`).
    EditEnd,
    /// A point was picked (the WB eyedropper). The owner resolves what a
    /// pick *means*, at M1 `lib.rs` routes it to the basic panel's
    /// `apply_wb_pick` (a temporary estimate until E10's pixel sampling).
    Picked {
        /// Image-space pick position.
        image_px: egui::Vec2,
    },
    /// The gizmo finished its job, the layer pops it (and the keymap
    /// context with it).
    Done,
    /// The gizmo aborted, the layer pops it. A param-editing gizmo should
    /// have already unwound any open gesture before emitting this.
    Cancelled,
}

/// Read-only paint context handed to [`Gizmo::paint`] alongside the
/// painter/xform: everything a gizmo may sample *without* touching pixels.
pub struct GizmoPaintCtx {
    /// The composited texture currently displayed (engine frame or tier
    /// preview, whichever `EditorCanvas` composited this frame), already
    /// registered with egui, plus its pixel size. Draw it with a small UV
    /// rect to magnify (the loupe pattern), the zero-copy way. `None`
    /// while nothing has composited yet (placard frames).
    pub texture: Option<(egui::TextureId, [u32; 2])>,
}

/// An on-canvas interactive tool (spec §6.6). See the module docs for the
/// frozen contracts and `canvas/gizmo.md` for the worked example.
pub trait Gizmo: Send {
    /// Stable identity (activation/dedup/drag pinning).
    fn id(&self) -> GizmoId;

    /// Does `screen` (logical points) land on this gizmo? Convert through
    /// `xf` and answer with the gizmo-local handle. Handle-style gizmos
    /// apply [`HIT_TOLERANCE_PT`] in screen space; area-style gizmos test
    /// containment. `None` = the input falls through to pan/zoom.
    fn hit(&self, screen: egui::Pos2, xf: &ViewXform) -> Option<HitId>;

    /// Handle one routed event; push any resulting [`GizmoEffect`]s onto
    /// `out`. Emitting `Done`/`Cancelled` pops the gizmo from the layer
    /// immediately after this call returns. For [`GizmoEvent::Cancel`]/
    /// [`GizmoEvent::Commit`], `xf` may be the *last routed* transform (or
    /// a degenerate placeholder before the first routed frame), do not
    /// derive geometry from it on those two events.
    fn on_event(&mut self, ev: GizmoEvent, xf: &ViewXform, out: &mut Vec<GizmoEffect>);

    /// Paint above the composited image (clip rect = the canvas view rect).
    /// Store geometry in image space; map through `xf` here.
    fn paint(&self, painter: &egui::Painter, xf: &ViewXform, ctx: &GizmoPaintCtx);

    /// The keymap sub-context pushed while this gizmo is the innermost
    /// active one (`"editor.gizmo.<id>"`). The layer's owner pushes the
    /// generic [`crate::keymap::CTX_GIZMO`] *plus* this, so Esc/Enter
    /// resolve generically and gizmo-specific actions can shadow them
    /// (innermost context wins, `keymap/registry.rs`).
    fn context(&self) -> ContextId;

    /// Pointer cursor while the pointer is over this gizmo's hit region
    /// (and while it holds a drag capture).
    fn cursor(&self) -> egui::CursorIcon {
        egui::CursorIcon::Default
    }
}

// ─── The layer ──────────────────────────────────────────────────────────────

/// The active-gizmo stack + input router + paint pass (spec §6.6 F1).
///
/// Owned by `lib.rs` (it outlives the canvas frame: the keymap context
/// push and the `gizmo.cancel`/`gizmo.commit` action handlers need it
/// outside `EditorCanvas::ui`), threaded into `EditorCanvas::ui` each
/// frame as `&mut`, the canvas calls [`GizmoLayer::route`] before its own
/// pan/zoom input handling and [`GizmoLayer::paint`] after compositing.
///
/// At M1 exactly one gizmo (the WB eyedropper) exists, so the stack holds
/// 0 or 1 entries in practice, but the API is a real stack per the spec:
/// activation pushes, `Done`/`Cancelled` pops, input routes innermost-
/// first, and the innermost gizmo's keymap sub-context is the one pushed.
#[derive(Default)]
pub struct GizmoLayer {
    /// Active gizmos, bottom → top (top = innermost = first claim on input).
    stack: Vec<Box<dyn Gizmo>>,
    /// A captured drag: which gizmo (by id, survives stack mutation) and
    /// which of its handles. While set, ALL pointer input belongs to that
    /// gizmo, the canvas never pans mid-capture.
    drag: Option<(GizmoId, HitId)>,
    /// Effects accumulated this frame, drained by `lib.rs` once per frame.
    effects: Vec<GizmoEffect>,
    /// The most recent [`ViewXform`] seen by [`GizmoLayer::route`], used
    /// for [`GizmoLayer::cancel_active`]/[`GizmoLayer::commit_active`],
    /// which arrive from the keymap handler before the canvas builds this
    /// frame's transform. See [`Gizmo::on_event`]'s caveat.
    last_xform: Option<ViewXform>,
}

impl GizmoLayer {
    /// An empty layer (no active gizmo).
    pub fn new() -> GizmoLayer {
        GizmoLayer::default()
    }

    /// Pushes `gizmo` as the new innermost active gizmo. If a gizmo with
    /// the same id is already active it is cancelled first (re-activation
    /// restarts the tool, it never double-stacks).
    pub fn activate(&mut self, gizmo: Box<dyn Gizmo>) {
        self.cancel(gizmo.id());
        self.stack.push(gizmo);
    }

    /// Sends [`GizmoEvent::Cancel`] to the active gizmo with this id (if
    /// any). The gizmo pops when it emits `Cancelled`/`Done` in response.
    pub fn cancel(&mut self, id: GizmoId) {
        if let Some(index) = self.index_of(id) {
            self.dispatch(index, GizmoEvent::Cancel);
        }
    }

    /// `gizmo.cancel` (Esc): cancels the innermost active gizmo.
    pub fn cancel_active(&mut self) {
        if !self.stack.is_empty() {
            self.dispatch(self.stack.len() - 1, GizmoEvent::Cancel);
        }
    }

    /// `gizmo.commit` (Enter): commits the innermost active gizmo.
    pub fn commit_active(&mut self) {
        if !self.stack.is_empty() {
            self.dispatch(self.stack.len() - 1, GizmoEvent::Commit);
        }
    }

    /// Cancels every active gizmo (active-image change / canvas exit, a
    /// gizmo's geometry belongs to the image it was armed on).
    pub fn cancel_all(&mut self) {
        while !self.stack.is_empty() {
            let before = self.stack.len();
            self.dispatch(before - 1, GizmoEvent::Cancel);
            if self.stack.len() == before {
                // A gizmo refused to unwind on Cancel (multi-stage). Forced
                // teardown: the image is gone, drop it rather than leak an
                // active context onto the wrong image.
                self.stack.pop();
            }
        }
        self.drag = None;
    }

    /// True while a gizmo with this id is active (panel button state).
    pub fn is_active(&self, id: GizmoId) -> bool {
        self.index_of(id).is_some()
    }

    /// The innermost active gizmo's keymap sub-context, if any, `lib.rs`
    /// pushes [`crate::keymap::CTX_GIZMO`] + this while `Some`.
    pub fn active_context(&self) -> Option<ContextId> {
        self.stack.last().map(|g| g.context())
    }

    /// Drains the effects accumulated since the last drain (`lib.rs`, once
    /// per frame, after the canvas has routed).
    pub fn take_effects(&mut self) -> Vec<GizmoEffect> {
        std::mem::take(&mut self.effects)
    }

    /// F1 input routing, called by `EditorCanvas::ready_ui` *before* its
    /// own pan/double-click handling. Returns `true` when the layer
    /// claimed this frame's pointer interaction, the canvas must then
    /// skip drag-pan and double-click-zoom (a gizmo hit wins over
    /// pan/zoom, spec §6.6). Wheel zoom is deliberately NOT claimed:
    /// zooming while a tool is armed is legitimate and gizmo geometry is
    /// zoom-invariant (image space).
    pub fn route(&mut self, ui: &egui::Ui, response: &egui::Response, xf: &ViewXform) -> bool {
        self.last_xform = Some(*xf);
        if self.stack.is_empty() {
            return false;
        }

        // ── Drag capture continuation: the captured gizmo owns ALL
        // pointer input until release, even outside its hit region. ──
        if let Some((id, hit)) = self.drag {
            let Some(index) = self.index_of(id) else {
                // The captured gizmo popped mid-drag (Esc during a drag).
                self.drag = None;
                return false;
            };
            if let Some(pos) = response.interact_pointer_pos() {
                if response.dragged() {
                    let now = xf.screen_to_image_clamped(pos);
                    let prev = xf.screen_to_image_clamped(pos - response.drag_delta());
                    if let (Some(now), Some(prev)) = (now, prev) {
                        self.dispatch(
                            index,
                            GizmoEvent::Drag {
                                hit,
                                image_px: now,
                                delta_px: now - prev,
                            },
                        );
                    }
                }
            }
            if response.drag_stopped() {
                // Re-resolve: the Drag dispatch above may have popped it.
                if let Some(index) = self.index_of(id) {
                    self.dispatch(index, GizmoEvent::DragEnd { hit });
                }
                self.drag = None;
            }
            if let Some(index) = self.index_of(id) {
                ui.ctx().set_cursor_icon(self.stack[index].cursor());
            }
            return true;
        }

        // ── Hover: always routed to the innermost gizmo (crosshair /
        // loupe tracking), plus the cursor hint over its hit region. ──
        let hover_pos = response.hover_pos();
        let top = self.stack.len() - 1;
        self.dispatch(
            top,
            GizmoEvent::Hover {
                image_px: hover_pos.and_then(|p| xf.screen_to_image(p)),
            },
        );
        if self.stack.is_empty() {
            return false; // hover finished the gizmo (unusual but legal)
        }
        if let Some(pos) = hover_pos {
            if let Some(gizmo) = self.stack.last() {
                if gizmo.hit(pos, xf).is_some() {
                    ui.ctx().set_cursor_icon(gizmo.cursor());
                }
            }
        }

        // ── Fresh interactions: innermost-first, first hit claims. ──
        let mut claimed = false;
        if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                for index in (0..self.stack.len()).rev() {
                    if let Some(hit) = self.stack[index].hit(pos, xf) {
                        if let Some(image_px) = xf.screen_to_image_clamped(pos) {
                            self.drag = Some((self.stack[index].id(), hit));
                            self.dispatch(index, GizmoEvent::DragStart { hit, image_px });
                        }
                        claimed = true;
                        break;
                    }
                }
            }
        } else if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                for index in (0..self.stack.len()).rev() {
                    if self.stack[index].hit(pos, xf).is_some() {
                        if let Some(image_px) = xf.screen_to_image(pos) {
                            self.dispatch(index, GizmoEvent::Click { image_px });
                        }
                        claimed = true;
                        break;
                    }
                }
            }
        }
        if response.double_clicked() {
            // Suppress the canvas's Fit↔100% toggle when the double-click
            // lands on a gizmo (its two clicks were tool input).
            if let Some(pos) = response.interact_pointer_pos() {
                if self.stack.iter().rev().any(|g| g.hit(pos, xf).is_some()) {
                    claimed = true;
                }
            }
        }
        claimed
    }

    /// F1 paint pass: above the composited image, below the canvas's own
    /// info overlay/chips. Bottom → top, so the innermost gizmo paints
    /// last (on top).
    pub fn paint(&self, painter: &egui::Painter, xf: &ViewXform, ctx: &GizmoPaintCtx) {
        for gizmo in &self.stack {
            gizmo.paint(painter, xf, ctx);
        }
    }

    fn index_of(&self, id: GizmoId) -> Option<usize> {
        self.stack.iter().position(|g| g.id() == id)
    }

    /// Delivers one event and pops the gizmo if it emitted
    /// `Done`/`Cancelled` (clearing any drag capture it held).
    fn dispatch(&mut self, index: usize, ev: GizmoEvent) {
        let xf = self.last_xform.unwrap_or_else(degenerate_xform);
        let before = self.effects.len();
        self.stack[index].on_event(ev, &xf, &mut self.effects);
        let finished = self.effects[before..]
            .iter()
            .any(|e| matches!(e, GizmoEffect::Done | GizmoEffect::Cancelled));
        if finished {
            let gone = self.stack.remove(index);
            if self.drag.is_some_and(|(id, _)| id == gone.id()) {
                self.drag = None;
            }
        }
    }
}

/// The placeholder transform for events dispatched before the canvas ever
/// routed (e.g. Esc while the canvas shows a placard). Per
/// [`Gizmo::on_event`]'s contract, gizmos must not derive geometry from
/// the xform on `Cancel`/`Commit`, the only events that can arrive this
/// way.
fn degenerate_xform() -> ViewXform {
    ViewXform::new(
        egui::Vec2::ZERO,
        lightbox_types::Orientation::O1,
        egui::Rect::ZERO,
        1.0,
        1.0,
        egui::Vec2::ZERO,
    )
}

// ─── F2: the reference gizmo, WB eyedropper ────────────────────────────────

/// The WB eyedropper's stable id.
pub const WB_EYEDROPPER: GizmoId = GizmoId("wb.eyedropper");

/// Its keymap sub-context (see [`Gizmo::context`]). No action targets it
/// at M1, `gizmo.cancel`/`gizmo.commit` are registered against the
/// generic `editor.gizmo`, which the owner pushes alongside this.
const CTX_WB_EYEDROPPER: ContextId = ContextId("editor.gizmo.wb_eyedropper");

/// Loupe chip edge length, logical points.
const LOUPE_SIZE_PT: f32 = 96.0;
/// Image pixels shown across the loupe (the magnified region's width)
/// small enough that individual pixels are discernible at any zoom.
const LOUPE_REGION_PX: f32 = 24.0;
/// Cursor → chip offset, logical points.
const LOUPE_OFFSET_PT: f32 = 18.0;

/// F2, the white-balance eyedropper (spec §6.6's reference gizmo, §8 F2):
/// an **area-style** gizmo whose hit region is the entire composited image.
/// While active: crosshair cursor + a magnified loupe chip that follows the
/// pointer, drawn zero-copy from the composited texture. A click emits
/// [`GizmoEffect::Picked`] at the image-space point followed by
/// [`GizmoEffect::Done`] (one pick per arming, the Lightroom convention);
/// Esc cancels without picking. Dragging never pans the canvas (F1 AC)
/// it just moves the loupe until release.
///
/// The pick's *meaning* (pixel → WB params) is deliberately NOT here: the
/// gizmo reads no pixels (zero-copy invariant) and owns no color science
/// (§2.3). `lib.rs` routes `Picked` to `panels::basic::apply_wb_pick`
/// at M1 a temporary position-linear estimate behind a
/// `debug_assertions`-gated TODO; E10 replaces that callback's body with
/// real neutral-point sampling without touching this gizmo.
pub struct WbEyedropper {
    /// Pointer position in IMAGE space (the frozen convention: geometry
    /// survives zoom/pan/resize), `None` until the pointer enters the
    /// image.
    hover_image_px: Option<egui::Vec2>,
}

impl WbEyedropper {
    /// A fresh, un-hovered eyedropper.
    pub fn new() -> WbEyedropper {
        WbEyedropper {
            hover_image_px: None,
        }
    }
}

impl Default for WbEyedropper {
    fn default() -> Self {
        WbEyedropper::new()
    }
}

impl Gizmo for WbEyedropper {
    fn id(&self) -> GizmoId {
        WB_EYEDROPPER
    }

    /// Area-style: any screen point inside the composited image hits
    /// (handle id 0, the eyedropper has exactly one "handle": the image).
    /// Letterbox/outside points fall through to pan/zoom.
    fn hit(&self, screen: egui::Pos2, xf: &ViewXform) -> Option<HitId> {
        xf.screen_to_image(screen).map(|_| HitId(0))
    }

    fn on_event(&mut self, ev: GizmoEvent, _xf: &ViewXform, out: &mut Vec<GizmoEffect>) {
        match ev {
            GizmoEvent::Hover { image_px } => self.hover_image_px = image_px,
            // A drag is a captured inspection (the loupe tracks; the canvas
            // must not pan, F1 AC); release picks nothing, only a clean
            // click commits a pick.
            GizmoEvent::DragStart { image_px, .. } | GizmoEvent::Drag { image_px, .. } => {
                self.hover_image_px = Some(image_px);
            }
            GizmoEvent::DragEnd { .. } => {}
            GizmoEvent::Click { image_px } => {
                out.push(GizmoEffect::Picked { image_px });
                out.push(GizmoEffect::Done);
            }
            GizmoEvent::Cancel => out.push(GizmoEffect::Cancelled),
            GizmoEvent::Commit => out.push(GizmoEffect::Done),
        }
    }

    fn paint(&self, painter: &egui::Painter, xf: &ViewXform, ctx: &GizmoPaintCtx) {
        let Some(image_px) = self.hover_image_px else {
            return;
        };
        let screen = xf.image_to_screen(image_px);
        let clip = painter.clip_rect();

        // Crosshair: four hairline arms with a contrast underlay, leaving
        // the exact target pixel unobscured.
        let dark = egui::Stroke::new(3.0, egui::Color32::from_black_alpha(160));
        let light = egui::Stroke::new(1.0, egui::Color32::WHITE);
        for (from, to) in [
            (egui::vec2(-11.0, 0.0), egui::vec2(-4.0, 0.0)),
            (egui::vec2(4.0, 0.0), egui::vec2(11.0, 0.0)),
            (egui::vec2(0.0, -11.0), egui::vec2(0.0, -4.0)),
            (egui::vec2(0.0, 4.0), egui::vec2(0.0, 11.0)),
        ] {
            painter.line_segment([screen + from, screen + to], dark);
            painter.line_segment([screen + from, screen + to], light);
        }

        // Loupe chip: the composited texture re-drawn with a small UV rect
        // centered on the hovered image pixel, magnification WITHOUT any
        // pixel readback (zero-copy invariant; module docs). UVs are
        // normalized over the full image, valid because the composited
        // texture always shows the whole image (the engine renders
        // fit-to-viewport; zoom/pan are display-side crops, `view.rs`).
        // Sampler address mode clamps at the edges, so a near-edge UV rect
        // degrades gracefully. M1 note: UV mapping assumes `Orientation::O1`
        // (the canvas's only orientation until E11, `xform.rs` module docs).
        let Some((texture, _tex_px)) = ctx.texture else {
            return;
        };
        let image_size = xf.image_size_px();
        if image_size.x <= 0.0 || image_size.y <= 0.0 {
            return;
        }
        let uv_center = egui::pos2(image_px.x / image_size.x, image_px.y / image_size.y);
        let uv_half = egui::vec2(
            LOUPE_REGION_PX * 0.5 / image_size.x,
            LOUPE_REGION_PX * 0.5 / image_size.y,
        );
        let uv = egui::Rect::from_min_max(uv_center - uv_half, uv_center + uv_half);

        // Chip placement: below-right of the cursor, flipped inside the
        // canvas when it would overflow.
        let mut chip = egui::Rect::from_min_size(
            screen + egui::vec2(LOUPE_OFFSET_PT, LOUPE_OFFSET_PT),
            egui::vec2(LOUPE_SIZE_PT, LOUPE_SIZE_PT),
        );
        if chip.max.x > clip.max.x {
            chip = chip.translate(egui::vec2(-(LOUPE_SIZE_PT + 2.0 * LOUPE_OFFSET_PT), 0.0));
        }
        if chip.max.y > clip.max.y {
            chip = chip.translate(egui::vec2(0.0, -(LOUPE_SIZE_PT + 2.0 * LOUPE_OFFSET_PT)));
        }

        painter.rect_filled(chip.expand(2.0), 4.0, egui::Color32::from_black_alpha(200));
        painter.image(texture, chip, uv, egui::Color32::WHITE);
        painter.rect_stroke(
            chip,
            2.0,
            egui::Stroke::new(1.0, egui::Color32::from_white_alpha(180)),
            egui::StrokeKind::Outside,
        );
        // Center marker: one loupe "pixel" outlined (the pick target).
        let px_pt = LOUPE_SIZE_PT / LOUPE_REGION_PX;
        painter.rect_stroke(
            egui::Rect::from_center_size(chip.center(), egui::vec2(px_pt, px_pt)),
            0.0,
            egui::Stroke::new(1.0, egui::Color32::WHITE),
            egui::StrokeKind::Outside,
        );
    }

    fn context(&self) -> ContextId {
        CTX_WB_EYEDROPPER
    }

    fn cursor(&self) -> egui::CursorIcon {
        egui::CursorIcon::Crosshair
    }
}
