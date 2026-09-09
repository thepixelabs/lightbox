// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! D1, the declarative action registry with innermost-context-wins
//! resolution, rebind + conflict queries, and the delta-only override
//! store D3 persists (spec §6.8).
//!
//! Contexts form a runtime **stack** (`"app"` → `"editor"` → one of the
//! editor sub-contexts → `"editor.gizmo.<id>"`); [`KeymapRegistry::resolve`]
//! walks it innermost-first, so a chord bound in `editor.loupe` shadows the
//! same chord bound in `editor`, by design, not a conflict.
//! [`KeymapRegistry::conflicts_with`] answers the *would-they-collide*
//! question for one context instead (used by rebind validation and, later,
//! the D5 rebind editor, cut from this phase).

use std::collections::{BTreeMap, HashMap};

use super::chord::Chord;

/// Stable, dotted action identity (`"nav.next"`). The string is the
/// persistence key in `keymap.toml`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ActionId(pub &'static str);

/// A keymap context (`"editor.loupe"`). Dotted names express nesting for
/// conflict queries; the *runtime* nesting authority is the context stack.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ContextId(pub &'static str);

/// One registered action (spec §6.8).
#[derive(Clone, Copy, Debug)]
pub struct ActionDef {
    /// Stable identity.
    pub id: ActionId,
    /// Human label (cheat sheet, D5 editor).
    pub label: &'static str,
    /// Cheat-sheet grouping.
    pub category: &'static str,
    /// Contexts in which the action is available.
    pub contexts: &'static [ContextId],
    /// Default binding (`None` = shipped unbound).
    pub default: Option<Chord>,
    /// Whether OS key-repeat re-fires the action (D2).
    pub repeatable: bool,
}

/// A rejected [`KeymapRegistry::rebind`]: the chord is already taken by
/// actions reachable from the same context(s).
///
/// `#[allow(dead_code)]` items in this module: the rebind/reset/conflict
/// API is the spec-§6.8 surface the **D5 rebind editor** (this phase's
/// named cut-line, hosted by Phase G's prefs panel) drives; it is
/// complete and test-exercised now so D5 is pure UI. No runtime caller
/// exists until then.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub struct RebindConflict {
    /// The contested chord.
    pub chord: Chord,
    /// Who already holds it.
    pub with: Vec<ActionId>,
}

impl std::fmt::Display for RebindConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.with.iter().map(|id| id.0).collect();
        write!(f, "chord already bound to {}", names.join(", "))
    }
}

/// Action definitions + user overrides (the delta D3 persists) + rows from
/// `keymap.toml` naming actions this build doesn't know (preserved verbatim
/// so a newer build's rebinds survive an older build's save, spec §5.2).
pub struct KeymapRegistry {
    defs: Vec<ActionDef>,
    by_id: HashMap<&'static str, usize>,
    /// User rebinds only (`None` = explicitly unbound). An override equal
    /// to the default is never stored, [`KeymapRegistry::rebind`] and the
    /// D3 loader both maintain that invariant, so "delta-only" holds by
    /// construction.
    overrides: HashMap<ActionId, Option<Chord>>,
    /// Unknown `keymap.toml` rows: id → verbatim chord string.
    pub(crate) foreign: BTreeMap<String, String>,
}

impl KeymapRegistry {
    /// An empty registry (M1 callers want [`super::default_registry`]).
    pub fn new() -> KeymapRegistry {
        KeymapRegistry {
            defs: Vec::new(),
            by_id: HashMap::new(),
            overrides: HashMap::new(),
            foreign: BTreeMap::new(),
        }
    }

    /// Registers an action. **Panics on a duplicate id**, that's a
    /// developer error caught at registration time (spec §6.8), never a
    /// runtime condition.
    pub fn register(&mut self, def: ActionDef) {
        assert!(
            self.by_id.insert(def.id.0, self.defs.len()).is_none(),
            "duplicate keymap action id {:?} (registration-time dev error)",
            def.id.0
        );
        self.defs.push(def);
    }

    /// All registered actions, in registration order (the cheat sheet's
    /// row order).
    pub fn defs(&self) -> &[ActionDef] {
        &self.defs
    }

    /// The definition behind an id.
    pub fn def(&self, id: ActionId) -> Option<&ActionDef> {
        self.by_id.get(id.0).map(|&i| &self.defs[i])
    }

    /// The definition behind a *runtime* (file-sourced) id string.
    pub(crate) fn def_by_str(&self, id: &str) -> Option<&ActionDef> {
        self.by_id.get(id).map(|&i| &self.defs[i])
    }

    /// The LIVE binding: the user's override when present, else the
    /// default. `None` = unbound.
    pub fn binding(&self, id: ActionId) -> Option<Chord> {
        match self.overrides.get(&id) {
            Some(over) => *over,
            None => self.def(id).and_then(|d| d.default),
        }
    }

    /// True when the action is on its shipped default (no override row).
    #[allow(dead_code)] // D5-editor seam — see `RebindConflict`'s doc
    pub fn is_default(&self, id: ActionId) -> bool {
        !self.overrides.contains_key(&id)
    }

    /// The user's rebind delta, for D3's save (id → live binding, only
    /// where it differs from the default).
    pub(crate) fn overrides_delta(&self) -> impl Iterator<Item = (ActionId, Option<Chord>)> + '_ {
        self.overrides.iter().map(|(id, c)| (*id, *c))
    }

    /// Resolves a chord against the context stack, **innermost context
    /// wins** (spec §6.8). Within one context, registration order breaks
    /// ties deterministically (a tie is a conflict
    /// [`Self::conflicts_with`] surfaces it; resolution never guesses
    /// randomly).
    pub fn resolve(&self, chord: Chord, stack: &[ContextId]) -> Option<ActionId> {
        for ctx in stack.iter().rev() {
            for def in &self.defs {
                if def.contexts.contains(ctx) && self.binding(def.id) == Some(chord) {
                    return Some(def.id);
                }
            }
        }
        None
    }

    /// Rebinds (or with `None`, unbinds) an action. Fails with the holders
    /// when the chord is already reachable from any of the action's own
    /// contexts (same context, dotted ancestor/descendant, or the always-
    /// on-stack `"app"` root). Rebinding *back to the default* clears the
    /// override, the delta-only discipline D3 persists.
    ///
    /// Panics on an unregistered id (dev error, same posture as
    /// [`Self::register`]).
    #[allow(dead_code)] // D5-editor seam — see `RebindConflict`'s doc
    pub fn rebind(&mut self, id: ActionId, chord: Option<Chord>) -> Result<(), RebindConflict> {
        let def = *self
            .def(id)
            .unwrap_or_else(|| panic!("rebind of unregistered action {:?}", id.0));
        if let Some(c) = chord {
            let mut with: Vec<ActionId> = Vec::new();
            for ctx in def.contexts {
                for holder in self.conflicts_with(c, *ctx) {
                    if holder != id && !with.contains(&holder) {
                        with.push(holder);
                    }
                }
            }
            if !with.is_empty() {
                return Err(RebindConflict { chord: c, with });
            }
        }
        self.set_override(id, chord);
        Ok(())
    }

    /// D3's tolerant loader path: applies an override **without** conflict
    /// rejection (a user's file is applied as written; shadowing keeps
    /// resolution deterministic and the conflict query still reports it).
    pub(crate) fn force_bind(&mut self, id: ActionId, chord: Option<Chord>) {
        self.set_override(id, chord);
    }

    fn set_override(&mut self, id: ActionId, chord: Option<Chord>) {
        let default = self.def(id).and_then(|d| d.default);
        if chord == default {
            self.overrides.remove(&id);
        } else {
            self.overrides.insert(id, chord);
        }
    }

    /// Every action whose LIVE binding is `chord` and which is reachable
    /// from `ctx`, same context, dotted ancestor/descendant, or the
    /// `"app"` root (always on the stack).
    #[allow(dead_code)] // D5-editor seam — see `RebindConflict`'s doc
    pub fn conflicts_with(&self, chord: Chord, ctx: ContextId) -> Vec<ActionId> {
        self.defs
            .iter()
            .filter(|def| self.binding(def.id) == Some(chord))
            .filter(|def| def.contexts.iter().any(|c| contexts_overlap(*c, ctx)))
            .map(|def| def.id)
            .collect()
    }

    /// Actions declared for `ctx`, with their live bindings (cheat-sheet /
    /// D5 rows).
    #[allow(dead_code)] // D5-editor seam — see `RebindConflict`'s doc
    pub fn bindings_for(&self, ctx: ContextId) -> Vec<(&ActionDef, Option<Chord>)> {
        self.defs
            .iter()
            .filter(|def| def.contexts.contains(&ctx))
            .map(|def| (def, self.binding(def.id)))
            .collect()
    }

    /// Drops one action's override (back to default).
    #[allow(dead_code)] // D5-editor seam — see `RebindConflict`'s doc
    pub fn reset(&mut self, id: ActionId) {
        self.overrides.remove(&id);
    }

    /// Drops every override. Unknown-id rows (`foreign`) are NOT ours to
    /// drop, they belong to another build, so they survive.
    #[allow(dead_code)] // D5-editor seam — see `RebindConflict`'s doc
    pub fn reset_all(&mut self) {
        self.overrides.clear();
    }
}

impl Default for KeymapRegistry {
    fn default() -> Self {
        KeymapRegistry::new()
    }
}

/// Would bindings in `a` and `b` ever compete on one context stack?
/// True for the same context, a dotted ancestor/descendant pair, or when
/// either is the `"app"` root (which is on every stack).
#[allow(dead_code)] // reached only via `conflicts_with` (D5 seam)
fn contexts_overlap(a: ContextId, b: ContextId) -> bool {
    fn is_descendant(child: &str, parent: &str) -> bool {
        child.len() > parent.len()
            && child.starts_with(parent)
            && child.as_bytes()[parent.len()] == b'.'
    }
    a == b || a.0 == "app" || b.0 == "app" || is_descendant(a.0, b.0) || is_descendant(b.0, a.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Key;

    const CTX_APP: ContextId = ContextId("app");
    const CTX_EDITOR: ContextId = ContextId("editor");
    const CTX_LOUPE: ContextId = ContextId("editor.loupe");

    fn def(id: &'static str, contexts: &'static [ContextId], default: Option<Chord>) -> ActionDef {
        ActionDef {
            id: ActionId(id),
            label: "test",
            category: "Test",
            contexts,
            default,
            repeatable: false,
        }
    }

    fn registry() -> KeymapRegistry {
        let mut r = KeymapRegistry::new();
        r.register(def(
            "editor.thing",
            &[CTX_EDITOR],
            Some(Chord::plain(Key::X)),
        ));
        r.register(def("loupe.thing", &[CTX_LOUPE], Some(Chord::plain(Key::X))));
        r.register(def("app.thing", &[CTX_APP], Some(Chord::cmd(Key::X))));
        r
    }

    /// D1 AC: context shadowing, innermost wins; popping the context
    /// re-exposes the outer binding.
    #[test]
    fn innermost_context_wins_and_pops_cleanly() {
        let r = registry();
        let x = Chord::plain(Key::X);
        assert_eq!(
            r.resolve(x, &[CTX_APP, CTX_EDITOR, CTX_LOUPE]),
            Some(ActionId("loupe.thing")),
            "loupe shadows editor while on the stack"
        );
        assert_eq!(
            r.resolve(x, &[CTX_APP, CTX_EDITOR]),
            Some(ActionId("editor.thing")),
            "outer binding resolves once the inner context pops"
        );
        assert_eq!(r.resolve(x, &[CTX_APP]), None);
        assert_eq!(
            r.resolve(Chord::cmd(Key::X), &[CTX_APP, CTX_EDITOR, CTX_LOUPE]),
            Some(ActionId("app.thing")),
            "root bindings stay reachable under any stack"
        );
    }

    /// D1 AC: duplicate-id registration panics (dev error, registration
    /// time).
    #[test]
    #[should_panic(expected = "duplicate keymap action id")]
    fn duplicate_id_panics_at_registration() {
        let mut r = registry();
        r.register(def("editor.thing", &[CTX_EDITOR], None));
    }

    /// D1 AC: conflict detection, same context, nested contexts, and the
    /// app root all report; siblings don't.
    #[test]
    fn conflict_query_sees_nesting_and_the_app_root() {
        let mut r = registry();
        r.register(def(
            "filmstrip.thing",
            &[ContextId("editor.filmstrip")],
            Some(Chord::plain(Key::X)),
        ));
        let x = Chord::plain(Key::X);
        let in_loupe = r.conflicts_with(x, CTX_LOUPE);
        assert!(in_loupe.contains(&ActionId("loupe.thing")));
        assert!(
            in_loupe.contains(&ActionId("editor.thing")),
            "ancestor-context binding is reachable in the nested context"
        );
        assert!(
            !in_loupe.contains(&ActionId("filmstrip.thing")),
            "sibling contexts never conflict"
        );
        // The app root conflicts with everything (it's on every stack).
        let cmd_x = r.conflicts_with(Chord::cmd(Key::X), CTX_LOUPE);
        assert_eq!(cmd_x, vec![ActionId("app.thing")]);
    }

    #[test]
    fn rebind_conflict_names_the_holders_and_leaves_state_untouched() {
        let mut r = registry();
        let err = r
            .rebind(ActionId("editor.thing"), Some(Chord::cmd(Key::X)))
            .unwrap_err();
        assert_eq!(err.with, vec![ActionId("app.thing")]);
        assert_eq!(
            r.binding(ActionId("editor.thing")),
            Some(Chord::plain(Key::X)),
            "failed rebind must not change the binding"
        );
        assert!(r.is_default(ActionId("editor.thing")));
    }

    #[test]
    fn rebind_unbind_and_reset_round_trip() {
        let mut r = registry();
        let id = ActionId("editor.thing");
        r.rebind(id, Some(Chord::plain(Key::Y))).unwrap();
        assert_eq!(r.binding(id), Some(Chord::plain(Key::Y)));
        assert!(!r.is_default(id));

        r.rebind(id, None).unwrap(); // explicit unbind
        assert_eq!(r.binding(id), None);

        r.reset(id);
        assert_eq!(r.binding(id), Some(Chord::plain(Key::X)));
        assert!(r.is_default(id));
    }

    /// Delta-only discipline: rebinding back to the default clears the
    /// override instead of storing a no-op row. (Uses `app.thing`, whose
    /// default no other action shadows, returning `editor.thing` to X
    /// would correctly trip the conflict check against `loupe.thing`.)
    #[test]
    fn rebinding_to_the_default_is_not_an_override() {
        let mut r = registry();
        let id = ActionId("app.thing");
        r.rebind(id, Some(Chord::cmd(Key::Y))).unwrap();
        r.rebind(id, Some(Chord::cmd(Key::X))).unwrap();
        assert!(r.is_default(id), "manual return to default clears the row");
        assert_eq!(r.overrides_delta().count(), 0);
    }

    #[test]
    fn reset_all_clears_overrides_but_not_foreign_rows() {
        let mut r = registry();
        r.rebind(ActionId("editor.thing"), Some(Chord::plain(Key::Y)))
            .unwrap();
        r.foreign
            .insert("future.action".to_owned(), "Cmd+9".to_owned());
        r.reset_all();
        assert!(r.is_default(ActionId("editor.thing")));
        assert_eq!(r.foreign.len(), 1, "unknown ids are another build's data");
    }

    #[test]
    fn bindings_for_lists_live_bindings_in_registration_order() {
        let mut r = registry();
        r.rebind(ActionId("editor.thing"), Some(Chord::plain(Key::Y)))
            .unwrap();
        let rows = r.bindings_for(CTX_EDITOR);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.id, ActionId("editor.thing"));
        assert_eq!(rows[0].1, Some(Chord::plain(Key::Y)), "live, not default");
    }
}
