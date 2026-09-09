<!--
SPDX-FileCopyrightText: 2026 PixeLabs
SPDX-License-Identifier: AGPL-3.0-or-later
Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
-->

# On-canvas gizmos, the author guide (E08 §6.6, frozen at Phase F)

This document is the contract E11 (crop/geometry) and E12 (mask pins) build
against. It freezes the interaction conventions of `canvas/gizmo.rs` and
walks the one shipped gizmo, the WB eyedropper, through every seam.
Read it together with the rustdoc on `Gizmo`, `GizmoLayer`, `GizmoEvent`,
and `GizmoEffect` in `gizmo.rs`; nothing below contradicts that rustdoc,
it only adds the *why* and the worked example.

Status note: Phase F shipped under the owner's defer-all-testing directive
the conventions here are frozen and the plumbing is wired end to end, but
the F1/F2 AC tests (and the test templates at the bottom) are deferred to
the final cross-epic pass. See `docs/plan/epics/E08-deviations.md` Phase F.

---

## 1. The five frozen conventions

1. **Geometry lives in image space.** Store every coordinate your gizmo
   owns (crop rect, pin position, hover point) in raw, unrotated image
   pixels, the space `ViewXform` (`canvas/xform.rs`) maps. Zoom, pan,
   window resize, and (post-E11) orientation changes then cost you
   nothing: only the transform changes, never your data. The eyedropper
   stores even its transient hover position this way.

2. **Hit-testing happens in screen space,** against the frame's
   `ViewXform`, the ONE authority compositing already uses (spec §6.4).
   - *Handle-style* gizmos (crop corners/edges, mask pins): a hit is
     "within `HIT_TOLERANCE_PT` (= **8 logical points**, minimum, round
     up for small touch targets, never down) of the handle's screen
     position." Map the handle image→screen with `xf.image_to_screen`,
     compare in points. The tolerance is *screen*-space by design: a
     handle must stay grabbable at 25 % zoom.
   - *Area-style* gizmos (the eyedropper): containment test, any screen
     point inside the composited image hits
     (`xf.screen_to_image(screen).is_some()`), letterbox falls through.
   - Return a `HitId` naming *which* handle (gizmo-local meaning; the
     layer threads it through the drag lifecycle unchanged). An
     area-style gizmo with one conceptual handle uses `HitId(0)`.

3. **A gizmo hit wins over pan/zoom.** You get this for free:
   `EditorCanvas::ready_ui` calls `GizmoLayer::route` *before* its own
   drag-pan / double-click-zoom handling and skips both whenever the
   layer claims the pointer. A drag that starts on your hit region is
   captured to you until release, the canvas never pans mid-capture,
   even when the pointer leaves your hit region (or the image). Wheel
   zoom is deliberately **not** claimed: zooming mid-tool is legitimate,
   and convention 1 makes your geometry immune to it.

4. **Esc = `Cancelled`, Enter = `Done`, as keymap actions.** Phase D's
   dispatcher consumes the chords before any widget sees them, so you
   never receive raw key events. `gizmo.cancel`/`gizmo.commit` (bound to
   Esc/Enter, registered against `CTX_GIZMO` in `keymap/mod.rs`) arrive
   as `GizmoEvent::Cancel`/`GizmoEvent::Commit`. Respond by pushing
   `GizmoEffect::Cancelled`/`GizmoEffect::Done` when you are fully
   unwound, the layer pops you the moment you emit either. A
   multi-stage gizmo may consume a `Cancel` per stage (clear selection
   first, exit second): simply don't emit `Cancelled` until you mean it.
   Caveat: on `Cancel`/`Commit` the `xf` argument may be stale or
   degenerate (the action can arrive before the canvas has routed a
   frame), never derive geometry from it on those two events.

5. **One drag = one `EditBinding` gesture.** A param-editing drag maps
   exactly onto E09's coalesced-history discipline via the effect
   bracket:

   | Your event      | You emit                              | `lib.rs` routes to                 |
   |-----------------|---------------------------------------|------------------------------------|
   | `DragStart`     | `EditBegin(ParamId)`                  | `EditBinding::begin_gesture(p)`    |
   | `Drag` (each)   | `Edit(ParamDelta)`                    | `EditBinding::preview(d)`, re-renders the **same frame** (C3 `recipe_rev` submit key) |
   | `DragEnd`       | `EditEnd`                             | `EditBinding::end_gesture()`, ONE durable history step |

   Emit the bracket balanced: exactly one `EditEnd` per `EditBegin`, and
   unwind (emit `EditEnd`, or never `EditBegin` in the first place)
   before `Cancelled`. Never touch `Session`/`Command::Edit` yourself
   the effects queue is your only exit (the same seam rule panels live
   under, §6.5).

## 2. The layer contract (what the plumbing does for you)

`GizmoLayer` is owned by `LightboxApp` (`lib.rs`) and threaded into
`EditorCanvas::ui` as `&mut` each frame. Lifecycle:

- **Activate:** `layer.activate(Box::new(YourGizmo::new(...)))`, pushes
  you as the innermost active gizmo. Same-id re-activation cancels the
  old instance first (never double-stacks). At M1 the only activation
  site is a panel tool button via `DevelopCtx.gizmos`
  (`panels/basic.rs::eyedropper_button` is the model).
- **Stack:** input routes innermost-first (top of stack claims first);
  paint runs bottom→top (innermost paints on top). With one active gizmo
, every M1 frame, this degenerates to "you".
- **Events you receive** (all positions image-space):
  `Hover { image_px: Option<_> }` every routed frame while no drag is
  captured (`None` = pointer off the image, clear your hover
  affordance); `DragStart`/`Drag`/`DragEnd` while captured (positions
  clamped into image bounds; `delta_px` derives from clamped position
  pairs, so it flattens at the edges, fine for pins/edges, know it's
  there); `Click` for a clean press+release on your hit region;
  `Cancel`/`Commit` from the keymap.
- **Pop:** emit `Done` or `Cancelled` and you're removed immediately.
  Navigation to another image (and canvas exit) force-cancels every
  active gizmo, your geometry belongs to the image you were armed on;
  don't expect to survive it.
- **Cursor:** implement `cursor()` (e.g. `CursorIcon::Crosshair`); the
  layer sets it while the pointer is over your hit region and for the
  whole drag capture.
- **Effects drain:** once per frame, `lib.rs` drains `take_effects()`
  and routes them (see the table above; `Picked` goes to the owning
  panel's callback). Effects emitted from a keymap action (Esc) drain
  the same frame.

**Canvas-not-Ready caveat:** routing and painting only happen while the
canvas shows a `Ready` entry (placard/loading frames have no `ViewXform`).
Your keymap context stays pushed the whole time you're active, so Esc
always works, but you receive no pointer events on such frames.

## 3. Keymap context push/pop rules

While the layer is non-empty, `lib.rs::keymap_stack` pushes (innermost
last):

```text
app → editor [→ editor.loupe] → editor.gizmo → editor.gizmo.<your-id>
```

- `editor.gizmo` (`keymap::CTX_GIZMO`) is what makes the shared
  `gizmo.cancel` (Esc) / `gizmo.commit` (Enter) resolve. You do nothing
  to get this.
- `editor.gizmo.<id>` is *your* `Gizmo::context()` value (the
  eyedropper's: `"editor.gizmo.wb_eyedropper"`). Register gizmo-specific
  actions against it (`registry.register(ActionDef { contexts:
  &[ContextId("editor.gizmo.geom_crop")], .. })` at app construction) and
  they shadow everything outer, innermost context wins
  (`keymap/registry.rs`). No M1 action targets a sub-context; the slot is
  reserved for you.
- Push happens the frame after activation (the stack is rebuilt at
  frame top); pop happens the frame after you emit `Done`/`Cancelled`.
  Both are one-frame latencies inherent to immediate mode; the dispatch
  order (keymap → panels → canvas → drain) makes them unobservable in
  practice.
- While your context is pushed, plain-key chords bound to outer contexts
  (Z, Space, arrows…) still resolve unless you shadow them, shadow
  deliberately, not by accident (run the cheat sheet, ⌘/, while your
  gizmo is active to audit).

## 4. Zero-copy magnification (the loupe pattern)

The shell never reads GPU pixels, no `map_async`, no readback, ever
(`lib.rs` top-of-file invariants). To magnify, re-draw the
already-registered composited texture with a small UV rect:

```text
GizmoPaintCtx.texture     = (TextureId, [w, h])  — engine frame or tier
                            preview, whichever composited this frame
uv_center                 = image_px / xf.image_size_px()
uv_half                   = (region_px / 2) / image_size   (per axis)
painter.image(texture_id, chip_rect, Rect{uv_center ± uv_half}, WHITE)
```

This is valid because the engine always renders the *whole* image fit to
the viewport, zoom/pan are display-side crops (`view.rs` module docs)
so normalized UVs over the full image are always correct. Sampler address
mode clamps at edges (graceful near-edge degradation). The eyedropper's
numbers, tuned by eye: 96 pt chip, 24 image px across (4×+ apparent
magnification), 18 pt cursor offset, flipped inside the canvas when it
would overflow. `texture` is `None` on placard frames, skip the chip,
keep your outline affordances.

**M1 orientation caveat:** UV mapping assumes `Orientation::O1`, true
until E11 lands orientation, at which point loupe UVs need the
`orient_forward` mapping (`xform.rs`); the eyedropper carries the same
note at its paint site.

## 5. Worked example, the WB eyedropper, end to end

The complete flow of the one shipped gizmo (`WbEyedropper`, ~90 lines):

1. **Arm.** The Basic panel's Eyedropper toggle
   (`panels/basic.rs::eyedropper_button`) calls
   `ctx.gizmos.activate(Box::new(WbEyedropper::new()))`. The button's
   pressed state *is* `ctx.gizmos.is_active(WB_EYEDROPPER)`, one
   authority, no mirrored flag to drift.
2. **Context.** Next frame, `keymap_stack` pushes `editor.gizmo` +
   `editor.gizmo.wb_eyedropper`. Esc/Enter now resolve to
   `gizmo.cancel`/`gizmo.commit`.
3. **Hover.** Each `Ready` frame the layer routes
   `Hover { image_px }`; the gizmo stores it (image space) and paints a
   crosshair + the loupe chip (§4) at `xf.image_to_screen(hover)`. The
   layer sets `CursorIcon::Crosshair` over the image.
4. **Hit.** `hit()` = area-style containment: `screen_to_image(screen)
   .map(|_| HitId(0))`. Letterbox clicks/drags fall through to pan.
5. **Drag.** A drag from inside the image is captured, the canvas never
   pans (F1's AC), and just moves the loupe (`Drag` updates the stored
   hover). Release picks nothing; only a clean click commits.
6. **Pick.** `Click { image_px }` → the gizmo emits
   `Picked { image_px }` **and** `Done` (one pick per arming, the
   Lightroom convention). The layer pops it immediately.
7. **Resolve.** `lib.rs`'s drain routes `Picked` to
   `panels::basic::apply_wb_pick(&mut binding, image_px, image_size)`
   the *panel* owns what a pick means. At M1 that is a **temporary
   position-linear estimate** (normalized x → ±3000 K around 6500 K,
   normalized y → ±50 tint) behind a `debug_assertions`-gated TODO
   spec §8 F2's named seam. E10 replaces only that function's estimate
   block with real neutral-point sampling; the gizmo, layer, routing,
   and button never change.
8. **Commit.** `apply_wb_pick` brackets one gesture
   (`begin_gesture(WhiteBalance)` → `preview(wb_delta(Custom{..}))` →
   `end_gesture()`): one history step, same-frame re-render via the C3
   recipe_rev key.
9. **Cancel path.** Esc at any point → `Cancel` → the gizmo emits
   `Cancelled` → popped, nothing committed, button un-presses (it reads
   `is_active`). Clicking the button off, or navigating to another
   image, takes the same path (`cancel`/`cancel_all`).

## 6. Adding a gizmo (E11/E12 checklist)

1. Define your type in your module (it does NOT have to live in
   `canvas/gizmo.rs`); implement `Gizmo`. Give it a stable
   `GizmoId("geom.crop")` and `ContextId("editor.gizmo.geom_crop")`
   (dotted id with `.` → `_`, matching the eyedropper's spelling).
2. Store geometry in image space (convention 1). Seed it from the
   active recipe via `EditBinding` values at activation time (pass what
   you need into your constructor, the gizmo is a plain struct).
3. Hit-test per convention 2 (8 pt minimum for handles).
4. Drive edits ONLY through the `EditBegin`/`Edit`/`EditEnd` bracket
   (convention 5). `Picked` is for point-picks routed to a panel
   callback; add your routing arm to the `lib.rs` drain if you add a new
   pick consumer.
5. Mount an activation affordance: a panel button via
   `DevelopCtx.gizmos` (copy `eyedropper_button`), or a keymap action
   whose handler calls `self.gizmos.activate(..)`.
6. Register gizmo-specific chords against your sub-context if you need
   them (§3); Esc/Enter come free.
7. Emit `Done`/`Cancelled` deliberately; expect force-cancel on
   navigation.

## 7. Interaction-test templates (DEFERRED, final testing pass)

Owner directive: no test code may be written during E08's implementation
phases. These templates encode the F1/F2 ACs for whoever writes the final
pass (they mirror `keymap/dispatch.rs`'s existing kittest patterns; the
probe-gizmo one is the F1 AC verbatim). They are *sketches to adapt*
not compiled, not doctests (this file is not wired into rustdoc
recorded in the Phase F deviations).

```rust,ignore
/// F1 AC: a drag inside a gizmo's hit region never pans; Esc pops the
/// context and emits Cancelled.
#[test]
fn probe_gizmo_drag_never_pans_and_esc_cancels() {
    // ProbeGizmo: hit() = Some(HitId(0)) inside a fixed image rect;
    // on_event records every event; Cancel => emits Cancelled.
    // Harness: EditorCanvas::ui with CanvasContent::Ready + a GizmoLayer
    //   holding the probe (see canvas/view.rs's headless_engine module
    //   for the no-GPU scheduler this needs).
    // 1. drag from inside the hit region → assert canvas pan unchanged
    //    AND probe saw DragStart/Drag/DragEnd.
    // 2. dispatch gizmo.cancel (simulate Esc via keymap::dispatch with
    //    CTX_GIZMO on the stack) → assert take_effects() contains
    //    Cancelled and active_context() is None.
}

/// F2 AC: a click at a known screen point yields the expected image px
/// within 0.5 px across zoom/pan/orientation.
#[test]
fn eyedropper_click_maps_to_image_px_within_half_a_pixel() {
    // Sweep ViewXform configs (reuse xform.rs's round-trip test's
    // zoom/pan/orientation grid). For each: activate WbEyedropper,
    // synthesize a click at xf.image_to_screen(known_px), assert the
    // drained Picked{image_px} is within 0.5 px of known_px.
}
```
