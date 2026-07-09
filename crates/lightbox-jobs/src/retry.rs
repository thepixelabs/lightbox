// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Retry-with-backoff helper (E06 spec §2 item 7, T14) for consumers with
//! flaky externalities — E13 model downloads being the canonical one.
//!
//! Exponential backoff with jitter, **cancel-aware between attempts**
//! (spec T14 AC: cancel during backoff returns `Cancelled` immediately —
//! it never waits out the backoff sleep).

use std::hash::{BuildHasher, Hasher, RandomState};
use std::time::Duration;

use crate::token::Interrupted;
use crate::CancelToken;

/// Backoff policy. Build via [`RetryPolicy::default`] and override fields,
/// or [`RetryPolicy::attempts`] for the common case.
#[derive(Clone, Debug)]
pub struct RetryPolicy {
    /// Total attempts (first try included). 1 = no retries.
    pub max_attempts: u32,
    /// Backoff before the second attempt.
    pub initial_backoff: Duration,
    /// Backoff ceiling.
    pub max_backoff: Duration,
    /// Per-attempt multiplier (2.0 = doubling).
    pub multiplier: f64,
    /// Jitter fraction in `[0, 1]`: each backoff is scaled by a uniform
    /// factor in `[1 − jitter/2, 1 + jitter/2]` (decorrelates a thundering
    /// herd of retries).
    pub jitter: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(30),
            multiplier: 2.0,
            jitter: 0.5,
        }
    }
}

impl RetryPolicy {
    /// The default policy with a different attempt count.
    pub fn attempts(max_attempts: u32) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            ..RetryPolicy::default()
        }
    }

    /// The (jittered) backoff before attempt `attempt + 1` (0-based).
    fn backoff(&self, attempt: u32) -> Duration {
        let base = self.initial_backoff.as_secs_f64()
            * self.multiplier.max(1.0).powi(attempt.min(63) as i32);
        let base = base.min(self.max_backoff.as_secs_f64());
        let jitter = self.jitter.clamp(0.0, 1.0);
        // Jitter without a rand dependency: the std hasher's per-process
        // random keys give a cheap uniform-ish scalar. Not cryptographic;
        // does not need to be.
        let noise = RandomState::new().build_hasher().finish();
        #[allow(clippy::cast_precision_loss)]
        let unit = (noise >> 11) as f64 / (1u64 << 53) as f64; // [0, 1)
        let factor = 1.0 - jitter / 2.0 + unit * jitter;
        Duration::from_secs_f64((base * factor).max(0.0))
    }
}

/// Why [`retry`] gave up.
#[derive(Debug, thiserror::Error)]
pub enum RetryError<E> {
    /// Cancelled (before an attempt or during a backoff wait).
    #[error("cancelled")]
    Cancelled,
    /// Every attempt failed; carries the **last** error.
    #[error("all {attempts} attempts failed: {last}")]
    Exhausted {
        /// Attempts made.
        attempts: u32,
        /// The final attempt's error.
        last: E,
    },
}

/// Runs `f` up to `policy.max_attempts` times with exponential backoff +
/// jitter between attempts. `f` receives the 0-based attempt number.
///
/// Cancel-awareness: checked before every attempt and **raced against the
/// backoff sleep** — a cancel during backoff returns
/// [`RetryError::Cancelled`] immediately (T14 AC). A cancel *during* an
/// attempt is the attempt's own business (thread `cancel` into it).
pub async fn retry<T, E, F, Fut>(
    policy: &RetryPolicy,
    cancel: &CancelToken,
    mut f: F,
) -> Result<T, RetryError<E>>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let attempts = policy.max_attempts.max(1);
    let mut last: Option<E> = None;
    for attempt in 0..attempts {
        if cancel.is_cancelled() {
            return Err(RetryError::Cancelled);
        }
        if attempt > 0 {
            let backoff = policy.backoff(attempt - 1);
            tokio::select! {
                () = cancel.cancelled() => return Err(RetryError::Cancelled),
                () = tokio::time::sleep(backoff) => {}
            }
        }
        match f(attempt).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                tracing::debug!(
                    target: "lightbox_jobs",
                    attempt,
                    of = attempts,
                    "retryable attempt failed"
                );
                last = Some(err);
            }
        }
    }
    Err(RetryError::Exhausted {
        attempts,
        last: last.expect("at least one attempt ran"),
    })
}

/// Maps an exhausted retry into the job error vocabulary (the
/// `ActivityEntry::error` surfacing path): `Cancelled` stays a normal
/// outcome; `Exhausted` becomes a user-visible failure string.
impl<E: std::fmt::Display> From<RetryError<E>> for crate::JobError {
    fn from(err: RetryError<E>) -> crate::JobError {
        match err {
            RetryError::Cancelled => crate::JobError::Cancelled,
            RetryError::Exhausted { .. } => crate::JobError::Failed(err.to_string()),
        }
    }
}
