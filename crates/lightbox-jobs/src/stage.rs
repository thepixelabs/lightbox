// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Backpressure primitive (E06 spec §4.6, T13): cancel-aware **bounded**
//! channels between pipeline stages.
//!
//! This is the mechanism by which "import throttles preview-build enqueue
//! so a 10k-raw card can't OOM the queue" (spec §5.3) — the copy stage's
//! [`StageSender::send`] blocks when the preview stage lags. A thin wrapper
//! over `tokio::sync::mpsc` + `select!` with the [`CancelToken`] — boring
//! by design; its value is that every stage boundary in E04/E03/E15 uses
//! the *same* cancel-aware idiom instead of five hand-rolled ones.

use tokio::sync::mpsc;

use crate::token::Interrupted;
use crate::CancelToken;

pub use tokio::sync::mpsc::error::TrySendError;

/// Creates a bounded, cancel-aware stage channel. `capacity` bounds the
/// number of in-flight items (peak queued memory = `capacity` ×
/// item size).
///
/// # Panics
///
/// If `capacity == 0` (a zero-capacity stage can never move an item).
pub fn bounded<T: Send>(capacity: usize) -> (StageSender<T>, StageReceiver<T>) {
    assert!(capacity > 0, "stage::bounded requires capacity >= 1");
    let (tx, rx) = mpsc::channel(capacity);
    (StageSender { tx }, StageReceiver { rx })
}

/// The producing side. Clone for fan-in; the channel closes when every
/// sender is dropped (the receiver then drains and yields `None`).
pub struct StageSender<T> {
    tx: mpsc::Sender<T>,
}

impl<T> Clone for StageSender<T> {
    fn clone(&self) -> Self {
        StageSender {
            tx: self.tx.clone(),
        }
    }
}

impl<T: Send> StageSender<T> {
    /// Awaits capacity; `Err(Interrupted)` if `cancel` fires while waiting
    /// **or the receiver is gone** (either way: stop producing — the item
    /// is dropped, matching cancellation semantics).
    pub async fn send(&self, item: T, cancel: &CancelToken) -> Result<(), Interrupted> {
        if cancel.is_cancelled() {
            return Err(Interrupted);
        }
        tokio::select! {
            () = cancel.cancelled() => Err(Interrupted),
            sent = self.tx.send(item) => sent.map_err(|_| Interrupted),
        }
    }

    /// Non-blocking send: `Full(item)`/`Closed(item)` hand the item back.
    pub fn try_send(&self, item: T) -> Result<(), TrySendError<T>> {
        self.tx.try_send(item)
    }
}

impl<T> std::fmt::Debug for StageSender<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StageSender")
            .field("capacity", &self.tx.max_capacity())
            .finish_non_exhaustive()
    }
}

/// The consuming side (single consumer, matching `tokio::sync::mpsc`).
pub struct StageReceiver<T> {
    rx: mpsc::Receiver<T>,
}

impl<T: Send> StageReceiver<T> {
    /// Awaits the next item; `Ok(None)` when the upstream closed (all
    /// senders dropped — buffered items are drained first);
    /// `Err(Interrupted)` if `cancel` fires while waiting. Cancelling a
    /// parked *producer* is the sender's own token's business — cancelling
    /// the consumer's token here also unblocks parked producers naturally,
    /// because dropping this receiver (the usual next step after
    /// `Interrupted`) closes the channel.
    pub async fn recv(&mut self, cancel: &CancelToken) -> Result<Option<T>, Interrupted> {
        if cancel.is_cancelled() {
            return Err(Interrupted);
        }
        tokio::select! {
            () = cancel.cancelled() => Err(Interrupted),
            item = self.rx.recv() => Ok(item),
        }
    }

    /// Non-blocking receive (drain loops in shutdown paths).
    pub fn try_recv(&mut self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

impl<T> std::fmt::Debug for StageReceiver<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StageReceiver").finish_non_exhaustive()
    }
}
