// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Command`] — the mutation vocabulary of seam 1 (spec §3.8).
//!
//! Every command executes as **one WAL transaction** on the catalog's
//! single writer (architecture §3.1); results come back asynchronously as
//! broadcast [`crate::Event`]s correlated by [`CommandTicket`].

use std::path::PathBuf;

use lightbox_types::{Flag, ImageId, ImportSessionId};

/// Correlates a submitted command with its outcome events. Allocated by
/// [`crate::Session::submit`]; unique within a session.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct CommandTicket(pub(crate) u64);

impl CommandTicket {
    /// The session-unique id.
    pub fn id(self) -> u64 {
        self.0
    }
}

/// Mutations the shell/CLI may request (spec §3.8, `#[non_exhaustive]` —
/// E04/E07/E09 grow this).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Command {
    /// Add-in-place import of a directory (spec §1 item 4). Progress and
    /// completion arrive as `Import*` events.
    ImportAddInPlace {
        /// Directory to import.
        source_dir: PathBuf,
        /// Descend into subdirectories.
        recursive: bool,
    },
    /// Remove an import session's catalog rows; files on disk untouched.
    UndoImport {
        /// The session to undo.
        session: ImportSessionId,
    },
    /// Set or clear the 1..=5 star rating — the canonical trivial command
    /// proving the txn+event path (spec §3.8; E08 grows the UX).
    SetRating {
        /// Target image.
        image: ImageId,
        /// `None` clears.
        rating: Option<u8>,
    },
    /// Set the pick/reject flag.
    SetFlag {
        /// Target image.
        image: ImageId,
        /// The new flag.
        flag: Flag,
    },
    /// Run a verified backup now (spec §3.2 pipeline); completion arrives
    /// as `Event::BackupFinished`.
    BackupNow,
}
