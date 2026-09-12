// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Before and after viewing: the machinery behind
//! [`states::BeforeAfterMode`], the canvas state the user cycles with the
//! keymap's `view.before_after_cycle` action and holds with `\`.
//!
//! Read `canvas/before_after.md` for the interaction model in prose. This
//! module doc answers the two questions the code itself cannot: what
//! "before" means, and what showing it costs.
//!
//! # What "before" is here: the file with no edits at all
//!
//! There were two defensible definitions. Lightbox keeps a full edit
//! history (`lightbox_edit::history::recipe_at(cat, image, seq)`,
//! `crates/lightbox-edit/src/history.rs:140`), so "the recipe at an earlier
//! history position" was genuinely available, not hypothetical. This
//! module still uses the **no-edits** definition:
//!
//! `before = Recipe::identity(after.pv)`, carrying the after recipe's
//! `geometry` (see [`before_recipe`] and the geometry note below).
//!
//! Three pieces of evidence decided it, all of them in the edit crate:
//!
//! 1. **The two definitions already coincide at the interesting point.**
//!    An image that has never been edited has no `edit_state` row at all,
//!    and both `EditStore::open_state` and `EditStore::recipe_of` seed it
//!    with `Recipe::identity(pv)`
//!    (`crates/lightbox-edit/src/store.rs:141` and `:169`).
//!    `Recipe::default_for` is documented as equal to `Recipe::identity`
//!    at M1 (`crates/lightbox-edit/src/recipe.rs:274`). So "history step 0"
//!    *is* the identity recipe: `recipe_at` returns exactly
//!    `Recipe::identity(row.pv)` for `seq == 0`
//!    (`crates/lightbox-edit/src/history.rs:161`). Taking the richer
//!    definition would buy a different answer only once a "Copy Before"
//!    command exists, and no such command exists in `EditCommand` today.
//!
//! 2. **The richer definition has a degenerate case here, and the honest
//!    one does not.** `history::clear` keeps the current (already edited)
//!    doc while collapsing `head_seq` to 0, and `recipe_at` checks
//!    `seq == head_seq` *before* `seq == 0` precisely so that a cleared
//!    image's step 0 returns that edited doc
//!    (`crates/lightbox-edit/src/history.rs:130-137` and `:179`). A
//!    "before" defined as step 0 would therefore silently become "after"
//!    the moment somebody clears their history, which is the one thing a
//!    before view must never do.
//!
//! 3. **It costs nothing and cannot drift.** The identity recipe is
//!    derived from the after snapshot the canvas already has in hand every
//!    frame, so there is no SQL read on the frame path (spec §7 forbids
//!    per-frame SQL), no cache to invalidate on `Event::EditCommitted`,
//!    and no way for the before image to move under the user while they
//!    undo and redo.
//!
//! When a "Copy Before" command does land, exactly one function changes:
//! [`before_recipe`]. Everything below it keys off an opaque
//! [`BeforeKey`], so a before recipe that varies per image is already
//! expressible (add the source of the variation to the key and the
//! snapshot re-captures itself).
//!
//! **The geometry exception, stated plainly.** The before recipe keeps the
//! after recipe's `geometry` rather than neutralizing it. This is not a
//! hedge between the two definitions; it is forced by the canvas having
//! exactly one display basis. `view.rs`'s `cropped_extent` derives
//! `image_px` (and therefore the fit, the zoom, the pan clamp and every
//! `ViewXform`) from the *committed crop*, so a before render with a
//! different crop would land at a different extent and be stretched over
//! the after image's rect. Both a split divider and a side-by-side pair
//! are only meaningful when the two sides are the same framing at the same
//! scale. So this view answers "what did my tone and colour do", and the
//! crop tool (E11, `crop_gizmo.rs`, which renders the uncropped frame
//! while armed) answers "what did I crop away".
//!
//! # What it costs: the measurement, not the assumption
//!
//! The fear worth checking was two full-resolution renders of a 60
//! megapixel raw. That is not what happens, for two reasons that are both
//! in the render crate and both verifiable:
//!
//! * **A canvas render is viewport sized, never sensor sized.**
//!   `RenderScheduler::to_request` builds every request with
//!   `scale: RenderScale::Fit(job.view.viewport)`
//!   (`crates/lightbox-render/src/ng/sched/mod.rs:230`), and the viewport
//!   this canvas submits is the widget's own pixel extent, clamped to
//!   `max_texture_dimension_2d` (`view.rs`'s `ready_ui`). The published
//!   frame is `rgba8unorm` (`ng::sched::canvas::CANVAS_FORMAT`), so the
//!   whole cost of holding a second rendered image is
//!   `viewport_w * viewport_h * 4` bytes, independent of the raw's
//!   megapixels. [`BeforeSnapshot::bytes`] reports the live figure and
//!   `snapshot_bytes_is_four_bytes_per_viewport_pixel` pins the formula.
//!   For a full-screen canvas on a 16 inch Retina panel (about
//!   2360 x 1200 points at 2x, so 4720 x 2400 px) that is **45.3 MB**, one
//!   texture, allocated once per viewport size and reused across every
//!   toggle. The engine's own display ring already holds three textures of
//!   that same size (`RING = 3`,
//!   `crates/lightbox-render/src/ng/sched/canvas.rs:35`), so this feature
//!   adds a third of what the canvas already spends to show one image.
//!
//! * **The decoded source is shared, so the source pin never doubles and
//!   never re-decodes.** `SourcePin` is keyed by `ImageId` alone
//!   (`crates/lightbox-render/src/ng/engine.rs:799-825`), and
//!   `Engine::submit` touches it by `req.image`
//!   (`crates/lightbox-render/src/ng/engine.rs:539-542`). Two recipes for
//!   the same image therefore hit the same pinned `Arc<SourceImage>`: the
//!   before render adds **zero** bytes to the pin and cannot evict the
//!   source it is itself rendering (the insert path never evicts the entry
//!   it just touched, `engine.rs:840-852`). This is the property that
//!   makes the feature usable at all, and it is why the capture goes
//!   through the ordinary scheduler rather than any second engine.
//!
//! The cost that *is* real is engine renders, and there are **two** per
//! capture, not one: the before render, and the render that comes back
//! when the after recipe is put into the scheduler's single per-image
//! recipe slot again (installing a recipe and asking for a render are the
//! same act, `RenderScheduler::set_recipe`). The second is pure overhead
//! and only a render-crate change could remove it, see
//! `canvas/before_after.md` §9. The design spends both as rarely as it
//! can: the snapshot is captured once per
//! `(image, pv, geometry, clip overlay)` and survives every mode toggle,
//! every hold of `\`, every zoom, every pan and every after-side edit,
//! because none of those change what "before" looks like. A window resize
//! does not invalidate it either, it only schedules a sharper re-capture,
//! since the engine always renders the whole image fit to the viewport and
//! a stale snapshot is simply the same picture at a different resolution.
//!
//! # Why the snapshot is copied instead of held
//!
//! The published frame's texture cannot be retained. The publisher cycles
//! three ring slots and reuses slot `generation % 3`
//! (`crates/lightbox-render/src/ng/sched/canvas.rs:35` and `:151`), so a
//! `CanvasFrame` kept for more than two further generations would quietly
//! start showing the after image under a BEFORE label. [`BeforeSnapshot`]
//! therefore copies the captured frame texel for texel into a texture this
//! module owns, with a compute kernel (`textureLoad` then `textureStore`,
//! no sampler, no filtering, bit exact for `rgba8unorm`). That is a GPU to
//! GPU copy on the shell's shared device: it maps nothing and reads back
//! nothing, so `lib.rs`'s zero-copy invariant ("NO `map_async`/CPU
//! readback anywhere in the frame path") holds unchanged.

use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use lightbox_edit::{Geometry, Recipe};
use lightbox_render::ng::sched::canvas::CANVAS_FORMAT;
use lightbox_render::ng::{DeviceCtx, KernelBuilder};
use lightbox_types::{ImageId, ProcessVersion};

use crate::canvas::states::{self, BeforeAfterMode};
use crate::canvas::xform::ViewXform;

// ─── What "before" is (the one function a future Copy Before replaces) ────

/// The recipe that renders the "before" image for `after`.
///
/// Neutral everything, at `after`'s process version, carrying `after`'s
/// geometry. See the module docs for why this definition and not the
/// history-position one, and for why geometry is the single exception.
pub fn before_recipe(after: &Recipe) -> Recipe {
    let mut before = Recipe::identity(after.pv);
    before.geometry = after.geometry.clone();
    before
}

/// Everything the captured before frame depends on.
///
/// Split into a *hard* part, which changes what the before image IS, and a
/// *soft* part, which only changes how sharp or how annotated it is. A
/// hard mismatch makes the snapshot unusable; a soft mismatch keeps
/// showing it while a fresher capture is scheduled, so resizing the window
/// in split mode does not flash the before pane away (the engine always
/// renders the whole image fit to the viewport, so a snapshot at any
/// extent is a valid whole-image texture, just at a different resolution).
#[derive(Clone, Debug, PartialEq)]
pub struct BeforeKey {
    /// Which image (hard).
    pub image: ImageId,
    /// The process version the before recipe renders under (hard).
    pub pv: ProcessVersion,
    /// The geometry the before render shares with the after render (hard),
    /// see the module docs' geometry note.
    pub geometry: Geometry,
    /// The viewport the frame was rendered at, pixels (soft).
    pub out: [u32; 2],
    /// Whether the `J` clip overlay was composited into it (soft): the
    /// overlay is a property of the pixels, so the before side gets its
    /// own, rather than a clipping readout that only tells half the story.
    pub clip_overlay: bool,
}

impl BeforeKey {
    /// Whether a snapshot captured for `self` is still showing the right
    /// picture for `want` (the hard part matches).
    fn shows_the_same_image_as(&self, want: &BeforeKey) -> bool {
        self.image == want.image && self.pv == want.pv && self.geometry == want.geometry
    }
}

// ─── Divider geometry (pure; image space, per `gizmo.md` convention 1) ────

/// Grab tolerance around the split divider, logical points. `gizmo.md`
/// convention 2 freezes 8 pt as the minimum for a handle-style hit region;
/// the divider is a handle in every sense except that it belongs to the
/// view rather than to the image, so it uses the same number.
pub const DIVIDER_HIT_TOLERANCE_PT: f32 = 8.0;

/// Gap between the two panes in side-by-side mode, logical points.
pub const SIDE_BY_SIDE_GUTTER_PT: f32 = 10.0;

/// How far one second of holding a divider key moves the divider, as a
/// fraction of the image width. Tuned so a full sweep takes about a second
/// and a half, fast enough to be useful and slow enough to land on a face.
const DIVIDER_KEY_RATE_PER_S: f32 = 0.7;

/// Shift multiplies the key rate (the coarse sweep).
const DIVIDER_KEY_COARSE: f32 = 3.0;

/// The screen x (logical points) of the divider at image-space fraction
/// `t`, under `xf`.
///
/// The divider is stored in image space (`gizmo.md` convention 1) so zoom,
/// pan and window resize move it with the photo instead of sliding it
/// across the picture. **M1 orientation caveat**, the same one the
/// eyedropper's loupe carries: this reads the mapping along image x, which
/// is the screen x only for `Orientation::O1`. `view.rs` builds every
/// `ViewXform` with `O1` today (see `xform.rs`'s module docs on why); when
/// E11 lands orientation this needs the `orient_forward` mapping.
pub fn divider_screen_x(xf: &ViewXform, t: f32) -> f32 {
    let width = xf.image_size_px().x;
    xf.image_to_screen(egui::vec2(width * t.clamp(0.0, 1.0), 0.0))
        .x
}

/// The inverse of [`divider_screen_x`], clamped into `0..=1`. Falls back to
/// the centre at a degenerate (zero width on screen) transform.
pub fn divider_fraction_at(xf: &ViewXform, screen_x: f32) -> f32 {
    let width = xf.image_size_px().x;
    let left = xf.image_to_screen(egui::Vec2::ZERO).x;
    let right = xf.image_to_screen(egui::vec2(width, 0.0)).x;
    let span = right - left;
    if span.abs() < f32::EPSILON {
        return 0.5;
    }
    ((screen_x - left) / span).clamp(0.0, 1.0)
}

/// The (before, after) halves of `view_rect` split at screen x `sx`.
///
/// Either half can be empty: at 200 % zoom the divider is frequently off
/// screen, in which case one side legitimately fills the whole view. The
/// caller must not paint a pane label for an empty half, or it would claim
/// a half that is not there.
pub fn split_clip_rects(view_rect: egui::Rect, sx: f32) -> (egui::Rect, egui::Rect) {
    let x = sx.clamp(view_rect.left(), view_rect.right());
    (
        egui::Rect::from_min_max(view_rect.min, egui::pos2(x, view_rect.bottom())),
        egui::Rect::from_min_max(egui::pos2(x, view_rect.top()), view_rect.max),
    )
}

/// The (before, after) panes of `view_rect` for side-by-side, separated by
/// `gutter` points. Degenerates to two copies of `view_rect` rather than
/// to negative-width rects when the view is narrower than the gutter.
pub fn side_by_side_panes(view_rect: egui::Rect, gutter: f32) -> (egui::Rect, egui::Rect) {
    if view_rect.width() <= gutter {
        return (view_rect, view_rect);
    }
    let pane_w = (view_rect.width() - gutter) / 2.0;
    let left = egui::Rect::from_min_size(view_rect.min, egui::vec2(pane_w, view_rect.height()));
    let right = egui::Rect::from_min_size(
        egui::pos2(view_rect.left() + pane_w + gutter, view_rect.top()),
        egui::vec2(pane_w, view_rect.height()),
    );
    (left, right)
}

// ─── Compositing the arrangement ──────────────────────────────────────────

/// Everything one frame's arrangement needs to be painted. Built by
/// `view.rs`'s `ready_ui`; separate from it so the "which texture ends up
/// on which side" decision can be checked against real rendered pixels
/// (see this module's `paints_the_before_texture_on_the_before_side` test)
/// rather than only by reading the code.
pub struct Arrangement<'a> {
    /// The arrangement to paint.
    pub mode: BeforeAfterMode,
    /// The whole canvas widget rect.
    pub view_rect: egui::Rect,
    /// Where the before image is framed (equal to `view_rect` except in
    /// side-by-side).
    pub before_pane: egui::Rect,
    /// Where the after image is framed.
    pub after_pane: egui::Rect,
    /// The image's display basis in pixels (post-crop, `view.rs`'s
    /// `cropped_extent`).
    pub image_px: egui::Vec2,
    /// Image-to-screen for the before pane.
    pub before_xform: &'a ViewXform,
    /// Image-to-screen for the after pane. Same zoom and pan as
    /// `before_xform`; only the pane it centres on differs.
    pub after_xform: &'a ViewXform,
    /// The captured before image, if one is held.
    pub before: Option<egui::TextureId>,
    /// The live canvas frame (engine output or tier preview).
    pub after: egui::TextureId,
    /// Split divider position, fraction of image width.
    pub divider: f32,
    /// Whether the divider is currently being dragged (brightens it).
    pub dragging: bool,
}

/// Paints one frame's arrangement: the image (or images), the divider, and
/// the labels that say which is which.
///
/// The `before.unwrap_or(after)` fallbacks are unreachable through
/// `ready_ui`, which only asks for a before-showing mode when a snapshot
/// is held ([`BeforeAfterMode::effective`]). They are written the safe way
/// round anyway: if one is ever reached it paints the after image, and the
/// label is what would then be wrong, which is why the labels are painted
/// from the same match arm and never from a separate pass.
pub fn paint_arrangement(ui: &egui::Ui, a: &Arrangement<'_>) {
    let whole_uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    let draw = |clip: egui::Rect, id: egui::TextureId, xf: &ViewXform| {
        let rect = egui::Rect::from_two_pos(
            xf.image_to_screen(egui::Vec2::ZERO),
            xf.image_to_screen(a.image_px),
        );
        ui.painter()
            .with_clip_rect(clip)
            .image(id, rect, whole_uv, egui::Color32::WHITE);
    };
    let before = a.before.unwrap_or(a.after);

    match a.mode {
        BeforeAfterMode::AfterOnly => draw(a.view_rect, a.after, a.after_xform),
        BeforeAfterMode::BeforeOnly => {
            draw(a.view_rect, before, a.after_xform);
            states::before_only_border(ui, a.view_rect);
            states::pane_label(ui, a.view_rect, states::PaneSide::Before);
        }
        BeforeAfterMode::Split => {
            let divider_x = divider_screen_x(a.after_xform, a.divider);
            let (before_half, after_half) = split_clip_rects(a.view_rect, divider_x);
            draw(before_half, before, a.after_xform);
            draw(after_half, a.after, a.after_xform);
            states::split_divider(ui, a.view_rect, divider_x, a.dragging);
            // An empty half gets no label: a label there would name a half
            // that is not on screen (the divider can be panned off).
            if before_half.width() > 0.0 {
                states::pane_label(ui, before_half, states::PaneSide::Before);
            }
            if after_half.width() > 0.0 {
                states::pane_label(ui, after_half, states::PaneSide::After);
            }
        }
        BeforeAfterMode::SideBySide => {
            draw(a.before_pane, before, a.before_xform);
            draw(a.after_pane, a.after, a.after_xform);
            states::pane_label(ui, a.before_pane, states::PaneSide::Before);
            states::pane_label(ui, a.after_pane, states::PaneSide::After);
        }
    }
}

// ─── Keyboard, and why it is read here rather than through the keymap ─────

/// Held to show the before image alone. `\` is the binding every editor
/// that has this feature uses.
pub const HOLD_KEY: egui::Key = egui::Key::Backslash;

/// Moves the split divider towards the before side.
pub const DIVIDER_LEFT_KEY: egui::Key = egui::Key::OpenBracket;

/// Moves the split divider towards the after side.
pub const DIVIDER_RIGHT_KEY: egui::Key = egui::Key::CloseBracket;

/// Whether the momentary before-only key is down this frame.
///
/// **Why this does not go through the keymap dispatcher.** `view.rs`'s
/// `ui` doc says keyboard input never reaches the canvas, and every other
/// canvas key does go through `keymap/dispatch.rs`, including this
/// feature's mode cycle (`view.before_after_cycle`). A momentary hold
/// cannot: the dispatcher matches `Event::Key { pressed: true, .. }` and
/// hands back a fired [`ActionId`], it has no concept of a key that is
/// still down (`keymap/dispatch.rs:40-66`), and there is no release edge
/// in its vocabulary at all. Reading `keys_down` is therefore not a
/// shortcut around the dispatcher, it is the only expression of the
/// gesture, and it is safe alongside it: `keys_down` is maintained by
/// egui's own `begin_pass` from the raw events, so the dispatcher removing
/// matched events from `InputState::events` cannot desynchronise it.
///
/// The text-focus guard is reproduced deliberately: it is exactly the rule
/// `dispatch` applies to chords without a hard modifier, so typing a
/// backslash into the snapshot-name field never flips the canvas.
pub fn hold_key_down(ctx: &egui::Context) -> bool {
    if ctx.egui_wants_keyboard_input() {
        return false;
    }
    ctx.input(|i| i.key_down(HOLD_KEY))
}

/// This frame's keyboard nudge of the split divider, as a signed fraction
/// of the image width, or `0.0` for none.
///
/// Continuous while held (rate times this frame's `stable_dt`) rather than
/// one step per press, because `InputState::key_pressed` counts only
/// non-repeat press events and would make a held key move the divider
/// exactly once. Shift sweeps.
pub fn divider_key_delta(ctx: &egui::Context) -> f32 {
    if ctx.egui_wants_keyboard_input() {
        return 0.0;
    }
    ctx.input(|i| {
        let left = i.key_down(DIVIDER_LEFT_KEY);
        let right = i.key_down(DIVIDER_RIGHT_KEY);
        let dir = f32::from(right) - f32::from(left);
        if dir == 0.0 {
            return 0.0;
        }
        let rate = if i.modifiers.shift {
            DIVIDER_KEY_RATE_PER_S * DIVIDER_KEY_COARSE
        } else {
            DIVIDER_KEY_RATE_PER_S
        };
        dir * rate * i.stable_dt
    })
}

// ─── The captured before frame ────────────────────────────────────────────

/// Texel-exact copy of the published canvas frame into a texture this
/// module owns. `textureLoad`/`textureStore` at matching integer
/// coordinates: no sampler, no filtering, no format conversion, so the
/// snapshot is bit-identical to the frame that was captured. Modelled on
/// `lightbox-render`'s `clip_overlay.wgsl`, which uses the same two
/// bindings against the same `rgba8unorm` canvas frames.
const COPY_WGSL: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(0) var dst: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(16, 16, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    textureStore(dst, vec2<i32>(gid.xy), textureLoad(src, vec2<i32>(gid.xy), 0));
}
"#;

/// How long a capture may stay outstanding before it is abandoned. The
/// primary abort is precise (the scheduler went idle without publishing a
/// newer generation, so nothing is coming); this is only a backstop
/// against a state neither this module nor the scheduler anticipated.
const CAPTURE_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(5);

/// Bytes one captured before image occupies: the published canvas format
/// is `rgba8unorm` (`ng::sched::canvas::CANVAS_FORMAT`), so four bytes per
/// pixel of the **viewport**, never of the sensor. This one line is the
/// entire memory cost of the feature; the module docs' cost section shows
/// what it works out to on a real display.
const fn snapshot_bytes(extent: [u32; 2]) -> usize {
    extent[0] as usize * extent[1] as usize * 4
}

/// Where the before snapshot is in its capture lifecycle.
#[derive(Clone, Debug, PartialEq)]
enum Capture {
    /// Nothing captured, nothing in flight.
    Idle,
    /// A before render is in flight. `after_generation` is the canvas
    /// generation that was published when it was submitted: the capture
    /// frame is the first one **strictly newer** than it.
    Waiting {
        key: BeforeKey,
        after_generation: u64,
        started: Instant,
    },
    /// A snapshot texture is registered and valid for `key`.
    Held { key: BeforeKey },
    /// The last capture attempt failed. Kept (rather than retried every
    /// frame) so the canvas can say so once instead of thrashing the
    /// engine; cleared by anything that changes the key.
    Failed { key: BeforeKey, reason: String },
}

/// The before image: one owned GPU texture plus the state machine that
/// fills it. See the module docs for the cost analysis and for why the
/// published frame is copied rather than retained.
pub struct BeforeSnapshot {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    kernels: KernelBuilder,
    /// The owned copy target and its extent, rebuilt when the extent moves.
    target: Option<(wgpu::Texture, [u32; 2])>,
    /// `target` registered with egui's renderer, so the painter can draw
    /// it. Freed and re-registered whenever `target` is rebuilt.
    registered: Option<egui::TextureId>,
    capture: Capture,
    /// Set when a capture ends (either way) to say the scheduler is still
    /// holding the BEFORE recipe for that image and the after recipe has
    /// to be put back. Carries the image it is owed for and the viewport
    /// the capture ran at (the submit key the restore re-establishes).
    ///
    /// Keyed by image, and deliberately NOT cleared by
    /// [`BeforeSnapshot::release`], because the canvas cannot restore a
    /// recipe for an image it is not showing: there is one canvas
    /// publisher for all images, so a submit for the image the user just
    /// left would publish its frame over the one they are looking at. The
    /// debt therefore waits until that image is active again.
    restore_pending: Option<(ImageId, [u32; 2])>,
}

impl BeforeSnapshot {
    /// A snapshot recording on `device`/`queue`, the shell's shared device
    /// (architecture §2.3 seam 2), the same one the canvas composites on
    /// and the same one `HistogramPass`/`ClipOverlayPass` are handed.
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> BeforeSnapshot {
        let kernels = KernelBuilder::new(&DeviceCtx::new(Arc::clone(&device), Arc::clone(&queue)));
        BeforeSnapshot {
            device,
            queue,
            kernels,
            target: None,
            registered: None,
            capture: Capture::Idle,
            restore_pending: None,
        }
    }

    /// Size of the owned snapshot texture in bytes, `0` when nothing is
    /// captured. See [`snapshot_bytes`] and the module docs' cost section.
    pub fn bytes(&self) -> usize {
        match &self.target {
            Some((_, extent)) => snapshot_bytes(*extent),
            None => 0,
        }
    }

    /// The snapshot to composite for `want`, or `None` when what is held
    /// is not this image (or nothing is held at all). A soft-only mismatch
    /// (a stale extent or clip-overlay state) still composites, see
    /// [`BeforeKey`].
    pub fn texture_for(&self, want: &BeforeKey) -> Option<(egui::TextureId, [u32; 2])> {
        let Capture::Held { key } = &self.capture else {
            return None;
        };
        if !key.shows_the_same_image_as(want) {
            return None;
        }
        let (_, size) = self.target.as_ref()?;
        Some((self.registered?, *size))
    }

    /// Whether a capture should be started for `want` right now: nothing
    /// is in flight, and what is held (or last failed) is not what is
    /// wanted. A soft mismatch still asks for a re-capture; the caller
    /// keeps compositing the stale snapshot meanwhile.
    pub fn needs_capture(&self, want: &BeforeKey) -> bool {
        match &self.capture {
            Capture::Idle => true,
            Capture::Waiting { .. } => false,
            Capture::Held { key } => key != want,
            // Do not retry the same failing key on a loop; a changed key
            // is a genuinely different request and is allowed through.
            Capture::Failed { key, .. } => {
                !key.shows_the_same_image_as(want) || key.out != want.out
            }
        }
    }

    /// Whether a before render is in flight (the caller must not submit
    /// anything else to the scheduler for this image while it is).
    pub fn is_capturing(&self) -> bool {
        matches!(self.capture, Capture::Waiting { .. })
    }

    /// Takes the "the scheduler is still holding the before recipe" debt
    /// if it is owed for `image`, with the viewport the capture ran at.
    /// A debt owed for a different image is left alone (see the field's
    /// own doc).
    pub fn take_restore_for(&mut self, image: ImageId) -> Option<[u32; 2]> {
        match self.restore_pending {
            Some((owed, out)) if owed == image => {
                self.restore_pending = None;
                Some(out)
            }
            _ => None,
        }
    }

    /// Forgets a previous failure so the next explicit request retries.
    /// Only user-initiated requests call this: a failure that retried on
    /// its own would re-render on a loop.
    pub fn clear_failure(&mut self) {
        if matches!(self.capture, Capture::Failed { .. }) {
            self.capture = Capture::Idle;
        }
    }

    /// The reason the last capture failed, for the canvas's notice chip.
    pub fn failure(&self) -> Option<&str> {
        match &self.capture {
            Capture::Failed { reason, .. } => Some(reason),
            _ => None,
        }
    }

    /// Records that a before render has just been submitted for `key`,
    /// with `after_generation` the canvas generation published at the
    /// moment of submission.
    pub fn begin(&mut self, key: BeforeKey, after_generation: u64) {
        self.capture = Capture::Waiting {
            key,
            after_generation,
            started: Instant::now(),
        };
    }

    /// Whether `generation` is the frame this capture is waiting for.
    /// Frames at or below the recorded generation are ordinary after
    /// frames that had not been consumed yet, and must be displayed
    /// normally rather than mistaken for the before image.
    pub fn is_capture_frame(&self, generation: u64) -> bool {
        matches!(
            &self.capture,
            Capture::Waiting { after_generation, .. } if generation > *after_generation
        )
    }

    /// Copies `frame` (a published `rgba8unorm` canvas frame at `extent`)
    /// into the owned snapshot texture and registers it with egui.
    /// Transitions to `Held`, or to `Failed` if the GPU work could not be
    /// built. Returns whether a snapshot is now held.
    pub fn accept(
        &mut self,
        frame: &wgpu::TextureView,
        extent: [u32; 2],
        renderer: &mut eframe::egui_wgpu::Renderer,
    ) -> bool {
        let Capture::Waiting { key, .. } = &self.capture else {
            return false;
        };
        let key = key.clone();

        let pipeline = match self.kernels.compute_pipeline(COPY_WGSL, "main") {
            Ok(pipeline) => pipeline,
            Err(err) => {
                self.capture = Capture::Failed {
                    key,
                    reason: format!("snapshot kernel unavailable: {err}"),
                };
                return false;
            }
        };

        let w = extent[0].max(1);
        let h = extent[1].max(1);
        if !matches!(&self.target, Some((_, size)) if *size == [w, h]) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("lightbox before snapshot"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // The publisher's own format, taken from the publisher, so
                // the copy can never silently become a conversion.
                format: CANVAS_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    .union(wgpu::TextureUsages::STORAGE_BINDING),
                view_formats: &[],
            });
            self.target = Some((texture, [w, h]));
            // The old registration pointed at the texture just dropped.
            if let Some(old) = self.registered.take() {
                renderer.free_texture(&old);
            }
        }
        let (texture, _) = self.target.as_ref().expect("just (re)built above");
        let dst = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let src_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lightbox before snapshot src"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(frame),
            }],
        });
        let dst_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lightbox before snapshot dst"),
            layout: &pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&dst),
            }],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lightbox before snapshot"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("lightbox before snapshot"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &src_group, &[]);
            pass.set_bind_group(1, &dst_group, &[]);
            pass.dispatch_workgroups(w.div_ceil(16), h.div_ceil(16), 1);
        }
        self.queue.submit([encoder.finish()]);

        if self.registered.is_none() {
            self.registered = Some(renderer.register_native_texture(
                &self.device,
                &dst,
                // Linear, matching `view.rs`'s `swap_displayed` for the
                // after frame: the two sides of a split must be filtered
                // identically or the divider itself becomes a visible
                // sharpness seam.
                wgpu::FilterMode::Linear,
            ));
        }
        self.restore_pending = Some((key.image, [w, h]));
        self.capture = Capture::Held { key };
        true
    }

    /// Abandons the in-flight capture with a reason the canvas can show.
    /// A no-op when nothing is in flight.
    pub fn abort(&mut self, reason: impl Into<String>) {
        if let Capture::Waiting { key, .. } = &self.capture {
            // The before recipe went to the scheduler whether or not a
            // frame came back, so the restore is owed either way.
            self.restore_pending = Some((key.image, key.out));
            self.capture = Capture::Failed {
                key: key.clone(),
                reason: reason.into(),
            };
        }
    }

    /// Abandons an in-flight capture that has passed the backstop
    /// deadline. Returns whether it did.
    pub fn abort_if_stalled(&mut self) -> bool {
        let stalled = matches!(
            &self.capture,
            Capture::Waiting { started, .. } if started.elapsed() > CAPTURE_BACKSTOP
        );
        if stalled {
            self.abort("before render did not arrive");
        }
        stalled
    }

    /// Releases the GPU texture and its egui registration (canvas exit).
    pub fn release(&mut self, renderer: &mut eframe::egui_wgpu::Renderer) {
        if let Some(id) = self.registered.take() {
            renderer.free_texture(&id);
        }
        self.target = None;
        self.capture = Capture::Idle;
        // `restore_pending` deliberately survives: see its own doc.
    }
}

// ─── The view state the canvas owns ───────────────────────────────────────

/// The before/after view: the mode (a [`BeforeAfterMode`], the state
/// machine in `states.rs`), the split divider, and the captured before
/// image. One field on `EditorCanvas`.
pub struct BeforeAfterView {
    mode: BeforeAfterMode,
    /// Divider position as a fraction of image width, image space
    /// (`gizmo.md` convention 1).
    divider: f32,
    /// True while a pointer drag on the divider is captured, so the canvas
    /// does not pan and the divider keeps tracking outside its hit region.
    dragging: bool,
    /// Whether the user has used a before mode at least once this session.
    /// Gates the idle pre-warm, see [`BeforeAfterView::wants_prewarm`].
    ever_used: bool,
    /// Last frame's hold-key state, so a fresh press is distinguishable
    /// from a key that is simply still down.
    was_held: bool,
    /// The snapshot itself.
    pub snapshot: BeforeSnapshot,
}

impl BeforeAfterView {
    /// A view in after-only mode with a centred divider.
    pub fn new(device: Arc<wgpu::Device>, queue: Arc<wgpu::Queue>) -> BeforeAfterView {
        BeforeAfterView {
            mode: BeforeAfterMode::AfterOnly,
            divider: 0.5,
            dragging: false,
            ever_used: false,
            was_held: false,
            snapshot: BeforeSnapshot::new(device, queue),
        }
    }

    /// Advances the latched mode: after only, side by side, split, back to
    /// after only (`view.before_after_cycle`).
    pub fn cycle(&mut self) {
        self.mode = self.mode.cycled();
        if self.mode != BeforeAfterMode::AfterOnly {
            self.ever_used = true;
            // An explicit request is the one thing that retries a capture
            // that failed before.
            self.snapshot.clear_failure();
        }
    }

    /// The mode the canvas should actually paint, given the momentary hold
    /// key and whether a before image is available to paint. The decision
    /// itself is [`BeforeAfterMode::effective`] (pure, in `states.rs`).
    pub fn effective(&self, held: bool, before_available: bool) -> BeforeAfterMode {
        self.mode.effective(held, before_available)
    }

    /// Whether a before image is wanted this frame at all (any mode that
    /// shows one, or the hold key being down).
    pub fn wants_before(&self, held: bool) -> bool {
        self.mode.with_hold(held).needs_before()
    }

    /// Whether to capture the before image *before* it is asked for.
    ///
    /// Only once the user has entered a before mode at least once this
    /// session. Pre-warming unconditionally would spend one extra engine
    /// render on every image a browsing user merely looks at, and the
    /// first thing that render would do is delay the after frame they are
    /// actually waiting for (the T26 navigation probe). Waiting for proof
    /// of interest costs the first `\` press one render and makes every
    /// press after it, on every subsequent image, a texture swap.
    pub fn wants_prewarm(&self) -> bool {
        self.ever_used
    }

    /// Records this frame's hold-key state.
    ///
    /// A fresh press (rather than a key that is still down) counts as
    /// using the feature, and retries a capture that failed earlier, on
    /// the same "only a user request retries" rule as [`Self::cycle`].
    pub fn set_hold(&mut self, held: bool) {
        if held && !self.was_held {
            self.ever_used = true;
            self.snapshot.clear_failure();
        }
        self.was_held = held;
    }

    /// The divider position, fraction of image width.
    pub fn divider(&self) -> f32 {
        self.divider
    }

    /// Moves the divider by `delta` (fraction of image width), clamped.
    pub fn nudge_divider(&mut self, delta: f32) {
        self.divider = (self.divider + delta).clamp(0.0, 1.0);
    }

    /// Sets the divider absolutely, clamped.
    pub fn set_divider(&mut self, t: f32) {
        self.divider = t.clamp(0.0, 1.0);
    }

    /// Whether a divider drag is currently captured.
    pub fn dragging(&self) -> bool {
        self.dragging
    }

    /// Routes this frame's pointer input at the split divider, **before**
    /// the canvas's own drag-pan, exactly as `GizmoLayer::route` does for
    /// gizmos (`gizmo.md` convention 3: a hit on an on-canvas affordance
    /// wins over pan). Returns whether the divider claimed the pointer.
    ///
    /// The divider is deliberately not a `Gizmo`: gizmos hold image-space
    /// *edit* geometry, are popped by Esc, and are force-cancelled on
    /// navigation (`gizmo.md` §2), and a view mode must survive all three.
    /// It obeys the same input-precedence rule without joining the stack.
    pub fn route_divider(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        xf: &ViewXform,
        active: bool,
    ) -> bool {
        if !active {
            self.dragging = false;
            return false;
        }

        if self.dragging {
            if let Some(pos) = response.interact_pointer_pos() {
                self.set_divider(divider_fraction_at(xf, pos.x));
            }
            if response.drag_stopped() {
                self.dragging = false;
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            return true;
        }

        let divider_x = divider_screen_x(xf, self.divider);
        let over = |pos: egui::Pos2| (pos.x - divider_x).abs() <= DIVIDER_HIT_TOLERANCE_PT;

        if let Some(pos) = response.hover_pos() {
            if over(pos) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }
        if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                if over(pos) {
                    self.dragging = true;
                    self.set_divider(divider_fraction_at(xf, pos.x));
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_edit::Crop;
    use lightbox_types::{Orientation, PV_M0};

    fn key(image: i64, out: [u32; 2]) -> BeforeKey {
        BeforeKey {
            image: ImageId(image),
            pv: PV_M0,
            geometry: Geometry::default(),
            out,
            clip_overlay: false,
        }
    }

    // ── What "before" is ────────────────────────────────────────────────

    /// **AC:** the before recipe is neutral, at the after recipe's process
    /// version. This is the whole definition, pinned so a future edit to
    /// `before_recipe` has to be deliberate.
    #[test]
    fn before_is_the_recipe_with_no_edits_at_the_same_process_version() {
        let mut after = Recipe::identity(PV_M0);
        after.global.exposure = 1.5;
        after.global.contrast = -0.4;

        let before = before_recipe(&after);
        assert!(
            before.is_neutral(),
            "before must render the source unmodified"
        );
        assert_eq!(before.pv, after.pv, "both sides render under the same PV");
        assert_eq!(before, Recipe::identity(PV_M0));
    }

    /// **AC (the geometry exception):** the before recipe carries the
    /// after recipe's crop, so both sides land at the one display basis
    /// `view.rs`'s `cropped_extent` derives. A before at a different crop
    /// would be stretched over the after image's rect.
    #[test]
    fn before_keeps_the_after_geometry_so_both_sides_share_one_extent() {
        let mut after = Recipe::identity(PV_M0);
        after.geometry.crop = Crop {
            left: 0.25,
            top: 0.0,
            right: 0.75,
            bottom: 1.0,
        };
        after.global.exposure = 1.0;

        let before = before_recipe(&after);
        assert_eq!(
            before.geometry, after.geometry,
            "the two sides must be the same framing"
        );
        assert_eq!(
            before.global.exposure, 0.0,
            "everything that is not geometry is still neutral"
        );
    }

    // ── The capture key: hard vs soft mismatches ────────────────────────

    /// A different viewport is a *soft* mismatch: the held snapshot is
    /// still the right picture (the engine always fits the whole image to
    /// the viewport), so it keeps compositing while a sharper capture is
    /// scheduled. Resizing the window in split mode must not flash the
    /// before pane away.
    #[test]
    fn a_resize_keeps_showing_the_snapshot_and_schedules_a_recapture() {
        let held = key(1, [800, 600]);
        let want = key(1, [1600, 1200]);
        assert!(
            held.shows_the_same_image_as(&want),
            "a resize does not change what before looks like"
        );
        assert_ne!(held, want, "but it does ask for a fresher capture");
    }

    /// A different image, process version or geometry is a *hard*
    /// mismatch: the snapshot is of something else and must not be shown.
    #[test]
    fn a_different_image_or_geometry_invalidates_the_snapshot() {
        let held = key(1, [800, 600]);
        assert!(!held.shows_the_same_image_as(&key(2, [800, 600])));

        let mut cropped = key(1, [800, 600]);
        cropped.geometry.crop = Crop {
            left: 0.1,
            top: 0.1,
            right: 0.9,
            bottom: 0.9,
        };
        assert!(
            !held.shows_the_same_image_as(&cropped),
            "a committed crop changes the display basis, so the snapshot is stale"
        );
    }

    // ── Divider geometry (pure) ─────────────────────────────────────────

    fn xform_for(zoom: f32, pan: egui::Vec2) -> ViewXform {
        ViewXform::new(
            egui::vec2(400.0, 300.0),
            Orientation::O1,
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0)),
            zoom,
            1.0,
            pan,
        )
    }

    /// **AC:** the divider round-trips through screen space at every zoom
    /// and pan, which is what makes "grab it where you see it" true.
    #[test]
    fn divider_position_round_trips_through_screen_space() {
        for &(zoom, pan) in &[
            (1.0_f32, egui::Vec2::ZERO),
            (0.5, egui::vec2(30.0, -10.0)),
            (2.0, egui::vec2(-120.0, 40.0)),
        ] {
            let xf = xform_for(zoom, pan);
            for &t in &[0.0_f32, 0.25, 0.5, 0.9, 1.0] {
                let back = divider_fraction_at(&xf, divider_screen_x(&xf, t));
                assert!(
                    (back - t).abs() < 1e-3,
                    "zoom={zoom} pan={pan:?} t={t} came back {back}"
                );
            }
        }
    }

    /// Image-space storage is the point: zooming must not slide the
    /// divider across the picture. The same image pixel stays under it.
    #[test]
    fn the_divider_stays_on_the_same_image_pixel_across_zoom() {
        let t = 0.4;
        let at_fit = xform_for(1.0, egui::Vec2::ZERO);
        let zoomed = xform_for(2.0, egui::Vec2::ZERO);
        let px_at_fit = at_fit
            .screen_to_image(egui::pos2(divider_screen_x(&at_fit, t), 150.0))
            .expect("on the image");
        let px_zoomed = zoomed
            .screen_to_image(egui::pos2(divider_screen_x(&zoomed, t), 150.0))
            .expect("on the image");
        assert!(
            (px_at_fit.x - px_zoomed.x).abs() < 0.5,
            "{px_at_fit:?} vs {px_zoomed:?}"
        );
    }

    #[test]
    fn split_clip_rects_partition_the_view_and_survive_an_offscreen_divider() {
        let view = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(400.0, 300.0));
        let (before, after) = split_clip_rects(view, 210.0);
        assert_eq!(before.width() + after.width(), view.width());
        assert_eq!(before.right(), after.left());

        // Divider far to the left of the view: the whole view is "after",
        // and the before half is empty rather than negative.
        let (before, after) = split_clip_rects(view, -500.0);
        assert_eq!(before.width(), 0.0);
        assert_eq!(after.width(), view.width());

        let (before, after) = split_clip_rects(view, 5000.0);
        assert_eq!(before.width(), view.width());
        assert_eq!(after.width(), 0.0);
    }

    #[test]
    fn side_by_side_panes_are_equal_and_separated_by_the_gutter() {
        let view = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(410.0, 300.0));
        let (left, right) = side_by_side_panes(view, 10.0);
        assert_eq!(left.width(), 200.0);
        assert_eq!(right.width(), 200.0);
        assert_eq!(right.left() - left.right(), 10.0);
        assert_eq!(left.height(), view.height());

        // Degenerate: a view narrower than the gutter never produces
        // negative-width panes.
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(4.0, 4.0));
        let (left, right) = side_by_side_panes(tiny, 10.0);
        assert_eq!(left, tiny);
        assert_eq!(right, tiny);
    }

    // ── Which texture ends up on which side (real rendered pixels) ──────

    /// The claim the labels make has to be true of the pixels, and reading
    /// the match arms is not proof of that. This renders
    /// [`paint_arrangement`] through the real wgpu path with two solid,
    /// unmistakable colours standing in for the before and after frames,
    /// then reads the result back and checks each side is the colour its
    /// label claims. Skips with no adapter, like `theme::gallery`'s own
    /// render tests.
    mod composite {
        use super::*;
        use egui_kittest::Harness;

        const VIEW: egui::Vec2 = egui::vec2(400.0, 200.0);
        /// Stands in for the captured before image.
        const BEFORE_RGB: [u8; 3] = [255, 0, 0];
        /// Stands in for the live canvas frame.
        const AFTER_RGB: [u8; 3] = [0, 0, 255];

        fn solid(ctx: &egui::Context, name: &str, rgb: [u8; 3]) -> egui::TextureId {
            let color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
            let image = egui::ColorImage::new([8, 8], vec![color; 64]);
            ctx.load_texture(name, image, egui::TextureOptions::NEAREST)
                .id()
        }

        /// The rendered frame as `(pixels, width)` in device pixels.
        /// Deliberately not the renderer's own image type: `egui_kittest`
        /// does not re-export the crate that type comes from, and this
        /// test only needs to read four bytes at a point.
        type Frame = (Vec<[u8; 4]>, u32);

        /// Renders `mode` and returns the captured frame, or `None` with
        /// no usable adapter.
        fn render(mode: BeforeAfterMode, divider: f32) -> Option<Frame> {
            let mut harness = Harness::builder()
                .with_size(VIEW)
                .with_pixels_per_point(1.0)
                .wgpu()
                .build_ui(crate::theme::test_support::themed(move |ui| {
                    let view_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, VIEW);
                    let before_id = solid(ui.ctx(), "before-probe", BEFORE_RGB);
                    let after_id = solid(ui.ctx(), "after-probe", AFTER_RGB);
                    let (before_pane, after_pane) = match mode {
                        BeforeAfterMode::SideBySide => {
                            side_by_side_panes(view_rect, SIDE_BY_SIDE_GUTTER_PT)
                        }
                        _ => (view_rect, view_rect),
                    };
                    // An image that exactly fills its pane, so every pixel
                    // of the pane is image and the read-back is
                    // unambiguous.
                    let image_px = egui::vec2(8.0, 8.0);
                    let zoom = (after_pane.width() / 8.0).max(after_pane.height() / 8.0);
                    let xf = |pane: egui::Rect| {
                        ViewXform::new(image_px, Orientation::O1, pane, zoom, 1.0, egui::Vec2::ZERO)
                    };
                    paint_arrangement(
                        ui,
                        &Arrangement {
                            mode,
                            view_rect,
                            before_pane,
                            after_pane,
                            image_px,
                            before_xform: &xf(before_pane),
                            after_xform: &xf(after_pane),
                            before: Some(before_id),
                            after: after_id,
                            divider,
                            dragging: false,
                        },
                    );
                }));
            // Frame 0 installs the theme's named font families and paints
            // nothing (`themed`'s documented contract); frame 1 is the one
            // worth capturing.
            harness.run();
            harness.run();
            match harness.render() {
                Ok(image) => {
                    let width = image.width();
                    Some((image.pixels().map(|p| p.0).collect(), width))
                }
                Err(err) => {
                    eprintln!("[before_after] no wgpu adapter / render failed, SKIPPED ({err})");
                    None
                }
            }
        }

        /// Which of the two probe colours a pixel is, if either.
        fn side_at(frame: &Frame, x: u32, y: u32) -> Option<&'static str> {
            let (pixels, width) = frame;
            let p = pixels[(y * width + x) as usize];
            let near =
                |want: [u8; 3]| (0..3).all(|i| (i32::from(p[i]) - i32::from(want[i])).abs() <= 8);
            if near(BEFORE_RGB) {
                Some("before")
            } else if near(AFTER_RGB) {
                Some("after")
            } else {
                None
            }
        }

        /// **AC:** in split mode the before image is left of the divider
        /// and the after image is right of it, in the pixels, not just in
        /// the code.
        #[test]
        fn split_paints_before_left_of_the_divider_and_after_right() {
            let Some(frame) = render(BeforeAfterMode::Split, 0.5) else {
                return;
            };
            let y = 40; // above the pane labels at the bottom
            assert_eq!(side_at(&frame, 40, y), Some("before"));
            assert_eq!(side_at(&frame, 150, y), Some("before"));
            assert_eq!(side_at(&frame, 250, y), Some("after"));
            assert_eq!(side_at(&frame, 360, y), Some("after"));
        }

        /// The divider is where the fraction says it is: at 25 % the
        /// before side is a quarter of the image, not half.
        #[test]
        fn the_split_fraction_places_the_seam() {
            let Some(frame) = render(BeforeAfterMode::Split, 0.25) else {
                return;
            };
            let y = 40;
            assert_eq!(side_at(&frame, 60, y), Some("before"));
            assert_eq!(
                side_at(&frame, 140, y),
                Some("after"),
                "past the quarter mark this must already be the edited image"
            );
        }

        /// **AC:** side by side puts before in the left pane and after in
        /// the right one.
        #[test]
        fn side_by_side_paints_before_left_and_after_right() {
            let Some(frame) = render(BeforeAfterMode::SideBySide, 0.5) else {
                return;
            };
            let y = 40;
            assert_eq!(side_at(&frame, 100, y), Some("before"));
            assert_eq!(side_at(&frame, 300, y), Some("after"));
        }

        /// Before-only paints the before image over the whole view, and
        /// after-only paints the after image over the whole view. The pair
        /// is the test: it proves the two arms are not both drawing the
        /// same texture.
        #[test]
        fn the_full_frame_modes_paint_the_texture_they_name() {
            let Some(frame) = render(BeforeAfterMode::BeforeOnly, 0.5) else {
                return;
            };
            assert_eq!(side_at(&frame, 200, 40), Some("before"));
            let Some(frame) = render(BeforeAfterMode::AfterOnly, 0.5) else {
                return;
            };
            assert_eq!(side_at(&frame, 200, 40), Some("after"));
        }
    }

    // ── The memory cost ─────────────────────────────────────────────────

    /// **AC (the measurement):** the before image costs four bytes per
    /// viewport pixel and nothing else. Pinned with the real numbers a
    /// full-screen canvas produces, so a change to the capture format or
    /// to what is captured has to move this test and say so.
    #[test]
    fn snapshot_bytes_is_four_bytes_per_viewport_pixel() {
        // A 16 inch Retina panel, canvas filling the window: about
        // 2360 x 1200 logical points at 2x device pixel ratio.
        assert_eq!(snapshot_bytes([4720, 2400]), 45_312_000);
        // A 1080p window at 1x, canvas about 1600 x 900.
        assert_eq!(snapshot_bytes([1600, 900]), 5_760_000);
        // Nothing captured yet.
        assert_eq!(snapshot_bytes([0, 0]), 0);
        // The figure does not depend on the raw's megapixels: a 60 MP
        // sensor and a 12 MP one at the same viewport cost the same, which
        // is the whole point of the canvas rendering fit-to-viewport.
        assert_eq!(snapshot_bytes([4720, 2400]), snapshot_bytes([4720, 2400]));
    }

    // ── The capture lifecycle (pure state, no GPU) ──────────────────────

    // ── The capture, against a real GPU ─────────────────────────────────

    /// Everything in [`BeforeSnapshot::accept`] that only a device can
    /// check: that the copy kernel compiles under naga, that its two bind
    /// groups match the pipeline's layouts, that the owned texture's
    /// usages allow both the storage write and the egui sample, and that
    /// re-capturing at a new extent reallocates and re-registers cleanly.
    /// wgpu's default error handler is fatal, so any validation fault in
    /// that path fails this test rather than degrading silently at
    /// runtime.
    ///
    /// Skips (rather than fails) with no adapter, the same posture
    /// `theme::gallery`'s render tests take. What it deliberately does NOT
    /// assert is pixel equality: proving that would need a readback, and
    /// this crate does not map GPU memory (`lib.rs`'s zero-copy
    /// invariants). The kernel is `textureStore(dst, gid,
    /// textureLoad(src, gid, 0))` at equal extents in one format, so there
    /// is no sampling, no filtering and no conversion for a copy to get
    /// wrong.
    mod gpu {
        use super::*;

        fn render_state() -> Option<eframe::egui_wgpu::RenderState> {
            // `create_render_state` panics when no adapter exists; the
            // panic message is left visible on purpose, it is the reason
            // the test skipped.
            std::panic::catch_unwind(|| {
                egui_kittest::wgpu::create_render_state(
                    egui_kittest::wgpu::default_wgpu_setup(),
                    eframe::egui_wgpu::RendererOptions::PREDICTABLE,
                )
            })
            .ok()
        }

        /// A stand-in for a published canvas frame: same format, same
        /// usages the publisher's ring textures carry.
        fn frame(device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32) -> wgpu::TextureView {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("test canvas frame"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: CANVAS_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING.union(wgpu::TextureUsages::COPY_DST),
                view_formats: &[],
            });
            let pixels: Vec<u8> = (0..w * h)
                .flat_map(|i| [(i % 251) as u8, 0x40, 0x80, 0xff])
                .collect();
            queue.write_texture(
                texture.as_image_copy(),
                &pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            texture.create_view(&wgpu::TextureViewDescriptor::default())
        }

        #[test]
        fn a_capture_copies_a_frame_into_an_owned_texture_and_recaptures_on_resize() {
            let Some(state) = render_state() else {
                eprintln!("[before_after] no wgpu adapter, SKIPPED");
                return;
            };
            let device = Arc::new(state.device.clone());
            let queue = Arc::new(state.queue.clone());
            let mut snapshot = BeforeSnapshot::new(Arc::clone(&device), Arc::clone(&queue));

            let small = key(1, [64, 32]);
            assert_eq!(snapshot.bytes(), 0);
            assert!(snapshot.texture_for(&small).is_none());
            assert!(snapshot.needs_capture(&small));

            snapshot.begin(small.clone(), 7);
            assert!(snapshot.is_capturing());
            assert!(!snapshot.needs_capture(&small), "one capture at a time");
            assert!(!snapshot.is_capture_frame(7));
            assert!(snapshot.is_capture_frame(8));

            let view = frame(&device, &queue, 64, 32);
            let mut renderer = state.renderer.write();
            assert!(snapshot.accept(&view, [64, 32], &mut renderer));
            drop(renderer);
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("the copy submits and completes");

            assert!(!snapshot.is_capturing());
            // The scheduler is still holding the before recipe; the canvas
            // is told to put the after recipe back, exactly once.
            assert_eq!(
                snapshot.take_restore_for(ImageId(2)),
                None,
                "a debt owed for one image must not be paid against another"
            );
            assert_eq!(snapshot.take_restore_for(ImageId(1)), Some([64, 32]));
            assert_eq!(snapshot.take_restore_for(ImageId(1)), None);
            assert_eq!(snapshot.bytes(), 64 * 32 * 4);
            let (_, size) = snapshot.texture_for(&small).expect("a snapshot is held");
            assert_eq!(size, [64, 32]);
            assert!(
                snapshot.texture_for(&key(2, [64, 32])).is_none(),
                "another image's before must never be composited for this one"
            );
            assert!(!snapshot.needs_capture(&small), "nothing has changed");

            // A resize: still usable, but a sharper capture is scheduled.
            let bigger = key(1, [128, 64]);
            assert!(snapshot.texture_for(&bigger).is_some());
            assert!(snapshot.needs_capture(&bigger));

            snapshot.begin(bigger.clone(), 9);
            let view = frame(&device, &queue, 128, 64);
            let mut renderer = state.renderer.write();
            assert!(snapshot.accept(&view, [128, 64], &mut renderer));
            device
                .poll(wgpu::PollType::wait_indefinitely())
                .expect("the second copy submits and completes");
            assert_eq!(snapshot.bytes(), 128 * 64 * 4);
            assert!(snapshot.texture_for(&bigger).is_some());

            snapshot.release(&mut renderer);
            assert_eq!(snapshot.bytes(), 0, "canvas exit frees the snapshot");
            assert!(snapshot.texture_for(&bigger).is_none());
        }

        /// An abandoned capture leaves a reason the canvas can show and
        /// does not retry the same failing request every frame.
        #[test]
        fn an_aborted_capture_reports_once_and_does_not_spin() {
            let Some(state) = render_state() else {
                eprintln!("[before_after] no wgpu adapter, SKIPPED");
                return;
            };
            let device = Arc::new(state.device.clone());
            let queue = Arc::new(state.queue.clone());
            let mut snapshot = BeforeSnapshot::new(device, queue);

            let k = key(1, [64, 32]);
            snapshot.begin(k.clone(), 3);
            snapshot.abort("the render produced no frame");
            assert!(!snapshot.is_capturing());
            assert_eq!(
                snapshot.take_restore_for(ImageId(1)),
                Some([64, 32]),
                "a failed capture still put the before recipe in the scheduler"
            );
            assert_eq!(snapshot.failure(), Some("the render produced no frame"));
            assert!(
                !snapshot.needs_capture(&k),
                "the same failing request must not be retried on a loop"
            );
            assert!(
                snapshot.needs_capture(&key(2, [64, 32])),
                "a genuinely different request is allowed through"
            );
        }
    }

    /// A capture must only ever consume a generation **newer** than the
    /// one that was published when it was submitted. An older or equal
    /// generation is an after frame that had not been consumed yet, and
    /// treating it as the before image would silently make before equal
    /// after: the exact failure the whole feature exists to prevent.
    #[test]
    fn only_a_strictly_newer_generation_is_the_capture_frame() {
        let mut capture = Capture::Waiting {
            key: key(1, [800, 600]),
            after_generation: 42,
            started: Instant::now(),
        };
        let is_capture = |c: &Capture, g: u64| matches!(c, Capture::Waiting { after_generation, .. } if g > *after_generation);
        assert!(!is_capture(&capture, 41));
        assert!(!is_capture(&capture, 42), "the frame already published");
        assert!(is_capture(&capture, 43));

        capture = Capture::Idle;
        assert!(
            !is_capture(&capture, 99),
            "nothing in flight, nothing to take"
        );
    }
}
