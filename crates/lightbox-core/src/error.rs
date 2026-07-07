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
    /// The E09 edit store (`lightbox-edit`) failed: recipe decode, a typed
    /// history/snapshot error, or a schema-too-new doc.
    #[error(transparent)]
    Edit(#[from] lightbox_edit::StoreError),
    /// A recipe/delta failed to apply or validate (`EditHub::update_gesture`).
    #[error(transparent)]
    Recipe(#[from] lightbox_edit::RecipeError),
    /// The develop-preset store failed (open/scan/create/import/export).
    #[error(transparent)]
    Preset(#[from] lightbox_edit::PresetError),
    /// The settings-transfer engine (sync/paste/previous/reset) failed.
    #[error(transparent)]
    Transfer(#[from] lightbox_edit::TransferError),
    /// The E03 preview store failed to open (`.lbdata`'s `previews/`
    /// cache surface — `lightbox-preview`'s `Store::open`, Phase A/B).
    #[error(transparent)]
    Preview(#[from] lightbox_preview::PreviewError),
    /// Invariant violation inside the core (a bug).
    #[error("internal core error: {0}")]
    Internal(String),
}

/// Crate-wide result alias.
pub type Result<T, E = CoreError> = std::result::Result<T, E>;
