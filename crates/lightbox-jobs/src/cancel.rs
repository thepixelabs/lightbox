// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! Hierarchical, cooperative cancellation (E01 spec §3.3, frozen surface).
//!
//! A [`CancelToken`] is a cheap-to-clone handle to a shared cancellation flag.
//! [`CancelToken::child`] derives a token that is cancelled whenever its parent
//! is cancelled (but never the other way round), so a coarse operation (an
//! import, a render ticket) can hand scoped tokens to its stages.
//!
//! Cancellation is *cooperative*: work must check [`CancelToken::is_cancelled`]
//! at its checkpoints (spec T14 budgets 50 ms between checkpoints for blocking
//! jobs) or `select!` on [`CancelToken::cancelled`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::Notify;

/// Clone-cheap cancellation handle (spec §3.3: `Arc<AtomicBool>` + `Notify`).
///
/// All clones observe the same flag; children derived via [`CancelToken::child`]
/// are cancelled when any handle to this token is cancelled.
#[derive(Clone)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

struct Inner {
    cancelled: AtomicBool,
    notify: Notify,
    /// Weak so a dropped child costs nothing; pruned on every `cancel`/`child`.
    children: Mutex<Vec<Weak<Inner>>>,
}

impl CancelToken {
    /// A fresh, un-cancelled root token.
    pub fn new() -> CancelToken {
        CancelToken {
            inner: Arc::new(Inner {
                cancelled: AtomicBool::new(false),
                notify: Notify::new(),
                children: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Derives a child token: cancelled when `self` is cancelled; cancelling
    /// the child does **not** cancel `self` (hierarchical cancellation).
    ///
    /// A child derived from an already-cancelled token is born cancelled.
    pub fn child(&self) -> CancelToken {
        let child = CancelToken::new();
        if self.is_cancelled() {
            child.cancel();
            return child;
        }
        {
            let mut children = self.inner.children.lock().expect("children lock poisoned");
            children.retain(|w| w.strong_count() > 0);
            children.push(Arc::downgrade(&child.inner));
        }
        // Racy window: `self` may have been cancelled between the check above
        // and the registration. Re-check so the child can never miss it.
        if self.is_cancelled() {
            child.cancel();
        }
        child
    }

    /// Cancels this token and (recursively) all its children. Idempotent.
    pub fn cancel(&self) {
        Inner::cancel_tree(&self.inner);
    }

    /// True once [`CancelToken::cancel`] was called on this token or an ancestor.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Resolves when the token is cancelled (for `tokio::select!`). Resolves
    /// immediately if already cancelled. Runtime-independent.
    pub async fn cancelled(&self) {
        loop {
            // Create the Notified future BEFORE checking the flag: tokio's
            // `notify_waiters` wakes every future created before the call, so
            // a cancel landing between check and await cannot be missed.
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

impl Inner {
    fn cancel_tree(inner: &Arc<Inner>) {
        if inner.cancelled.swap(true, Ordering::AcqRel) {
            return; // already cancelled; children were handled then
        }
        inner.notify.notify_waiters();
        // Snapshot the children under the lock, recurse outside it so deep
        // trees never hold more than one lock at a time.
        let children: Vec<Arc<Inner>> = {
            let mut guard = inner.children.lock().expect("children lock poisoned");
            let strong = guard.iter().filter_map(Weak::upgrade).collect();
            guard.clear();
            strong
        };
        for child in children {
            Inner::cancel_tree(&child);
        }
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        CancelToken::new()
    }
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let t = CancelToken::new();
        assert!(!t.is_cancelled());
    }

    #[test]
    fn cancel_is_observed_by_clones_and_idempotent() {
        let t = CancelToken::new();
        let c = t.clone();
        t.cancel();
        t.cancel();
        assert!(t.is_cancelled());
        assert!(c.is_cancelled());
    }

    #[test]
    fn parent_cancel_propagates_to_children_and_grandchildren() {
        let root = CancelToken::new();
        let child = root.child();
        let grandchild = child.child();
        root.cancel();
        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
    }

    #[test]
    fn child_cancel_does_not_propagate_upward() {
        let root = CancelToken::new();
        let child = root.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!root.is_cancelled());
    }

    #[test]
    fn child_of_cancelled_parent_is_born_cancelled() {
        let root = CancelToken::new();
        root.cancel();
        assert!(root.child().is_cancelled());
    }

    #[test]
    fn cancelled_future_resolves_immediately_when_already_cancelled() {
        let t = CancelToken::new();
        t.cancel();
        pollster::block_on(t.cancelled());
    }

    #[test]
    fn cancelled_future_resolves_on_later_cancel_from_another_thread() {
        let t = CancelToken::new();
        let t2 = t.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            t2.cancel();
        });
        pollster::block_on(t.cancelled());
        assert!(t.is_cancelled());
        canceller.join().unwrap();
    }

    #[test]
    fn cancelled_future_resolves_via_parent_cancel() {
        let root = CancelToken::new();
        let child = root.child();
        let r2 = root.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            r2.cancel();
        });
        pollster::block_on(child.cancelled());
        canceller.join().unwrap();
    }
}
