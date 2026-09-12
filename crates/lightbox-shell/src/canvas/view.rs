// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The editor canvas (E08 spec §6.4, tasks C2-C4): [`EditorCanvas`] evolves
//! the E01/F5 `loupe.rs`, every displayed pixel is still produced by
//! `ng::Engine::submit` and composited **zero-copy** on the shared
//! `wgpu::Device` (`RenderScheduler::canvas()`'s watch channel, unchanged
//! see the old `loupe.rs` module docs, carried forward here). What's new in
//! Phase C:
//!
//! * **C2**, [`ZoomMode`] generalizes the old binary Fit/100% toggle to a
//!   wheel-stepped ladder (`{25, 50, 100, 200}%` plus `Fit`) with
//!   zoom-to-cursor, over the same drag-pan-with-clamping logic (now pure
//!   functions: [`ladder_step`], [`clamp_pan`], [`pan_for_zoom_to_cursor`]).
//! * **C3**, [`RecipeSource`]/[`RecipeSnapshot`] replace the hardcoded
//!   `Recipe::identity` submit with a recipe-driven one; [`submit_if_changed`]
//!   is the pure "what changed, what to (re)submit" decision, keyed by
//!   `recipe_rev` instead of hashing the recipe.
//! * **C4**, [`TierPreview`] (E03's `PreviewClass::Loupe`) composites the
//!   best-available tier immediately on activation; [`ProgressiveDisplay`]
//!   is the pure tier→engine swap / same-image-failure decision core (spec
//!   §6.4).
//!
//! **Render-request scope note (read before changing `submit_if_changed`):**
//! `RenderScheduler::to_request` (`lightbox-render`) builds every
//! `RenderRequest` with `scale: RenderScale::Fit(view.viewport)` and never
//! reads `ViewState::zoom`/`ViewState::pan` at all, the engine always
//! renders "fit to the submitted viewport pixels"; zoom/pan/orientation are
//! entirely display-side crops of that texture (`ViewXform`, `xform.rs`).
//! This is unchanged from the pre-Phase-C loupe and is consistent with
//! `E05-deviations.md`'s note that Phase-C tiling isn't wired into live
//! `Engine::submit` yet. `submit_if_changed` therefore keys the engine
//! submit on `(image, viewport_px, recipe_rev)` only, **not** on
//! `ZoomMode`, since including it would force a wasted resubmit on every
//! wheel step for zero rendering benefit.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use lightbox_core::Session;
use lightbox_edit::{Crop, Recipe};
use lightbox_jobs::Class;
use lightbox_preview::{PreviewClass, PreviewProvider, PreviewState, PreviewTicket};
use lightbox_render::ng::{
    CanvasFrame, ClipOverlayPass, Extent, HistogramData, HistogramPass, OutputQuality,
    RenderScheduler, Roi, ViewState, Zoom,
};
use lightbox_types::{ImageId, Orientation, ProcessVersion, SourceTier, PV_M0};

use crate::canvas::before_after::{self, BeforeAfterView, BeforeKey};
use crate::canvas::gizmo::{GizmoLayer, GizmoPaintCtx};
use crate::canvas::states::{self, BeforeAfterMode, CanvasPlacard};
use crate::canvas::xform::{self, ViewXform};
use crate::theme::{fonts, tokens};
use crate::ShellOutcome;

// ─── C2: zoom ladder & pan (pure math) ──────────────────────────────────────

/// Zoom modes (spec §6.4). `Fit` resolves to a concrete percent (never
/// upscaling) against the current viewport each frame; `Percent(1.0)` is
/// "100%" (one image pixel = one physical pixel).
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum ZoomMode {
    /// Fit the viewport (never upscales, matches `filmstrip::fit_rect`).
    Fit,
    /// An explicit percent, e.g. `1.0` = 100 %.
    Percent(f32),
}

impl ZoomMode {
    /// The concrete percent this mode resolves to, given this frame's
    /// `Fit` percent.
    fn resolve(self, fit_percent: f32) -> f32 {
        match self {
            ZoomMode::Fit => fit_percent,
            ZoomMode::Percent(p) => p,
        }
    }

    /// Top-bar label ("Fit" / "100%").
    pub fn label(self) -> String {
        match self {
            ZoomMode::Fit => "Fit".to_owned(),
            ZoomMode::Percent(p) => format!("{:.0}%", p * 100.0),
        }
    }
}

/// The wheel-stepped rungs (spec §6.4: `{Fit, 25, 50, 100, 200}`, `Fit`
/// itself isn't a rung here since it's resolved to a percent before
/// stepping; this is the `{25, 50, 100, 200}` part).
const ZOOM_LADDER: [f32; 4] = [0.25, 0.5, 1.0, 2.0];

/// Steps `current` (a resolved percent) to the next/previous ladder rung.
/// Clamped at both ends (never wraps); `delta == 0` is a no-op.
fn ladder_step(current: f32, delta: i32) -> f32 {
    match delta.cmp(&0) {
        std::cmp::Ordering::Greater => ZOOM_LADDER
            .into_iter()
            .find(|&rung| rung > current + f32::EPSILON)
            .unwrap_or(*ZOOM_LADDER.last().expect("non-empty ladder")),
        std::cmp::Ordering::Less => ZOOM_LADDER
            .into_iter()
            .rev()
            .find(|&rung| rung < current - f32::EPSILON)
            .unwrap_or(ZOOM_LADDER[0]),
        std::cmp::Ordering::Equal => current,
    }
}

/// Clamps `pan` (screen-space points) so the displayed image never leaves
/// the view, generalized from the pre-Phase-C loupe's OneToOne-mode
/// clamp. An image that fits entirely within `view_size` can't be panned
/// (locks to centered, `Vec2::ZERO`).
fn clamp_pan(
    pan: egui::Vec2,
    display_size_screen: egui::Vec2,
    view_size: egui::Vec2,
) -> egui::Vec2 {
    let max_off = ((display_size_screen - view_size) * 0.5).max(egui::Vec2::ZERO);
    pan.clamp(-max_off, max_off)
}

/// The new pan that keeps the image point under `cursor` fixed on screen
/// while the points-per-image-pixel scale changes from `scale0` to
/// `scale1` (spec C2 AC: "zoom-to-cursor keeps the cursor's image point
/// fixed within 1 px"). `center` is the view rect's center (the xform's
/// zero-pan anchor).
fn pan_for_zoom_to_cursor(
    pan0: egui::Vec2,
    scale0: f32,
    scale1: f32,
    cursor: egui::Pos2,
    center: egui::Pos2,
) -> egui::Vec2 {
    if scale0 <= 0.0 {
        return pan0;
    }
    let ratio = scale1 / scale0;
    (cursor - center) * (1.0 - ratio) + pan0 * ratio
}

// ─── E11 canvas-display fix, Part A: the display basis follows a
//     committed crop (`E11-deviations.md` "geometry canvas display") ──────

/// The display/fit/zoom/pan basis extent for `crop` applied to a
/// `source_w`×`source_h` source. Mirrors
/// `lightbox_render::ng::nodes::geometry::crop::crop_pixels_from_params`'s
/// rounding **exactly** (that function is private to the render crate
/// this is a read-only reproduction on the shell side, per the phase
/// brief, not a dependency) so the canvas's fit-to-view/zoom/pan math
/// agrees with the engine's actual `geom.crop` output extent for a
/// committed crop, instead of stretching the (now smaller) cropped
/// texture across the full pre-crop frame.
///
/// `crop == Crop::default()` (no crop, or the crop tool's own
/// uncropped-preview override, Part B) short-circuits to the exact
/// source dims with no rounding pass at all, so an uncropped image's
/// `image_px` stays bit-identical to the pre-E11 behavior (the C1-C4
/// regression bar this phase must not move).
fn cropped_extent(source_w: u32, source_h: u32, crop: Crop) -> (u32, u32) {
    if crop == Crop::default() {
        return (source_w, source_h);
    }
    let src_w = source_w.max(1) as i64;
    let src_h = source_h.max(1) as i64;
    let round_px = |v: f32, dim: i64| -> i64 {
        ((v as f64) * dim as f64).round().clamp(0.0, dim as f64) as i64
    };
    let mut l = round_px(crop.left, src_w);
    let mut t = round_px(crop.top, src_h);
    let mut r = round_px(crop.right, src_w);
    let mut b = round_px(crop.bottom, src_h);
    // Degenerate rounding (a crop narrower than half a pixel) folds to a
    // minimal 1px window rather than an empty one, mirrors
    // `crop.rs::crop_pixels_from_params` exactly.
    if r <= l {
        r = (l + 1).min(src_w);
        l = (r - 1).max(0);
    }
    if b <= t {
        b = (t + 1).min(src_h);
        t = (b - 1).max(0);
    }
    ((r - l).max(1) as u32, (b - t).max(1) as u32)
}

// ─── Canvas content contract (what `lib.rs` hands the canvas each frame) ───

/// What the canvas is actually showing for this image.
///
/// A copyable summary of `lightbox_core::RawSourceStatus`, which carries the
/// fallback reason as an owned `String` and so cannot be `Copy` like
/// [`ActiveEntry`] is. The reason itself is not lost: the status notice reads
/// it from the session, where it lives in full.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RawPixels {
    /// Not a raw file. Nothing to say.
    NotRaw,
    /// Real sensor data, decoded through LibRaw and the camera's own matrices.
    Sensor,
    /// The sensor path was unavailable, so this is the JPEG the camera
    /// embedded. The badge says so rather than letting it pass as sensor data.
    EmbeddedPreview,
}

impl From<&lightbox_core::RawSourceStatus> for RawPixels {
    fn from(s: &lightbox_core::RawSourceStatus) -> RawPixels {
        match s {
            lightbox_core::RawSourceStatus::NotRaw => RawPixels::NotRaw,
            lightbox_core::RawSourceStatus::Sensor { .. } => RawPixels::Sensor,
            lightbox_core::RawSourceStatus::FellBack { .. } => RawPixels::EmbeddedPreview,
        }
    }
}

/// The working-set entry the canvas should render this frame, same role
/// as the pre-Phase-C `ActiveEntry`, unchanged shape.
#[derive(Copy, Clone, Debug)]
pub struct ActiveEntry<'a> {
    /// The registered image to render.
    pub image: ImageId,
    /// Display filename (info overlay).
    pub filename: &'a str,
    /// Full-size width (raw, unrotated, image space, spec §6.4/§6.6).
    pub width: u32,
    /// Full-size height.
    pub height: u32,
    /// Where this image's pixels came from. Derived from the session's source
    /// router, so the badge and the router can never disagree about what the
    /// user is looking at.
    pub raw_pixels: RawPixels,
}

/// What the working set says about the currently active entry (C5:
/// `lib.rs` projects `ItemState` into this each frame; the canvas owns
/// rendering every arm, see `states.rs`).
pub enum CanvasContent<'a> {
    /// Reached `Ready`, the normal, recipe-driven render path.
    Ready(ActiveEntry<'a>),
    /// Registration/hash still pending (`ItemState::Planned`), loading
    /// shimmer.
    Loading,
    /// Probe/hash/registration failed, the source-failed placard.
    SourceFailed {
        /// Human-readable failure reason.
        reason: &'a str,
    },
    /// Collapsed into an earlier identical-content item.
    Duplicate {
        /// 0-based index of the earlier item.
        of: usize,
    },
}

// ─── C3: the recipe seam ────────────────────────────────────────────────────

/// A recipe + a cheap, monotonic "did the effective recipe change" counter
/// (spec §6.4: `recipe_rev` is E09's cheap change counter, the submit key
/// includes it so a preview delta re-renders without hashing the recipe in
/// the UI). Phase E's live `EditBinding` (in-memory, DB-free per gesture)
/// produces this per-frame from the working snapshot; Phase C's
/// [`SessionRecipeSource`] produces it by polling E09's PERSISTED store.
#[derive(Clone)]
pub struct RecipeSnapshot {
    /// The recipe to render.
    pub recipe: Recipe,
    /// The process version it renders under.
    pub pv: ProcessVersion,
    /// Bumped whenever the effective recipe actually changes; the canvas
    /// resubmits exactly when this differs from the last-submitted value.
    pub rev: u64,
}

/// The canvas's recipe seam (C3): [`EditorCanvas::ui`] calls this once per
/// frame for the active image instead of hardcoding `Recipe::identity`.
/// Phase E swaps the concrete source (`SessionRecipeSource` → an
/// `EditBinding`-backed one); this trait's shape does not change, so the
/// canvas's call sites don't change again when that lands.
pub trait RecipeSource {
    /// The current recipe (+ revision) for `image`.
    fn recipe_for(&mut self, image: ImageId) -> RecipeSnapshot;
}

/// The real, Phase-C recipe source: E09's persisted `EditStore::recipe_of`,
/// cached with the same dirty-flag discipline `filmstrip::EditedBadges`
/// uses for the `edit_index` projection (`filmstrip.rs`'s module docs), a
/// SQL read happens only the first time an image is seen, or after
/// `lib.rs` marks it dirty on `Event::EditCommitted` (never once per
/// frame, spec §7). Phase E's live `EditBinding` replaces this; nothing in
/// `EditorCanvas` changes when it does.
pub struct SessionRecipeSource {
    session: Session,
    cache: HashMap<ImageId, (Recipe, ProcessVersion)>,
    rev: HashMap<ImageId, u64>,
    dirty: HashSet<ImageId>,
}

impl SessionRecipeSource {
    /// A source over `session` (a cheap `Arc`-backed clone, spec
    /// `Session::clone`).
    pub fn new(session: Session) -> SessionRecipeSource {
        SessionRecipeSource {
            session,
            cache: HashMap::new(),
            rev: HashMap::new(),
            dirty: HashSet::new(),
        }
    }

    /// Schedules a re-read of `image`'s persisted recipe on its next
    /// `recipe_for` call (`lib.rs` calls this from `drain_events` on
    /// `Event::EditCommitted`).
    pub fn mark_dirty(&mut self, image: ImageId) {
        self.dirty.insert(image);
    }
}

impl RecipeSource for SessionRecipeSource {
    fn recipe_for(&mut self, image: ImageId) -> RecipeSnapshot {
        let need_fetch = self.dirty.remove(&image) || !self.cache.contains_key(&image);
        if need_fetch {
            match self.session.edits().store().recipe_of(image) {
                Ok(read) => match read.recipe() {
                    Some(recipe) => {
                        let changed = self.cache.get(&image).map(|(r, _)| r) != Some(recipe);
                        if changed {
                            self.cache.insert(image, (recipe.clone(), recipe.pv));
                            *self.rev.entry(image).or_insert(0) += 1;
                        } else {
                            self.rev.entry(image).or_insert(0);
                        }
                    }
                    None => {
                        // `RecipeRead::NewerSchema`, nothing safely
                        // decodable; keep showing the last known recipe
                        // (never fatal, spec §7).
                        tracing::warn!(
                            target: "lightbox_shell",
                            image = image.0,
                            "recipe doc is a newer schema than this build supports; \
                             keeping the last known recipe"
                        );
                    }
                },
                Err(err) => {
                    tracing::warn!(
                        target: "lightbox_shell",
                        %err,
                        image = image.0,
                        "recipe_of failed; keeping the last known recipe"
                    );
                }
            }
        }
        let (recipe, pv) = self
            .cache
            .get(&image)
            .cloned()
            .unwrap_or((Recipe::identity(PV_M0), PV_M0));
        let rev = *self.rev.get(&image).unwrap_or(&0);
        RecipeSnapshot { recipe, pv, rev }
    }
}

/// The `(image, viewport-px, recipe_rev)` triple most recently submitted
/// see the module docs for why `ZoomMode` is deliberately NOT part of this
/// key.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
struct SubmitKey {
    image: ImageId,
    out: [u32; 2],
    recipe_rev: u64,
}

/// What one `submit_if_changed` call issued (H2 probe granularity: a
/// `RecipeOnly` submit is a slider-driven re-render, the input→submit
/// latency probe counts exactly these).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum SubmitScope {
    /// The image or viewport changed, the view was re-established.
    View,
    /// Only `recipe_rev` moved, a gesture preview / edit re-render.
    RecipeOnly,
}

/// C3's submit decision, factored out of [`EditorCanvas::ui`] so it is
/// integration-testable against a REAL `RenderScheduler` without a
/// `RenderState`/GPU (see the module tests), `EditorCanvas` is the only
/// production caller. Submits only what actually changed: a new image (or
/// viewport-pixel resize) re-establishes the view; a `recipe_rev` change
/// alone only re-submits the recipe (`RenderScheduler::set_recipe`
/// re-renders at the already-known view on its own). Latest-wins
/// per-viewport coalescing is entirely the scheduler's (spec §3.7/B6)
/// unchanged by this phase. Returns `Some(scope)` iff a submit was issued.
fn submit_if_changed(
    scheduler: &RenderScheduler,
    last_key: &mut Option<SubmitKey>,
    image: ImageId,
    out: [u32; 2],
    recipe: RecipeSnapshot,
) -> Option<SubmitScope> {
    let key = SubmitKey {
        image,
        out,
        recipe_rev: recipe.rev,
    };
    if *last_key == Some(key) {
        return None;
    }
    let view_changed = last_key.is_none_or(|p| p.image != image || p.out != out);
    if view_changed {
        scheduler.set_view(
            image,
            ViewState {
                viewport: Extent {
                    w: out[0],
                    h: out[1],
                },
                zoom: Zoom(1.0),
                pan: Roi {
                    x: 0,
                    y: 0,
                    w: out[0],
                    h: out[1],
                },
            },
        );
    }
    scheduler.set_recipe(image, recipe.recipe, recipe.pv);
    *last_key = Some(key);
    Some(if view_changed {
        SubmitScope::View
    } else {
        SubmitScope::RecipeOnly
    })
}

/// Whether the engine frame about to be composited has the **wrong shape**,
/// as a message for the non-modal error chip.
///
/// The compositor maps the whole engine texture (uv `0,0`→`1,1`) onto the rect
/// `ViewXform` derives from the image's full pixel extent, so it is only ever
/// correct for a frame that covers the whole image. It used to discard the
/// frame's own size entirely (`Some((texture_id, _size))`), which made a
/// wrong-shaped frame a *silent* visual corruption, it simply got stretched
/// over the image rect, reading as "zoomed into a corner". That is the failure
/// mode behind both this bug class's reports, and the reason it took a second
/// occurrence to find: nothing anywhere ever compared the two extents.
///
/// The engine's contract is that a frame lands on exactly the extent the
/// request asked for (`RenderRequest::roi`'s extent, the viewport this canvas
/// submitted). So a disagreement means one of two things:
///
/// * **In flight.** The viewport just changed and the composited frame is the
///   previous viewport's. Benign and self-correcting, and still *correctly*
///   composited, because that frame is also a whole-image fit, just at a
///   different resolution. `awaiting_frame` is precisely the shell's "a fresher
///   frame is coming" flag, so this case is excluded rather than reported.
/// * **Settled and wrong.** No fresher frame is coming and the extents still
///   disagree: the engine resolved a differently-shaped frame than was asked
///   for, exactly the stale-cached-extent fault `f4af88a` fixed. Surfaced.
///
/// Deliberately a *chip*, not a placard: the frame is still the best pixels
/// available and blanking the canvas over a shape disagreement would be a
/// worse outcome than showing it with a warning (spec §6.4's "keep the last
/// good frame + a non-modal chip" rule for the sibling render-failure case).
///
/// **Scope, stated honestly:** this catches a frame whose *extent* is wrong.
/// It cannot catch a frame of the right extent holding the wrong *content*
/// which is what the WB crop this commit fixes actually produced. No cheap
/// check at this seam can; that invariant is enforced where it belongs, in
/// `ng::exec::Executor::evaluate`, by deriving every node's target extent from
/// the tile it is actually handed.
fn engine_frame_shape_fault(
    frame: [u32; 2],
    submitted: Option<[u32; 2]>,
    awaiting_frame: bool,
) -> Option<String> {
    let want = submitted?;
    if awaiting_frame || frame == want {
        return None;
    }
    Some(format!(
        "frame {}×{} ≠ view {}×{}",
        frame[0], frame[1], want[0], want[1]
    ))
}

// ─── C4: progressive tier → engine display ─────────────────────────────────

/// What the canvas is currently authoritatively showing (pure decision
/// state, see [`ProgressiveDisplay`]).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Showing {
    #[default]
    Nothing,
    Tier,
    Engine,
}

/// C4's progressive-display decision core (pure, no GPU/egui), factored
/// out of [`EditorCanvas`] so the tier→engine swap and same-image-failure
/// rules (spec §6.4) are directly unit-testable. `EditorCanvas` is the
/// only production driver.
#[derive(Debug, Default)]
struct ProgressiveDisplay {
    showing: Showing,
    error: Option<String>,
}

impl ProgressiveDisplay {
    /// A tier-preview decode landed. Composites it immediately UNLESS
    /// something is already showing (the engine frame always wins, it's
    /// necessarily fresher). Returns `true` the frame this actually swaps
    /// in the tier.
    fn tier_ready(&mut self) -> bool {
        if self.showing == Showing::Nothing {
            self.showing = Showing::Tier;
            self.error = None;
            true
        } else {
            false
        }
    }

    /// The engine produced a frame for the SAME image, always supersedes
    /// the tier preview (spec §6.4: "then swap in the engine result on
    /// Ready"). Exactly one swap per activation, from whatever was
    /// showing (nothing or the tier) to the engine.
    fn engine_ready(&mut self) {
        self.showing = Showing::Engine;
        self.error = None;
    }

    /// A render attempt for the SAME image failed. Per §6.4: a failure
    /// while a good frame already exists keeps that frame and surfaces a
    /// non-modal chip; a failure before anything ever composited leaves
    /// `has_content() == false` so the caller renders the `RenderFailed`
    /// placard instead.
    fn engine_failed(&mut self, reason: String) {
        self.error = Some(reason);
    }

    /// `#[allow(dead_code)]`: `ready_ui` derives the same fact from
    /// whether `tex` resolved to a texture (it needs the texture handle
    /// too, not just the bool), this accessor documents the invariant
    /// precisely and is what the pure unit tests below assert against.
    #[allow(dead_code)]
    fn has_content(&self) -> bool {
        self.showing != Showing::Nothing
    }

    fn is_engine_frame(&self) -> bool {
        self.showing == Showing::Engine
    }

    fn error_chip(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// The single-slot progressive-tier preview for the ACTIVE image (C4):
/// requests E03's best-available `PreviewClass::Loupe` rendition on
/// activation and composites it the moment it decodes, mirrors
/// `thumbs::ThumbCache`'s request/poll discipline, narrowed to exactly one
/// image (the canvas shows one image at a time; there is no virtualized
/// set to track here).
struct TierPreview {
    provider: Arc<dyn PreviewProvider>,
    image: Option<ImageId>,
    ticket: Option<PreviewTicket>,
    texture: Option<(egui::TextureHandle, [u32; 2])>,
    tier: Option<SourceTier>,
}

impl TierPreview {
    fn new(provider: Arc<dyn PreviewProvider>) -> TierPreview {
        TierPreview {
            provider,
            image: None,
            ticket: None,
            texture: None,
            tier: None,
        }
    }

    /// Ensures a request is (or was) in flight for `image`; a no-op if
    /// already tracking it. Switching to a different image cancels any
    /// still-pending request and drops the previous texture.
    fn activate(&mut self, image: ImageId) {
        if self.image == Some(image) {
            return;
        }
        self.clear();
        self.image = Some(image);
        self.ticket = Some(
            self.provider
                .request(image, PreviewClass::Loupe, Class::Interactive),
        );
    }

    /// Releases everything (canvas exit / a new activation).
    fn clear(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            self.provider.cancel(&ticket);
        }
        self.texture = None;
        self.tier = None;
        self.image = None;
    }

    /// Per-frame poll. Returns `true` the frame a texture first becomes
    /// available (C4: "shows pixels on the next frame").
    fn pump(&mut self, ctx: &egui::Context) -> bool {
        let Some(ticket) = &self.ticket else {
            return false;
        };
        match self.provider.poll(ticket) {
            PreviewState::Pending => false,
            PreviewState::Ready(img) => {
                let color = egui::ColorImage::from_rgba_unmultiplied(
                    [img.width as usize, img.height as usize],
                    &img.px,
                );
                let tex =
                    ctx.load_texture("canvas-tier-preview", color, egui::TextureOptions::LINEAR);
                self.texture = Some((tex, [img.width, img.height]));
                self.tier = Some(img.tier);
                self.ticket = None;
                true
            }
            PreviewState::Failed(_) => {
                // No tier available, the engine frame (or, if that also
                // fails, the `RenderFailed` placard) is the fallback; E03
                // already logs the underlying cause.
                self.ticket = None;
                false
            }
        }
    }

    fn is_pending(&self) -> bool {
        self.ticket.is_some()
    }

    fn texture(&self) -> Option<(egui::TextureId, [u32; 2])> {
        self.texture.as_ref().map(|(t, s)| (t.id(), *s))
    }

    fn tier(&self) -> Option<SourceTier> {
        self.tier
    }
}

// ─── The engine texture (unchanged zero-copy mechanism from `loupe.rs`) ────

/// The engine texture currently composited (registered with egui).
///
/// It deliberately does not carry the generation it came from: the
/// "already consumed this one" test belongs to `EditorCanvas::last_generation`,
/// which counts every frame taken off the channel, including the
/// before-capture frames that never become `Displayed` at all.
struct Displayed {
    texture_id: egui::TextureId,
    size: [u32; 2],
}

// ─── `EditorCanvas` ─────────────────────────────────────────────────────────

/// The editor canvas, see the module docs.
pub struct EditorCanvas {
    render_state: eframe::egui_wgpu::RenderState,
    outcome: Arc<ShellOutcome>,
    canvas_rx: Option<tokio::sync::watch::Receiver<CanvasFrame>>,
    displayed: Option<Displayed>,
    tier: TierPreview,
    progressive: ProgressiveDisplay,
    last_key: Option<SubmitKey>,
    /// Set on every dispatch, cleared once a newer generation is observed
    /// the push-model's "still waiting on a fresher frame" flag.
    awaiting_frame: bool,
    last_quality: Option<OutputQuality>,
    zoom: ZoomMode,
    pan: egui::Vec2,
    /// D2 `view.zoom_in`/`out`: ladder steps queued by [`Self::zoom_step`]
    /// (keymap), applied in `ready_ui` where the Fit percent is known.
    pending_zoom_steps: i32,
    /// Navigation → texture-swap latency probe (T26 AC, carried forward:
    /// < 50 ms p95).
    nav_started: Option<Instant>,
    /// Rolling nav-swap latencies (ms), newest last, bounded.
    pub nav_swap_ms: Vec<f32>,
    /// H2: canvas frame counter (this widget's `ui` calls).
    frame_no: u64,
    /// H2: the last `recipe_rev` observed + the frame it was first seen.
    rev_seen: Option<(u64, u64)>,
    /// H2 slider→submit probe: lag in *frames* between the canvas first
    /// observing a new `recipe_rev` and the recipe-only engine submit it
    /// causes. 0 = same frame (the §7 budget: E08 adds ≤ 1 frame). Newest
    /// last, bounded.
    pub slider_submit_lag_frames: Vec<u32>,
    /// E10 D12: the live histogram reduction over every freshly published
    /// canvas frame (non-blocking, see `HistogramPass`'s own doc). `None`
    /// on a device that failed to build the pipeline (degrades silently;
    /// the develop histogram panel shows its empty state).
    hist_pass: Option<HistogramPass>,
    /// D12: the newest collected reduction, read each frame by the develop
    /// histogram panel through [`EditBinding::histogram`]
    /// (`panels::develop_ctx`).
    ///
    /// [`EditBinding::histogram`]: crate::panels::develop_ctx::EditBinding::histogram
    latest_histogram: Option<HistogramData>,
    /// E10 D13: the `J`-key canvas clip-overlay pass (texture→texture, no
    /// CPU readback, see `ClipOverlayPass`'s own doc).
    clip_overlay_pass: Option<ClipOverlayPass>,
    /// D13: whether the clip overlay is currently composited over the
    /// displayed frame (toggled by the `J` keymap action).
    clip_overlay_enabled: bool,
    /// The before/after view: mode, split divider, and the captured
    /// before image (`canvas/before_after.rs`).
    before_after: BeforeAfterView,
    /// The newest canvas generation this widget has consumed, whether it
    /// went to the display or to the before snapshot.
    ///
    /// Distinct from `displayed.generation` on purpose: a before-capture
    /// frame is consumed without ever becoming `displayed`, so keying the
    /// "have I already taken this one" test off `displayed` would hand the
    /// same frame back on the next poll and composite the BEFORE image as
    /// the after one.
    last_generation: u64,
}

impl EditorCanvas {
    /// A canvas compositing through `render_state`'s egui renderer,
    /// sampling `scheduler`'s canvas publisher and requesting progressive
    /// tiers through `previews` (E03's `PreviewProvider`, spec §6.4).
    pub fn new(
        render_state: eframe::egui_wgpu::RenderState,
        outcome: Arc<ShellOutcome>,
        scheduler: &RenderScheduler,
        previews: Arc<dyn PreviewProvider>,
    ) -> EditorCanvas {
        // D12/D13: both passes record on the SAME shared device the canvas
        // itself composites on (§2.3 seam 2), `RenderState`'s `device`/
        // `queue` are themselves cheap-clone wgpu handles; the `Arc` wrap
        // matches the `HistogramPass`/`ClipOverlayPass` constructors' shared-
        // device convention (`DeviceCtx`'s own, engine-wide).
        let device = Arc::new(render_state.device.clone());
        let queue = Arc::new(render_state.queue.clone());
        EditorCanvas {
            render_state,
            outcome,
            canvas_rx: scheduler.canvas(),
            displayed: None,
            tier: TierPreview::new(previews),
            progressive: ProgressiveDisplay::default(),
            last_key: None,
            awaiting_frame: false,
            last_quality: None,
            zoom: ZoomMode::Fit,
            pan: egui::Vec2::ZERO,
            pending_zoom_steps: 0,
            nav_started: None,
            nav_swap_ms: Vec::new(),
            frame_no: 0,
            rev_seen: None,
            slider_submit_lag_frames: Vec::new(),
            hist_pass: Some(HistogramPass::new(Arc::clone(&device), Arc::clone(&queue))),
            latest_histogram: None,
            clip_overlay_pass: Some(ClipOverlayPass::new(
                Arc::clone(&device),
                Arc::clone(&queue),
            )),
            clip_overlay_enabled: false,
            // Same shared device again: the before snapshot's texel copy
            // records on the device the canvas composites on, so the
            // captured texture is directly registerable with egui.
            before_after: BeforeAfterView::new(device, queue),
            last_generation: 0,
        }
    }

    /// `view.before_after_cycle` (keymap): after only, side by side,
    /// split, back to after only. Before-only is the momentary `\` hold
    /// and is deliberately not on this cycle (see [`BeforeAfterMode`]).
    pub fn cycle_before_after(&mut self) {
        self.before_after.cycle();
    }

    /// Host bytes the captured before image is holding, `0` when none is
    /// captured. The whole memory cost of this feature; see
    /// `canvas/before_after.rs`'s module docs for the analysis.
    pub fn before_snapshot_bytes(&self) -> usize {
        self.before_after.snapshot.bytes()
    }

    /// D12: the most recently collected histogram reduction (the develop
    /// histogram panel's data source, via `EditBinding::histogram`).
    pub fn latest_histogram(&self) -> Option<&HistogramData> {
        self.latest_histogram.as_ref()
    }

    /// D13: whether the `J`-key clip overlay is currently on.
    pub fn clip_overlay_enabled(&self) -> bool {
        self.clip_overlay_enabled
    }

    /// D13: toggles the clip overlay (the `J` keymap action's target).
    pub fn set_clip_overlay(&mut self, enabled: bool) {
        self.clip_overlay_enabled = enabled;
    }

    /// Resets for a freshly activated entry and kicks off its progressive
    /// tier request immediately (spec §6.4: "composite the best-available
    /// preview tier immediately").
    pub fn enter(&mut self, image: ImageId) {
        self.zoom = ZoomMode::Fit;
        self.pan = egui::Vec2::ZERO;
        self.pending_zoom_steps = 0;
        self.last_key = None; // force a submit for the (possibly new) image
        self.progressive = ProgressiveDisplay::default();
        self.tier.activate(image);
        self.nav_started = Some(Instant::now());
    }

    /// Releases egui/GPU resources when leaving the canvas (no active
    /// `Ready` entry any more).
    pub fn exit(&mut self) {
        {
            let mut renderer = self.render_state.renderer.write();
            if let Some(old) = self.displayed.take() {
                renderer.free_texture(&old.texture_id);
            }
            // The captured before image belongs to the image that just
            // left; releasing it here is what keeps this feature's memory
            // cost bounded at one snapshot, never one per visited image.
            self.before_after.snapshot.release(&mut renderer);
        }
        self.last_key = None;
        self.tier.clear();
        self.progressive = ProgressiveDisplay::default();
        // D12: don't show a stale histogram for the image that just left.
        self.latest_histogram = None;
    }

    /// True while a render or a tier-preview decode is in flight (keep
    /// repainting). A before capture counts: its frame arrives on the same
    /// watch channel and needs a frame to be polled on.
    pub fn busy(&self) -> bool {
        self.awaiting_frame || self.tier.is_pending() || self.before_after.snapshot.is_capturing()
    }

    /// The current zoom mode (top-bar view control).
    pub fn zoom_mode(&self) -> ZoomMode {
        self.zoom
    }

    /// Toggles Fit ↔ 100 % (top-bar button / Z / Space / double-click
    /// spec C2: a binary toggle, distinct from the wheel ladder).
    pub fn toggle_zoom_button(&mut self) {
        self.toggle_zoom();
    }

    fn toggle_zoom(&mut self) {
        self.zoom = match self.zoom {
            ZoomMode::Fit => ZoomMode::Percent(1.0),
            ZoomMode::Percent(_) => ZoomMode::Fit,
        };
        self.pan = egui::Vec2::ZERO;
    }

    /// D2 `view.zoom_in`/`view.zoom_out` (keymap): queues ladder steps to
    /// apply on the next rendered frame, where the Fit percent is known
    /// anchored at the view center (the wheel path anchors at the cursor).
    pub fn zoom_step(&mut self, delta: i32) {
        self.pending_zoom_steps += delta;
    }

    /// Renders one canvas frame. `content` is the caller's per-frame
    /// projection of the active working-set entry (spec C5); `idx`/`total`
    /// feed the info overlay's position readout; `device_degraded` is
    /// `Some(reason)` while the engine's device is degraded (spec C5, a
    /// non-modal chip, never blocking); `gizmos` is the app-owned Phase-F
    /// gizmo layer (spec §6.6), routed before pan/zoom and painted above
    /// the composited image while `content` is `Ready` (the other arms
    /// have no `ViewXform` to route/paint against; an active gizmo stays
    /// cancellable through the keymap the whole time).
    ///
    /// Keyboard input never arrives here: as of Phase D the keymap
    /// dispatcher (`keymap/dispatch.rs`) runs before any widget and drives
    /// nav/zoom through [`WorkingSetView::nav`]-in-`lib.rs` /
    /// [`Self::toggle_zoom_button`] / [`Self::zoom_step`], the pre-D
    /// per-widget T26 key handler collapsed into `nav.*`/`view.*` actions
    /// exactly as its TODO promised. `gizmo.cancel`/`gizmo.commit` reach
    /// the layer the same way (`lib.rs::handle_action`), never through
    /// this method.
    ///
    /// [`WorkingSetView::nav`]: crate::working_set::WorkingSetView::nav
    #[allow(clippy::too_many_arguments)]
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        scheduler: &RenderScheduler,
        max_tex_dim: u32,
        content: CanvasContent<'_>,
        recipe_source: &mut dyn RecipeSource,
        gizmos: &mut GizmoLayer,
        idx: usize,
        total: usize,
        device_degraded: Option<&str>,
    ) {
        let view_rect = ui.available_rect_before_wrap();

        match content {
            CanvasContent::Ready(entry) => {
                self.ready_ui(
                    ui,
                    view_rect,
                    scheduler,
                    max_tex_dim,
                    entry,
                    recipe_source,
                    gizmos,
                    idx,
                    total,
                );
            }
            CanvasContent::Loading => {
                ui.allocate_rect(view_rect, egui::Sense::hover());
                states::placard_ui(ui, view_rect, &CanvasPlacard::Loading);
            }
            CanvasContent::SourceFailed { reason } => {
                ui.allocate_rect(view_rect, egui::Sense::hover());
                states::placard_ui(
                    ui,
                    view_rect,
                    &CanvasPlacard::SourceFailed {
                        reason: reason.to_owned(),
                    },
                );
            }
            CanvasContent::Duplicate { of } => {
                ui.allocate_rect(view_rect, egui::Sense::hover());
                states::placard_ui(ui, view_rect, &CanvasPlacard::Duplicate { of });
            }
        }

        // C5: the device-degraded chip is orthogonal to `content`, it can
        // show over any state, and never steals focus/input (states.rs).
        if let Some(reason) = device_degraded {
            states::chip_ui(
                ui,
                view_rect,
                1,
                &format!("GPU device degraded: {reason}"),
                false,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ready_ui(
        &mut self,
        ui: &mut egui::Ui,
        view_rect: egui::Rect,
        scheduler: &RenderScheduler,
        max_tex_dim: u32,
        entry: ActiveEntry<'_>,
        recipe_source: &mut dyn RecipeSource,
        gizmos: &mut GizmoLayer,
        idx: usize,
        total: usize,
    ) {
        let ppp = ui.ctx().pixels_per_point();
        let view_px = [
            ((view_rect.width() * ppp).round() as u32).clamp(1, max_tex_dim),
            ((view_rect.height() * ppp).round() as u32).clamp(1, max_tex_dim),
        ];

        // --- C3: recipe-driven submit (latest-wins coalescing is the
        // scheduler's, unchanged). ---
        self.frame_no += 1;
        let recipe = recipe_source.recipe_for(entry.image);
        // E11 Part A: the display basis (`image_px` below) follows the
        // SAME crop this frame's recipe snapshot actually carries, the
        // real committed crop normally, or (Part B) the crop tool's
        // full-frame override while armed, so compositing and gizmo
        // geometry never disagree with what the engine is about to render.
        let crop = recipe.recipe.geometry.crop;
        // H2 probe bookkeeping: remember the frame each rev was first seen.
        let rev = recipe.rev;
        if self.rev_seen.map(|(r, _)| r) != Some(rev) {
            self.rev_seen = Some((rev, self.frame_no));
        }

        // --- Before and after: what "before" is for THIS frame, and
        // whether a capture of it has to be taken (see
        // `before_after.rs`'s module docs for both). ---
        let held = before_after::hold_key_down(ui.ctx());
        self.before_after.set_hold(held);
        let before_key = BeforeKey {
            image: entry.image,
            pv: recipe.pv,
            geometry: recipe.recipe.geometry.clone(),
            out: view_px,
            clip_overlay: self.clip_overlay_enabled,
        };
        // A capture that has ended (either way) left the BEFORE recipe
        // installed in the scheduler, and it has to come out before
        // anything else renders. This is a bare `set_recipe` rather than a
        // trip through `submit_if_changed` on purpose: that function's
        // view-changed path calls `set_view`, and `RenderScheduler::set_view`
        // re-renders whichever recipe the scheduler currently holds, which
        // would dispatch one more render of the unedited image and flash it
        // onto the after side. Recording the key it restores keeps the
        // `submit_if_changed` call below a no-op when nothing else moved,
        // and a correct re-establish when the viewport did.
        if let Some(restored_out) = self.before_after.snapshot.take_restore_for(entry.image) {
            scheduler.set_recipe(entry.image, recipe.recipe.clone(), recipe.pv);
            self.awaiting_frame = true;
            self.last_key = Some(SubmitKey {
                image: entry.image,
                out: restored_out,
                recipe_rev: rev,
            });
        }

        let capturing = self.drive_before_capture(ui, scheduler, &before_key, &recipe.recipe, held);

        // A capture owns the scheduler's recipe slot for this image while
        // it is in flight (there is one recipe per image,
        // `RenderScheduler::set_recipe`), so the ordinary submit is held
        // back rather than racing it. That is what makes "the first frame
        // newer than the recorded generation is the capture" true: this
        // canvas is the only submitter, so while a capture is out there is
        // no other frame that could arrive first. The after recipe goes
        // back in through the restore block above, on the frame after the
        // capture resolves.
        if !capturing {
            match submit_if_changed(scheduler, &mut self.last_key, entry.image, view_px, recipe) {
                Some(SubmitScope::RecipeOnly) => {
                    self.awaiting_frame = true;
                    // H2: a slider/edit re-render. Lag from first observation
                    // of this rev to the submit, in frames (0 = same frame
                    // the §7 "input → submit ≤ 1 frame" budget's shell half).
                    let lag = self
                        .rev_seen
                        .map(|(_, seen_at)| (self.frame_no - seen_at) as u32)
                        .unwrap_or(0);
                    self.slider_submit_lag_frames.push(lag);
                    if self.slider_submit_lag_frames.len() > 256 {
                        self.slider_submit_lag_frames.remove(0);
                    }
                }
                Some(SubmitScope::View) => self.awaiting_frame = true,
                None => {}
            }
        }

        // --- C4: progressive tier, then engine swap. ---
        self.tier.activate(entry.image);
        if self.tier.pump(ui.ctx()) {
            self.progressive.tier_ready();
        }
        self.poll_engine(scheduler, entry.image);
        self.settle_before_capture(scheduler, entry.image);

        // --- Which arrangement is actually painted this frame. ---
        // Resolved before the zoom/pan math because side-by-side reframes
        // the image into half the width, which changes what Fit means.
        let after_tex = if self.progressive.is_engine_frame() {
            self.displayed.as_ref().map(|d| (d.texture_id, d.size))
        } else {
            self.tier.texture()
        };
        let before_tex = self.before_after.snapshot.texture_for(&before_key);
        let mode = self
            .before_after
            .effective(held, before_tex.is_some() && after_tex.is_some());
        let (before_pane, after_pane) = match mode {
            BeforeAfterMode::SideBySide => {
                before_after::side_by_side_panes(view_rect, before_after::SIDE_BY_SIDE_GUTTER_PT)
            }
            _ => (view_rect, view_rect),
        };

        // --- C2: zoom/pan. `Orientation::O1`, see the `xform.rs` module
        // docs on why the canvas doesn't orient yet (E11 is unbuilt). ---
        let response = ui.allocate_rect(view_rect, egui::Sense::click_and_drag());
        let (basis_w, basis_h) = cropped_extent(entry.width, entry.height, crop);
        let image_px = egui::vec2(basis_w as f32, basis_h as f32);
        let disp_100 = xform::oriented_size(Orientation::O1, image_px) / ppp;
        // Fit against the pane the image is framed in, not the widget: in
        // side-by-side each pane must fit the WHOLE image on its own, and
        // both panes are the same size, so one percent serves both and the
        // two sides stay locked to the same scale.
        let fit_percent = if disp_100.x > 0.0 && disp_100.y > 0.0 {
            (after_pane.width() / disp_100.x)
                .min(after_pane.height() / disp_100.y)
                .min(1.0)
        } else {
            1.0
        };

        // D2: keyboard zoom (`view.zoom_in`/`out`), queued by the keymap
        // dispatcher, same ladder as the wheel, anchored at the view
        // center (no cursor to anchor to).
        let steps = std::mem::take(&mut self.pending_zoom_steps);
        if steps != 0 {
            let old_percent = self.zoom.resolve(fit_percent);
            let mut new_percent = old_percent;
            for _ in 0..steps.unsigned_abs() {
                new_percent = ladder_step(new_percent, steps.signum());
            }
            if (new_percent - old_percent).abs() > f32::EPSILON {
                self.pan = pan_for_zoom_to_cursor(
                    self.pan,
                    old_percent / ppp,
                    new_percent / ppp,
                    after_pane.center(),
                    after_pane.center(),
                );
                self.zoom = ZoomMode::Percent(new_percent);
            }
        }

        if response.hovered() {
            let scroll = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.5 {
                if let Some(cursor) = response.hover_pos() {
                    let old_percent = self.zoom.resolve(fit_percent);
                    // Wheel forward (positive delta) zooms in, the common
                    // photo-editor convention; no existing convention in
                    // this codebase to match (the pre-Phase-C loupe had no
                    // wheel handling at all).
                    let delta = if scroll > 0.0 { 1 } else { -1 };
                    let new_percent = ladder_step(old_percent, delta);
                    if (new_percent - old_percent).abs() > f32::EPSILON {
                        let old_scale = old_percent / ppp;
                        let new_scale = new_percent / ppp;
                        self.pan = pan_for_zoom_to_cursor(
                            self.pan,
                            old_scale,
                            new_scale,
                            cursor,
                            after_pane.center(),
                        );
                        self.zoom = ZoomMode::Percent(new_percent);
                    }
                }
            }
        }

        let mut zoom_percent = self.zoom.resolve(fit_percent);

        // --- F1: gizmo input routing, BEFORE the canvas's own drag-pan /
        // double-click-zoom, so a gizmo hit wins over pan/zoom (spec §6.6).
        // Hit-testing uses this frame's zoom with the PRE-drag pan (the
        // state the pointer event actually happened against); the clamped
        // FINAL xform below is the paint authority. ---
        let hit_xform = ViewXform::new(
            image_px,
            Orientation::O1,
            after_pane,
            zoom_percent,
            ppp,
            self.pan,
        );
        let gizmo_claimed = gizmos.route(ui, &response, &hit_xform);

        // The split divider routes after the gizmo stack and before
        // pan/zoom: a gizmo is an edit affordance and outranks a view
        // affordance, and both outrank panning (`gizmo.md` convention 3).
        let divider_claimed = self.before_after.route_divider(
            ui,
            &response,
            &hit_xform,
            !gizmo_claimed && mode == BeforeAfterMode::Split,
        );
        // Keyboard parity for the divider, live while the split is shown.
        if mode == BeforeAfterMode::Split {
            let step = before_after::divider_key_delta(ui.ctx());
            if step != 0.0 {
                self.before_after.nudge_divider(step);
                // Held-key movement is time-based, so it needs frames.
                ui.ctx().request_repaint();
            }
        }
        let claimed = gizmo_claimed || divider_claimed;

        if response.double_clicked() && !claimed {
            self.toggle_zoom();
            // Same-frame application, exactly as before Phase F moved the
            // double-click check below the gizmo routing.
            zoom_percent = self.zoom.resolve(fit_percent);
        }
        if response.dragged() && !claimed {
            self.pan += response.drag_delta();
        }
        // Pan-independent footprint (via a provisional xform, pan doesn't
        // affect it) drives the clamp; the FINAL xform (built with the
        // now-clamped pan) is the one authority for both the paint rect
        // below and the gizmo paint pass.
        let provisional = ViewXform::new(
            image_px,
            Orientation::O1,
            after_pane,
            zoom_percent,
            ppp,
            self.pan,
        );
        self.pan = clamp_pan(
            self.pan,
            provisional.display_size_screen(),
            after_pane.size(),
        );
        let xform = ViewXform::new(
            image_px,
            Orientation::O1,
            after_pane,
            zoom_percent,
            ppp,
            self.pan,
        );
        // The before pane's transform differs from the after pane's only
        // in which rect it centres on, so the same zoom and the same pan
        // drive both: the two sides can never drift out of register.
        let before_xform = ViewXform::new(
            image_px,
            Orientation::O1,
            before_pane,
            zoom_percent,
            ppp,
            self.pan,
        );

        // --- Composite whichever source is authoritative this frame. ---
        let showing_engine = self.progressive.is_engine_frame();
        let tex = after_tex;
        match tex {
            Some((texture_id, size)) => {
                // The one authority for WHERE the image lands is the same
                // `ViewXform` the gizmo layer hit-tests and paints against
                // (spec §6.4: "one authority" for compositing AND gizmos).
                // The arrangement only decides which texture is clipped
                // into which part of it.
                before_after::paint_arrangement(
                    ui,
                    &before_after::Arrangement {
                        mode,
                        view_rect,
                        before_pane,
                        after_pane,
                        image_px,
                        before_xform: &before_xform,
                        after_xform: &xform,
                        before: before_tex.map(|(id, _)| id),
                        after: texture_id,
                        divider: self.before_after.divider(),
                        dragging: self.before_after.dragging(),
                    },
                );
                let mut anchor = 0;
                if let Some(err) = self.progressive.error_chip() {
                    // §6.4: a same-image render failure keeps the last good
                    // frame + a non-modal error chip (never a placard).
                    states::chip_ui(
                        ui,
                        view_rect,
                        anchor,
                        &format!("render failed: {err}"),
                        true,
                    );
                    anchor += 1;
                }
                // The engine frame's own shape, checked rather than ignored
                // see `engine_frame_shape_fault`.
                if showing_engine {
                    if let Some(fault) = engine_frame_shape_fault(
                        size,
                        self.last_key.map(|k| k.out),
                        self.awaiting_frame,
                    ) {
                        states::chip_ui(ui, view_rect, anchor, &fault, true);
                    }
                }
            }
            None => {
                // Nothing has ever been composited for this image.
                match self.progressive.error_chip() {
                    Some(err) => states::placard_ui(
                        ui,
                        view_rect,
                        &CanvasPlacard::RenderFailed {
                            reason: err.to_owned(),
                        },
                    ),
                    None => states::placard_ui(ui, view_rect, &CanvasPlacard::Loading),
                }
            }
        }

        // A before mode that could not be honoured says so, rather than
        // quietly showing the after image twice. Anchor 2 sits above the
        // render-error (0) and device-degraded (1) chips.
        if self.before_after.wants_before(held) && mode == BeforeAfterMode::AfterOnly {
            let notice = match self.before_after.snapshot.failure() {
                Some(reason) => (format!("before unavailable: {reason}"), true),
                None => ("preparing before…".to_owned(), false),
            };
            states::chip_ui(ui, view_rect, 2, &notice.0, notice.1);
        }

        // --- F1: gizmo paint pass, above the composited image, below the
        // info overlay/chips. Zero-copy: `GizmoPaintCtx` hands gizmos the
        // ALREADY-REGISTERED texture id (engine frame or tier preview) so
        // magnifiers re-sample it with UV rects instead of reading pixels
        // (`lib.rs`'s zero-copy invariants). ---
        gizmos.paint(
            &ui.painter().with_clip_rect(view_rect),
            &xform,
            &GizmoPaintCtx { texture: tex },
        );

        self.info_overlay(ui, view_rect, &entry, idx, total, mode);
    }

    /// The published canvas generation right now, without consuming it.
    ///
    /// `watch::Receiver::borrow` (unlike `borrow_and_update`) does not
    /// mark the value seen, so this reads the publisher's true state
    /// rather than this widget's consumption state. That distinction is
    /// what makes the capture handshake safe: a frame published but not
    /// yet composited is still an AFTER frame and must not be mistaken for
    /// the before render's result.
    fn published_generation(&self) -> u64 {
        self.canvas_rx
            .as_ref()
            .map(|rx| rx.borrow().generation)
            .unwrap_or(0)
    }

    /// Starts a before capture when one is wanted and the scheduler can
    /// take it. Returns whether a capture is in flight (in which case the
    /// caller must not submit the after recipe this frame).
    ///
    /// **Why the scheduler must be idle first.** There is exactly one
    /// recipe slot per image (`RenderScheduler::set_recipe`) and one
    /// in-flight plus one pending job (`Coalescer`), so a before submit
    /// issued while something is in flight would land in the pending slot
    /// and the *next* published frame would be the other job's, not the
    /// before render's. `wait_idle(ZERO)` is a non-blocking read of that
    /// state (it checks once and returns on the already-passed deadline),
    /// and this canvas is the only submitter to the scheduler in the whole
    /// shell, so idle here means the before job dispatches immediately and
    /// the next generation is unambiguously its own.
    fn drive_before_capture(
        &mut self,
        ui: &egui::Ui,
        scheduler: &RenderScheduler,
        key: &BeforeKey,
        after: &Recipe,
        held: bool,
    ) -> bool {
        if self.before_after.snapshot.is_capturing() {
            return true;
        }
        // Wanted now (a before mode is showing, or `\` is down), or wanted
        // soon (the pre-warm, once this session has proved it uses the
        // feature, so the next `\` is a texture swap and not a render).
        let wanted = self.before_after.wants_before(held) || self.before_after.wants_prewarm();
        if !wanted || !self.before_after.snapshot.needs_capture(key) {
            return false;
        }
        if self.awaiting_frame || !scheduler.wait_idle(std::time::Duration::ZERO) {
            // Not now: keep frames coming so this is retried the moment
            // the scheduler settles, instead of waiting for the next
            // unrelated input event.
            ui.ctx().request_repaint();
            return false;
        }

        let after_generation = self.published_generation();
        scheduler.set_recipe(key.image, before_after::before_recipe(after), key.pv);
        self.before_after
            .snapshot
            .begin(key.clone(), after_generation);
        true
    }

    /// Ends a capture that is never going to produce a frame.
    ///
    /// The precise signal is "the scheduler is idle again and no newer
    /// generation was published": the before render reached a terminal
    /// state without publishing, which means it failed or was cancelled.
    /// Because the publish happens before the scheduler reports the job
    /// complete, a frame can land between this frame's `poll_engine` and
    /// this check, so the channel is polled once more before giving up.
    fn settle_before_capture(&mut self, scheduler: &RenderScheduler, image: ImageId) {
        if !self.before_after.snapshot.is_capturing() {
            return;
        }
        if self.before_after.snapshot.abort_if_stalled() {
            // Same restore debt as any other abort; keep frames coming.
            self.awaiting_frame = true;
            return;
        }
        if !scheduler.wait_idle(std::time::Duration::ZERO) {
            return;
        }
        self.poll_engine(scheduler, image);
        if self.before_after.snapshot.is_capturing() {
            let reason = scheduler
                .last_error(image)
                .unwrap_or_else(|| "the render produced no frame".to_owned());
            self.before_after.snapshot.abort(reason);
            // `abort` owes the same recipe restore a successful capture
            // does; keep frames coming until it happens.
            self.awaiting_frame = true;
        }
    }

    /// Samples the canvas watch channel for a newer generation; swaps the
    /// composited texture and feeds the [`ProgressiveDisplay`] state
    /// machine.
    fn poll_engine(&mut self, scheduler: &RenderScheduler, image: ImageId) {
        if let Some(err) = scheduler.last_error(image) {
            self.progressive.engine_failed(err);
        }

        // D12: drain any histogram reduction that finished, independent of
        // whether a NEW canvas frame arrives below, a cheap, non-blocking
        // poll every call (see `HistogramPass`'s own doc: `poll()` never
        // waits, whether or not a result is ready).
        if let Some(hist) = &mut self.hist_pass {
            if let Some((_gen, data)) = hist.poll() {
                self.latest_histogram = Some(data);
            }
        }

        let Some(rx) = &mut self.canvas_rx else {
            return;
        };
        let frame = rx.borrow_and_update();
        // Keyed on the newest generation CONSUMED, not the newest
        // displayed: a before-capture frame is consumed without becoming
        // `displayed`, and keying off `displayed` would hand it back on
        // the next poll to be composited as the after image.
        if frame.generation <= self.last_generation || frame.generation == 0 {
            return;
        }
        let (texture, quality, extent, generation) = (
            frame.texture.clone(),
            frame.quality,
            frame.extent,
            frame.generation,
        );
        drop(frame);
        self.last_generation = generation;

        // Before and after: this frame is the before render's result, not
        // the canvas's. It goes into the snapshot and nowhere else, so the
        // after frame already on screen stays on screen (no flash to the
        // unedited image mid-capture), the histogram keeps describing the
        // edit, and the navigation probe does not count a render the user
        // never asked for.
        if self.before_after.snapshot.is_capture_frame(generation) {
            let overlay_view = if self.clip_overlay_enabled {
                self.clip_overlay_pass
                    .as_mut()
                    .and_then(|pass| pass.run(&texture, extent))
            } else {
                None
            };
            let captured = overlay_view.as_ref().unwrap_or(&texture);
            let mut renderer = self.render_state.renderer.write();
            self.before_after
                .snapshot
                .accept(captured, [extent.w, extent.h], &mut renderer);
            drop(renderer);
            // The after recipe still has to be put back (the restore
            // block in `ready_ui`); keep the repaint pump alive for it.
            self.awaiting_frame = true;
            return;
        }

        // D12: kick a non-blocking reduction of the FRESH frame, skipped
        // (never blocked) on the rare frame where the ring is still
        // draining the previous dispatch.
        if let Some(hist) = &mut self.hist_pass {
            let _ = hist.try_dispatch(&texture, extent);
        }

        // D13: composite the `J`-key clip overlay when armed, a
        // texture→texture pass, no CPU readback (`ClipOverlayPass`'s own
        // doc); falls back to the plain frame if the pass failed to build.
        let overlay_view = if self.clip_overlay_enabled {
            self.clip_overlay_pass
                .as_mut()
                .and_then(|pass| pass.run(&texture, extent))
        } else {
            None
        };
        let shown = overlay_view.as_ref().unwrap_or(&texture);
        self.swap_displayed(shown, [extent.w, extent.h]);
        self.last_quality = Some(quality);
        self.progressive.engine_ready();
        self.awaiting_frame = false;
        if let Some(started) = self.nav_started.take() {
            let ms = started.elapsed().as_secs_f32() * 1000.0;
            self.nav_swap_ms.push(ms);
            if self.nav_swap_ms.len() > 256 {
                self.nav_swap_ms.remove(0);
            }
        }
    }

    /// Registers a finished engine texture with egui, releasing the
    /// previous one, flicker-free swap on the SAME shared device (seam
    /// 2). Never copies pixels.
    fn swap_displayed(&mut self, view: &wgpu::TextureView, size: [u32; 2]) {
        let mut renderer = self.render_state.renderer.write();
        let texture_id = renderer.register_native_texture(
            &self.render_state.device,
            view,
            wgpu::FilterMode::Linear,
        );
        if let Some(old) = self.displayed.replace(Displayed { texture_id, size }) {
            renderer.free_texture(&old.texture_id);
        }
        self.outcome
            .texture_swaps
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.outcome
            .seam_proven
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Info overlay (T26, carried forward): filename, dims, tier/quality
    /// badge, zoom, position.
    fn info_overlay(
        &self,
        ui: &egui::Ui,
        view_rect: egui::Rect,
        entry: &ActiveEntry<'_>,
        idx: usize,
        total: usize,
        before_after: BeforeAfterMode,
    ) {
        let quality = if self.progressive.is_engine_frame() {
            match self.last_quality {
                Some(OutputQuality::PreviewTier) => "preview".to_owned(),
                Some(OutputQuality::PreviewRes) => "preview-res".to_owned(),
                Some(OutputQuality::FullRes) => "full-res".to_owned(),
                None => "…".to_owned(),
            }
        } else {
            match self.tier.tier() {
                Some(SourceTier::EmbeddedPreview) => "embedded preview".to_owned(),
                // `SourceTier` is `#[non_exhaustive]` (future E03 tiers);
                // an unrecognized-but-present tier still badges honestly
                // rather than failing to compile against this build.
                Some(_) => "preview".to_owned(),
                None => "…".to_owned(),
            }
        };

        // The source segment, and the reason this is not folded into
        // `quality` above.
        //
        // `quality` describes the RENDER: how far the engine got this frame.
        // On an engine frame it reads "full-res", which is true of the render
        // and says nothing about what was rendered. Every image the canvas
        // draws today comes from the camera's embedded JPEG, because
        // `decode_for_develop` (the sensor path) has exactly one caller in the
        // workspace and it is `lightbox-cli`; `lightbox-shell` does not depend
        // on `lightbox-decode` at all. So on a raw file, "full-res" alone
        // reads as "you are looking at your sensor data" and the person
        // pulling a highlight back has no way to know they are working on
        // eight-bit rendered pixels with nothing left to recover.
        //
        // Say it on every frame, not only before the engine takes over, and
        // say it only for raw files, where it means something. Remove this
        // when the shell learns to ask the proxy for sensor pixels, and update
        // `web/index.html` and `README.md` in the same commit, which
        // `tools/site-checks/check_site.py` will insist on.
        // Where the pixels came from, said on every frame.
        //
        // `quality` above describes the RENDER: how far the engine got. It says
        // nothing about what was rendered, so on a raw file "full-res" alone
        // once read as "you are looking at your sensor data" when you were not.
        // This segment answers that question directly, from the router that
        // actually made the decision.
        let source = match entry.raw_pixels {
            RawPixels::Sensor => "  ·  sensor",
            RawPixels::EmbeddedPreview => "  ·  embedded preview",
            RawPixels::NotRaw => "",
        };

        // The before/after arrangement, said in the readout as well as
        // painted on the canvas. Two independent statements of the same
        // fact, because "which one am I looking at" is the question this
        // feature must never leave open. Silent in the ordinary
        // after-only state, so the readout stays quiet when nothing is
        // unusual.
        let arrangement = match before_after {
            BeforeAfterMode::AfterOnly => String::new(),
            other => format!("  ·  {}", other.label()),
        };

        let text = format!(
            "{}  ·  {}×{}  ·  {}{}  ·  {}{}  ·  {}/{}",
            entry.filename,
            entry.width,
            entry.height,
            quality,
            source,
            self.zoom.label(),
            arrangement,
            idx + 1,
            total,
        );
        // §8 placard treatment (same recipe `canvas::states::text_placard`
        // uses): `ELEV_1_PANEL @ 85%` so the canvas still reads through at
        // the edges, a 1px hairline border, `TEXT_SECONDARY` 11px text.
        let painter = ui.painter();
        let pos = view_rect.left_top() + egui::vec2(8.0, 8.0);
        let galley = painter.layout_no_wrap(text, fonts::status_bar_font(), tokens::TEXT_SECONDARY);
        let pad = egui::vec2(tokens::SPACE_2, 4.0);
        let bg = egui::Rect::from_min_size(pos, galley.size() + pad * 2.0);
        let card_bg = {
            let c = tokens::ELEV_1_PANEL;
            egui::Color32::from_rgba_unmultiplied(
                c.r(),
                c.g(),
                c.b(),
                (255.0 * tokens::CANVAS_PLACARD_BG_ALPHA).round() as u8,
            )
        };
        painter.rect_filled(bg, tokens::RADIUS_CONTROL, card_bg);
        painter.rect_stroke(
            bg,
            tokens::RADIUS_CONTROL,
            egui::Stroke::new(tokens::STROKE_HAIRLINE, tokens::SEPARATOR_HAIRLINE),
            egui::StrokeKind::Inside,
        );
        painter.galley(pos + pad, galley, tokens::TEXT_SECONDARY);
    }
}

#[cfg(test)]
mod tests {

    /// The badge must name the source the router actually chose.
    ///
    /// This is the honesty surface. It said "embedded preview" for every raw
    /// file for as long as that was true, and the moment sensor decoding
    /// landed it had to stop saying it, or the app would have been lying in
    /// the opposite direction. Pin the mapping so neither can happen silently.
    #[test]
    fn the_badge_names_the_source_the_router_chose() {
        use lightbox_core::{RawFallbackReason, RawSourceStatus};

        assert_eq!(
            RawPixels::from(&RawSourceStatus::Sensor {
                width: 4310,
                height: 2870
            }),
            RawPixels::Sensor
        );
        assert_eq!(
            RawPixels::from(&RawSourceStatus::FellBack {
                reason: RawFallbackReason::ProxyUnavailable
            }),
            RawPixels::EmbeddedPreview,
            "a fallback must never be allowed to read as sensor data"
        );
        assert_eq!(
            RawPixels::from(&RawSourceStatus::FellBack {
                reason: RawFallbackReason::DecodeFailed("signal 9".to_owned())
            }),
            RawPixels::EmbeddedPreview
        );
        assert_eq!(RawPixels::from(&RawSourceStatus::NotRaw), RawPixels::NotRaw);
    }
    use super::*;

    // ── E11 canvas-display fix, Part A: `cropped_extent` (pure math) ────

    /// **AC:** with a committed, non-identity crop, the display basis is
    /// the CROPPED extent (cropped aspect), not the full source dims.
    /// Numbers mirror `geom::crop::tests::output_extent_matches_the_rounded_crop_rect`
    /// (`crates/lightbox-render/src/ng/nodes/geometry/crop.rs`) exactly, on
    /// the shell side.
    #[test]
    fn cropped_extent_matches_the_engine_crop_nodes_rounding_for_a_committed_crop() {
        let crop = Crop {
            left: 0.25,
            top: 0.0,
            right: 0.75,
            bottom: 1.0,
        };
        let got = cropped_extent(400, 200, crop);
        assert_eq!(
            got,
            (200, 200),
            "the CROPPED extent, not the full 400x200 source"
        );
    }

    /// **AC (regression bar):** with no crop (`Crop::default()`, the
    /// identity value, and also what the crop tool's own Part-B override
    /// resets `geometry.crop` to), `image_px` stays EXACTLY the full
    /// source dims, bit-identical to the pre-E11 behavior C1-C4 pin.
    #[test]
    fn cropped_extent_is_bit_identical_to_the_source_dims_when_uncropped() {
        assert_eq!(cropped_extent(4032, 3024, Crop::default()), (4032, 3024));
        assert_eq!(cropped_extent(1, 1, Crop::default()), (1, 1));
    }

    /// Degenerate rounding (a crop narrower than half a source pixel)
    /// folds to a minimal 1px window, never an empty/zero one, mirrors
    /// `crop.rs::crop_pixels_from_params`'s own degenerate-fold rule.
    #[test]
    fn cropped_extent_folds_a_degenerate_rect_to_a_minimal_one_pixel_window() {
        let sliver = Crop {
            left: 0.5,
            top: 0.5,
            right: 0.5001,
            bottom: 0.5001,
        };
        let (w, h) = cropped_extent(10, 10, sliver);
        assert!(w >= 1 && h >= 1, "never a zero-area window: {w}x{h}");
    }

    // ── C2: zoom ladder & pan (pure math) ───────────────────────────────

    #[test]
    fn ladder_steps_up_and_down_and_clamps_at_the_ends() {
        assert_eq!(ladder_step(1.0, 1), 2.0);
        assert_eq!(ladder_step(2.0, 1), 2.0, "clamped at the top rung");
        assert_eq!(ladder_step(0.25, -1), 0.25, "clamped at the bottom rung");
        assert_eq!(ladder_step(0.5, -1), 0.25);
        assert_eq!(
            ladder_step(0.6, 1),
            1.0,
            "steps up from an in-between value"
        );
        assert_eq!(
            ladder_step(0.6, -1),
            0.5,
            "steps down from an in-between value"
        );
        assert_eq!(ladder_step(1.0, 0), 1.0, "no delta, no change");
    }

    #[test]
    fn clamp_pan_locks_to_center_when_the_image_fits_the_view() {
        let pan = clamp_pan(
            egui::vec2(50.0, 50.0),
            egui::vec2(100.0, 80.0),
            egui::vec2(400.0, 300.0),
        );
        assert_eq!(
            pan,
            egui::Vec2::ZERO,
            "a smaller-than-view image can't be panned"
        );
    }

    #[test]
    fn clamp_pan_bounds_to_half_the_overhang() {
        // display 1000x800 in a 400x300 view: overhang is (600,500), half
        // is (300,250), pan can range ±that.
        let pan = clamp_pan(
            egui::vec2(1000.0, 1000.0),
            egui::vec2(1000.0, 800.0),
            egui::vec2(400.0, 300.0),
        );
        assert_eq!(pan, egui::vec2(300.0, 250.0));
        let pan = clamp_pan(
            egui::vec2(-1000.0, -1000.0),
            egui::vec2(1000.0, 800.0),
            egui::vec2(400.0, 300.0),
        );
        assert_eq!(pan, egui::vec2(-300.0, -250.0));
    }

    /// C2 AC: zoom-to-cursor keeps the cursor's image point fixed within
    /// 1 px.
    #[test]
    fn zoom_to_cursor_keeps_the_cursor_image_point_fixed_within_a_pixel() {
        let center = egui::pos2(200.0, 150.0);
        let cursor = egui::pos2(260.0, 120.0);
        let pan0 = egui::vec2(10.0, -5.0);
        for &(s0, s1) in &[(1.0_f32, 2.0), (2.0, 1.0), (0.5, 4.0), (1.0, 1.0)] {
            let pan1 = pan_for_zoom_to_cursor(pan0, s0, s1, cursor, center);
            let img_rel = (cursor - center - pan0) / s0;
            let screen_after = center + pan1 + img_rel * s1;
            assert!(
                (screen_after - cursor).length() < 1.0,
                "s0={s0} s1={s1}: {screen_after:?} vs {cursor:?}"
            );
        }
    }

    // ── C4: progressive display (pure decision core) ────────────────────

    #[test]
    fn progressive_display_tier_then_engine_then_same_image_failure_keeps_last_frame() {
        let mut p = ProgressiveDisplay::default();
        assert!(!p.has_content());
        assert!(!p.is_engine_frame());

        // Nothing composited yet + a render failure ⇒ the RenderFailed
        // placard path (no content to fall back to).
        p.engine_failed("boom".to_owned());
        assert!(!p.has_content());
        assert_eq!(p.error_chip(), Some("boom"));

        // The tier preview lands: composites immediately, clears the
        // placard-worthy error.
        assert!(p.tier_ready(), "first tier arrival swaps it in");
        assert!(p.has_content());
        assert!(!p.is_engine_frame());
        assert_eq!(p.error_chip(), None);

        // A second `tier_ready` is a no-op, it never re-swaps once
        // something shows.
        assert!(!p.tier_ready());

        // The engine frame lands: exactly one swap, tier → engine.
        p.engine_ready();
        assert!(p.is_engine_frame());

        // A same-image render failure AFTER a good frame exists keeps the
        // last frame (has_content stays true) and surfaces the chip.
        p.engine_failed("device hiccup".to_owned());
        assert!(p.has_content());
        assert!(
            p.is_engine_frame(),
            "the stale engine frame stays composited"
        );
        assert_eq!(p.error_chip(), Some("device hiccup"));

        // A fresh success clears the chip.
        p.engine_ready();
        assert_eq!(p.error_chip(), None);
    }

    #[test]
    fn progressive_display_default_is_blank() {
        // What `EditorCanvas::enter` resets to on every activation.
        let p = ProgressiveDisplay::default();
        assert!(!p.has_content());
        assert!(!p.is_engine_frame());
        assert_eq!(p.error_chip(), None);
    }

    // ── C4: `TierPreview` (headless, no GPU, `egui::Context::default()`) ──

    mod tier_preview_tests {
        use super::*;
        use lightbox_preview::{DecodedImage, PreviewColorspace};
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Mutex;

        #[derive(Default)]
        struct StubProvider {
            next: AtomicU64,
            ready: Mutex<HashMap<u64, PreviewState>>,
            requests: Mutex<Vec<ImageId>>,
            cancels: Mutex<Vec<u64>>,
        }

        impl StubProvider {
            fn set_ready(&self, ticket: u64, w: u32, h: u32) {
                let px: Arc<[u8]> = vec![255u8; (w * h * 4) as usize].into();
                self.ready.lock().unwrap().insert(
                    ticket,
                    PreviewState::Ready(Arc::new(DecodedImage {
                        px,
                        width: w,
                        height: h,
                        // A UI stub: plain sRGB, the space every untagged
                        // source is assumed to be (spec §5.3).
                        colorspace: PreviewColorspace::Srgb,
                        orientation_applied: true,
                        tier: SourceTier::EmbeddedPreview,
                    })),
                );
            }
        }

        impl PreviewProvider for StubProvider {
            fn request(&self, image: ImageId, _class: PreviewClass, _prio: Class) -> PreviewTicket {
                let id = self.next.fetch_add(1, Ordering::Relaxed);
                self.requests.lock().unwrap().push(image);
                PreviewTicket::new(id)
            }
            fn poll(&self, t: &PreviewTicket) -> PreviewState {
                self.ready
                    .lock()
                    .unwrap()
                    .get(&t.id())
                    .cloned()
                    .unwrap_or(PreviewState::Pending)
            }
            fn cancel(&self, t: &PreviewTicket) {
                self.cancels.lock().unwrap().push(t.id());
            }
        }

        /// C4 AC: activating an entry composites pixels the next frame a
        /// decode lands (no blank), badged with the tier that produced it.
        #[test]
        fn requests_on_activate_and_uploads_on_ready() {
            let provider = Arc::new(StubProvider::default());
            let ctx = egui::Context::default();
            let mut tier = TierPreview::new(Arc::clone(&provider) as Arc<dyn PreviewProvider>);
            tier.activate(ImageId(1));
            assert_eq!(provider.requests.lock().unwrap().len(), 1);
            assert!(!tier.pump(&ctx), "still pending: no swap yet");
            assert!(tier.texture().is_none());

            provider.set_ready(0, 16, 12);
            assert!(tier.pump(&ctx), "first ready poll composites this frame");
            let (_, size) = tier.texture().expect("texture uploaded");
            assert_eq!(size, [16, 12]);
            assert_eq!(tier.tier(), Some(SourceTier::EmbeddedPreview));

            // Re-activating the SAME image is a no-op.
            tier.activate(ImageId(1));
            assert_eq!(provider.requests.lock().unwrap().len(), 1);

            // A DIFFERENT image clears the old texture and requests fresh.
            tier.activate(ImageId(2));
            assert_eq!(provider.requests.lock().unwrap().len(), 2);
            assert!(tier.texture().is_none());
        }

        #[test]
        fn cancels_the_inflight_request_on_reactivate() {
            let provider = Arc::new(StubProvider::default());
            let mut tier = TierPreview::new(Arc::clone(&provider) as Arc<dyn PreviewProvider>);
            tier.activate(ImageId(1));
            tier.activate(ImageId(2));
            assert_eq!(
                provider.cancels.lock().unwrap().len(),
                1,
                "the still-pending request for image 1 is cancelled"
            );
        }
    }

    // ── C3: recipe-driven submit, integration against a REAL headless
    //    (CPU-forced, no GPU) `RenderScheduler`, mirroring
    //    `lightbox-render/tests/ng_f5_canvas.rs`'s
    //    `f5_scheduler_without_canvas_stays_on_buffer_targets` pattern. ────

    mod headless_engine {
        use super::*;
        use lightbox_jobs::{CancelToken, JobConfig, JobSystem};
        use lightbox_render::ng::nodes::decoded::{SrcDecodedFactory, SrcDecodedNode};
        use lightbox_render::ng::nodes::display::{XformDisplayFactory, XformDisplayNode};
        use lightbox_render::ng::nodes::resize::{UtilResizeFactory, UtilResizeNode};
        use lightbox_render::ng::source::DeviceHandles;
        use lightbox_render::ng::{
            BackendPref, BoxFuture, DeviceError, DeviceProvider, Engine, EngineConfig, JobsHandle,
            NodeRegistry, PixelBuf, PixelFormat, PvRange, SourceColorimetry, SourceError,
            SourceImage, SourceProvider, SourceQuality, SourceWant,
        };

        struct NullDevice;
        impl DeviceProvider for NullDevice {
            fn current(&self) -> DeviceHandles {
                unimplemented!("CPU-only engine never calls current()")
            }
            fn rebuild(&self) -> BoxFuture<'static, Result<DeviceHandles, DeviceError>> {
                Box::pin(async { Err(DeviceError::Rebuild("no device in test".to_owned())) })
            }
        }

        struct SynthSource {
            pixels: PixelBuf,
        }
        impl SourceProvider for SynthSource {
            fn fetch(
                &self,
                _: ImageId,
                _: SourceWant,
                _: &CancelToken,
            ) -> BoxFuture<'static, Result<SourceImage, SourceError>> {
                let pixels = self.pixels.clone();
                Box::pin(async move {
                    let full_extent = pixels.extent;
                    Ok(SourceImage {
                        pixels,
                        colorimetry: SourceColorimetry::default(),
                        full_extent,
                        quality: SourceQuality::Preview,
                    })
                })
            }
        }

        /// A headless (CPU-forced) `RenderScheduler` driving the real PV1
        /// node graph, no wgpu device anywhere, so this exercises the
        /// REAL scheduler/coalescing machinery C3 submits against, without
        /// needing a live `RenderState` (that GPU leg, actual texture
        /// registration, is smoke-only, the same posture the
        /// pre-Phase-C loupe already had for `swap_displayed`).
        pub(super) fn build() -> RenderScheduler {
            let mut reg = NodeRegistry::new();
            reg.register(
                SrcDecodedNode::ID,
                PvRange::from_open(PV_M0),
                Arc::new(SrcDecodedFactory::default()),
            )
            .unwrap();
            reg.register(
                UtilResizeNode::ID,
                PvRange::from_open(PV_M0),
                Arc::new(UtilResizeFactory::default()),
            )
            .unwrap();
            reg.register(
                XformDisplayNode::ID,
                PvRange::from_open(PV_M0),
                Arc::new(XformDisplayFactory::default()),
            )
            .unwrap();

            let dp: Arc<dyn DeviceProvider> = Arc::new(NullDevice);
            let mut px = PixelBuf::new_zeroed(PixelFormat::Rgba8Srgb, Extent { w: 8, h: 8 });
            for y in 0..8 {
                for x in 0..8 {
                    px.set_rgba_f32(x, y, [0.2, 0.4, 0.6, 1.0]);
                }
            }
            let sp: Arc<dyn SourceProvider> = Arc::new(SynthSource { pixels: px });
            let engine = Arc::new(
                Engine::new(
                    dp,
                    sp,
                    reg,
                    EngineConfig {
                        backend: BackendPref::ForceCpu,
                        ..EngineConfig::default()
                    },
                )
                .unwrap(),
            );
            let jobs = JobsHandle(Arc::new(JobSystem::new(JobConfig::default())));
            RenderScheduler::new(engine, jobs)
        }
    }

    fn snap(rev: u64) -> RecipeSnapshot {
        RecipeSnapshot {
            recipe: Recipe::identity(PV_M0),
            pv: PV_M0,
            rev,
        }
    }

    /// The compositing seam's shape guard: a settled frame whose extent
    /// disagrees with the submitted viewport is surfaced, an in-flight one is
    /// not, and a matching one is silent. The guard exists because this seam
    /// used to discard the frame's size outright, which is what let a
    /// wrong-shaped frame become a silent stretch instead of a visible fault.
    #[test]
    fn engine_frame_shape_fault_reports_only_a_settled_mismatch() {
        // Matching extents: never a fault, awaiting or not.
        assert_eq!(
            engine_frame_shape_fault([920, 800], Some([920, 800]), false),
            None
        );
        assert_eq!(
            engine_frame_shape_fault([920, 800], Some([920, 800]), true),
            None
        );
        // Mismatched but a fresher frame is still coming, the ordinary
        // rail-open / window-resize transient, deliberately not reported.
        assert_eq!(
            engine_frame_shape_fault([1200, 800], Some([920, 800]), true),
            None
        );
        // Mismatched and settled: the stale-cached-extent fault.
        let fault = engine_frame_shape_fault([1200, 800], Some([920, 800]), false)
            .expect("a settled extent mismatch must be surfaced");
        assert!(
            fault.contains("1200×800") && fault.contains("920×800"),
            "the chip must name both extents, got {fault:?}"
        );
        // Nothing submitted yet ⇒ nothing to compare against.
        assert_eq!(engine_frame_shape_fault([920, 800], None, false), None);
    }

    /// C3 AC: a recipe change issues a new submit the same frame; no
    /// change (rev unchanged) issues no extra submit; a genuinely new
    /// image submits even at an unchanged rev.
    #[test]
    fn recipe_change_issues_a_new_submit_unchanged_rev_does_not() {
        let scheduler = headless_engine::build();
        let image = ImageId(42);
        let mut last_key = None;
        let out = [8u32, 8u32];
        let timeout = std::time::Duration::from_secs(10);

        let submitted = submit_if_changed(&scheduler, &mut last_key, image, out, snap(1));
        assert_eq!(
            submitted,
            Some(SubmitScope::View),
            "first activation of a never-seen image must submit as a view change"
        );
        assert!(scheduler.wait_idle(timeout));
        let subs_after_first = scheduler.submissions();
        assert!(subs_after_first >= 1);

        let submitted = submit_if_changed(&scheduler, &mut last_key, image, out, snap(1));
        assert_eq!(submitted, None, "an unchanged rev must not resubmit");
        assert!(scheduler.wait_idle(timeout));
        assert_eq!(
            scheduler.submissions(),
            subs_after_first,
            "no extra engine submission for an unchanged recipe_rev"
        );

        let submitted = submit_if_changed(&scheduler, &mut last_key, image, out, snap(2));
        assert_eq!(
            submitted,
            Some(SubmitScope::RecipeOnly),
            "a recipe_rev change must issue a recipe-only submit in the same frame \
             (the H2 slider→submit probe counts exactly these)"
        );
        assert!(scheduler.wait_idle(timeout));
        assert!(
            scheduler.submissions() > subs_after_first,
            "the recipe_rev change must have produced a new engine submission"
        );
        let subs_after_change = scheduler.submissions();

        let submitted = submit_if_changed(&scheduler, &mut last_key, ImageId(99), out, snap(2));
        assert_eq!(
            submitted,
            Some(SubmitScope::View),
            "a new image must submit even at an unchanged recipe_rev"
        );
        assert!(scheduler.wait_idle(timeout));
        assert!(scheduler.submissions() > subs_after_change);
    }

    /// The [`RecipeSource`] trait is dyn-compatible and feeds
    /// `submit_if_changed` exactly as `EditorCanvas::ui` will (proves the
    /// C3 seam end-to-end, headless).
    #[test]
    fn recipe_source_trait_object_feeds_submit_if_changed() {
        struct TestRecipeSource {
            rev: u64,
        }
        impl RecipeSource for TestRecipeSource {
            fn recipe_for(&mut self, _image: ImageId) -> RecipeSnapshot {
                snap(self.rev)
            }
        }

        let scheduler = headless_engine::build();
        let mut source = TestRecipeSource { rev: 1 };
        let source: &mut dyn RecipeSource = &mut source;
        let mut last_key = None;
        let image = ImageId(7);
        let out = [8u32, 8u32];

        let submitted = submit_if_changed(
            &scheduler,
            &mut last_key,
            image,
            out,
            source.recipe_for(image),
        );
        assert_eq!(submitted, Some(SubmitScope::View));
        assert!(scheduler.wait_idle(std::time::Duration::from_secs(10)));
    }
}
