// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Store-level error taxonomy (E03 spec §5.5, Phase A T01/T02).
//!
//! [`crate::PreviewError`] (the E01 seed's provider-facing error, extended
//! here — see `lib.rs`) covers the decode/provider surface; [`StoreError`]
//! is the narrower type the spec's [`crate::BlobStore`] signatures name
//! explicitly (`put`/`get`/`remove` never need the wider taxonomy).

/// Everything that can go wrong reading or writing a content-addressed blob.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// Filesystem-level failure (create dirs, write, rename, read, remove).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The bytes read back do not hash to the requested [`crate::BlobRef`] —
    /// a torn or corrupted file. The caller (spec §5.5) treats this as a
    /// miss: the entry is removed so a future `put` can heal it.
    #[error("blob {0} failed its content-address checksum (torn or corrupted)")]
    Corrupt(String),
}
