// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Error taxonomy of the core façade. Downstream (shell/CLI) sees this one
//! type; catalog/engine errors are wrapped, never re-thrown as SQL or wgpu
//! specifics (seam 1).

/// Everything that can go wrong starting a core, opening a session, or
/// closing one.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// The catalog layer failed (open/create/backup/migration).
    #[error(transparent)]
    Catalog(#[from] lightbox_catalog::CatalogError),
    /// The render engine could not be constructed.
    #[error(transparent)]
    Engine(#[from] lightbox_render::ng::EngineInitError),
    /// Filesystem-level failure outside the catalog.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Invariant violation inside the core (a bug).
    #[error("internal core error: {0}")]
    Internal(String),
}

/// Crate-wide result alias.
pub type Result<T, E = CoreError> = std::result::Result<T, E>;
