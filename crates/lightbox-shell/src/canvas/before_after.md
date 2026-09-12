<!--
SPDX-FileCopyrightText: 2026 PixeLabs
SPDX-License-Identifier: AGPL-3.0-or-later
Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
-->

# Before and after, the interaction model

Companion to `canvas/gizmo.md`. That document freezes the conventions for
on-canvas *edit* affordances; this one covers the before/after view, which
is an on-canvas *view* affordance and deliberately not a gizmo. Read it
together with the rustdoc on `canvas/before_after.rs` (which owns the
definition of "before" and the memory analysis) and
`states::BeforeAfterMode` (which owns the state machine).

---

## 1. The four states, and how you reach them

| State | How | What you see |
|---|---|---|
| After only | the default; `Y` from split | the edited image, no extra chrome |
| Before only | **hold `\`** | the unedited image, full frame, amber border + `BEFORE` |
| Before and after, side by side | `Y` from after only | two panes, before left, after right |
| Before and after, split | `Y` from side by side | one image, one draggable divider |

`Y` (`view.before_after_cycle`, registered in `keymap/mod.rs`, rebindable
like every other action) cycles **after only → side by side → split → after
only**.

Before-only is not on that cycle. It is the one arrangement that can be
mistaken for the edited photo, because there is nothing beside it to
compare against, so it exists only while the key is physically down. Let go
and you are back exactly where you were.

## 2. Why the hold key is read by the canvas and not by the dispatcher

Every other canvas key goes through `keymap/dispatch.rs`, and this feature's
cycle does too. The hold cannot: the dispatcher matches
`Event::Key { pressed: true, .. }` and returns fired `ActionId`s, it has no
representation of a key that is *still* down and no release edge at all
(`keymap/dispatch.rs:40-66`). `before_after::hold_key_down` therefore reads
`InputState::key_down` directly.

Two things make that safe rather than a hole in the model:

- `keys_down` is maintained by egui's own `begin_pass` from the raw event
  stream, so the dispatcher removing matched events from
  `InputState::events` cannot desynchronise it. The two mechanisms coexist
  without either seeing the other.
- The text-focus guard is reproduced verbatim
  (`Context::egui_wants_keyboard_input`), which is the same rule `dispatch`
  applies to chords without a hard modifier. Typing a backslash into the
  snapshot-name field never flips the canvas.

The same applies to `[` and `]`, the split divider's keyboard controls:
they move the divider continuously while held (rate times `stable_dt`,
tripled with Shift), which `InputState::key_pressed` cannot express because
it counts only non-repeat press events.

Arrow keys were the obvious choice for the divider and are deliberately not
used: `nav.prev`/`nav.next` own them in `CTX_LOUPE`, so an arrow would both
move the divider and change the photo.

## 3. Input precedence

`EditorCanvas::ready_ui` routes pointer input in this order, and each stage
can claim the pointer from the ones below it:

```text
gizmo stack  →  split divider  →  canvas pan / double-click zoom
```

Gizmos keep the precedence `gizmo.md` convention 3 gives them: an edit
affordance outranks a view affordance. The divider uses the same 8 pt hit
tolerance (`gizmo.md` convention 2) and the same capture discipline: a drag
that starts on it is captured until release, and keeps tracking when the
pointer leaves the hit region.

## 4. Why the divider is not a `Gizmo`

It would fit the trait, and it obeys two of the five conventions already
(geometry in image space, hit-testing in screen space). It stays out of the
stack because the other three are wrong for it:

- **Esc must not cancel it.** `gizmo.md` convention 4 makes Esc mean
  "unwind and pop". Esc in a split view should do nothing at all; the split
  is not a pending action with a commit.
- **Navigation must not cancel it.** The layer force-cancels every gizmo on
  image change (`gizmo.md` §2, "your geometry belongs to the image you were
  armed on"). A view mode is the opposite: it belongs to the person, not to
  the photo, and browsing a set in split view is the whole point.
- **It drives no `EditBinding` gesture.** Convention 5's
  `EditBegin`/`Edit`/`EditEnd` bracket exists so a drag becomes one history
  step. Moving the divider changes nothing about the image and must write
  no history.

So the divider is routed by the canvas, immediately after the gizmo layer,
under the same precedence rule.

## 5. The divider lives in image space

Stored as a fraction of image width (`gizmo.md` convention 1), so zoom, pan
and window resize move it with the photograph instead of sliding it across
the picture. At high zoom it can end up outside the viewport; when that
happens the visible half is drawn in full, the divider itself is not drawn,
and only that half's label is painted, so the view still says what it is.
`[` and `]` keep working, which is how you bring it back without panning.

**M1 orientation caveat**, the same one the eyedropper's loupe carries: the
mapping from image-x to screen-x assumes `Orientation::O1`, which is what
`view.rs` builds every `ViewXform` with today (`xform.rs` module docs). When
E11 lands orientation, `divider_screen_x`/`divider_fraction_at` need the
`orient_forward` mapping.

## 6. Labelling, and the one rule that outranks everything

A photographer must never be unsure which image is in front of them.
Three independent statements of the same fact:

1. **Pane labels.** A `BEFORE` chip in `STATUS_WARNING` and an `AFTER` chip
   in the quiet placard treatment, at the bottom centre of each pane. The
   asymmetry is intentional: the eye should land on the half that is *not*
   the edit. A pane too small for its label paints nothing, rather than a
   label spilling across the divider and naming the wrong half.
2. **A border.** Before-only gets a 2 px `STATUS_WARNING` frame around the
   whole view, because that state has no second half to compare against.
3. **The info readout.** The canvas's top-left overlay names the
   arrangement, and says nothing at all in the ordinary after-only state.

The rule that outranks the rest: **if the before image is not available,
every before-showing mode degrades to the plain after canvas**
(`BeforeAfterMode::effective`). Showing the after image with no label is
recoverable. Showing the after image under a `BEFORE` label is the mistake
that makes somebody ship the wrong edit. A quiet chip says "preparing
before…", or names the failure if there was one.

Export is unaffected by any of this by construction: `Command::Export`
resolves each image's recipe with `edit_hub.store().open_state(image)`
(`lightbox-core/src/session.rs:1411`), the persisted edit state, never what
the canvas happens to be compositing. There is no path from "forgot I was
in before mode" to "exported the wrong pixels".

## 7. The capture handshake

The engine has one recipe slot per image (`RenderScheduler::set_recipe`)
and one canvas publisher, so before and after cannot be in flight at once.
The before image is therefore *captured* into a texture the shell owns, and
that capture is a small, explicit handshake:

```text
want a before image, scheduler idle, nothing awaited
    → record the published generation G
    → set_recipe(image, before_recipe)          [ordinary submits suspended]
    → the first frame with generation > G is the capture
    → copy it into the owned texture (compute kernel, no readback)
    → put the after recipe back (set_recipe, never set_view)
```

Four details are load bearing:

- **Idle first.** A before submit issued while a render is in flight would
  land in the coalescer's pending slot, and the next published frame would
  be the *other* job's. `wait_idle(Duration::ZERO)` is a non-blocking read
  of that state, and `EditorCanvas` is the only submitter to the scheduler
  in the whole shell, so idle means the before job dispatches immediately.
- **Strictly newer.** A frame at or below `G` is an after frame that had
  not been consumed yet. Taking it as the before image would make before
  equal after: silent, and exactly the failure this feature exists to
  prevent.
- **The captured frame never becomes `displayed`.** The after frame already
  on screen stays on screen, so there is no flash to the unedited image
  mid-capture, the histogram keeps describing the edit, and the navigation
  probe does not count a render nobody asked for.
- **Restore with `set_recipe`, not `set_view`.** `set_view` re-renders
  whichever recipe the scheduler currently holds, which at that moment is
  the before one. Going back through the ordinary submit path would
  therefore dispatch one more render of the unedited image and put it on
  the after side for a frame.

If the render terminates without publishing (the scheduler goes idle and no
newer generation appeared), the capture is abandoned with a reason, the
after recipe is restored, and the canvas degrades to after-only with a
chip. A failed capture is not retried on its own; only an explicit request
(`Y`, or a fresh press of `\`) tries again.

## 8. When the capture happens, and when it does not

The snapshot is keyed on `(image, process version, geometry, clip overlay)`
plus, softly, the viewport. It survives every mode toggle, every hold of
`\`, every zoom, every pan and every edit to the after side, because none
of those change what "before" looks like. A window resize keeps showing the
snapshot and merely schedules a sharper one: the engine always renders the
whole image fit to the viewport, so a stale snapshot is the same picture at
a different resolution, and flashing the before pane away over a resize
would be worse than a slightly soft one.

The first capture for an image costs **two** engine renders at viewport
resolution, not one: the before render itself, and the render that comes
back when the after recipe is put into the scheduler's single recipe slot
again. The second one is pure overhead and the seam is what forces it, see
§9. To keep the first `\` instant rather than the second, the
canvas pre-warms on idle, but only once the session has used a before mode
at least once. Pre-warming unconditionally would spend a render on every
image a browsing user merely looks at, and would delay the after frame they
are actually waiting for.

## 9. What the render crate would need to make this cheaper

The whole handshake in §7 exists because of one missing capability:
`RenderScheduler` has exactly one recipe per image
(`ImageState::last_recipe`) and installing a recipe is the same act as
asking for a render (`set_recipe`). There is no way to render one frame
under a recipe without also making that recipe the image's standing one.

Two shapes would fix it, either would do:

- `RenderScheduler::render_once(image, recipe, pv, view) -> ticket`, a
  one-off render that does not touch `last_recipe` and publishes to a slot
  the caller names. The restore render disappears, and so does the
  suspend-other-submits rule, because the before job would no longer be
  competing for the image's recipe slot.
- `CanvasFrame` carrying `Arc<wgpu::Texture>` alongside its `TextureView`,
  plus a way to pin a published generation against ring reuse. That would
  let the shell keep a frame instead of copying it, removing the compute
  kernel and the owned texture, though not the second render.

Neither was added: the brief for this work was explicit that
`lightbox-render` is not to be edited. The workaround costs one extra
render per capture and about 45 MB of texture, both measured in §8 and in
`before_after.rs`'s module docs.

## 10. Deliberately not built

- **No "Copy Before".** Lightroom's before moves when you copy the current
  settings onto it. Lightbox has no such command
  (`lightbox_core::EditCommand`), and inventing one is an edit-model
  decision, not a canvas one. `before_after::before_recipe` is the single
  function that would change.
- **No independent panning of the two panes.** Both sides share one zoom
  and one pan by construction, so they can never drift out of register.
  Comparing two different framings is not a comparison.
- **No vertical split.** The divider is horizontal-only (a vertical
  position along x). Lightroom offers both; nothing about the geometry here
  forbids the other, it is simply unbuilt.
- **Gizmos in side-by-side route against the after pane.** A click on the
  before pane maps outside the after image and falls through to pan. That
  is the correct-by-accident behaviour, not a designed one; a gizmo that
  wants to be usable in side-by-side needs its own thinking.
