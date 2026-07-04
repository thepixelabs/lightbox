// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-cli` — headless driver over `lightbox-core` (E01 spec §3.10).
//!
//! Owned by **E01**; the real subcommands (`create`, `import`, `list`,
//! `render`, `backup`, `check`) land in E01 Phase 7 (T27) and are the
//! headless E2E proof of seam 1: this binary must never grow a dependency on
//! `lightbox-shell`, egui, or winit (asserted in CI via `cargo tree`).
//!
//! Error-taxonomy convention (spec T4): `anyhow` is allowed here because this
//! is a binary; library crates use `thiserror`.
//!
//! **Status: skeleton** — reserved by E01 Phase 1 (T1).

use lightbox_core::observability;

fn main() -> anyhow::Result<()> {
    observability::init(&observability::ObservabilityOptions::default())
        .map_err(|e| anyhow::anyhow!(e))?;
    eprintln!(
        "lightbox-cli: subcommands arrive with E01 Phase 7 (T27): \
         create | import | list | render | backup | check"
    );
    std::process::exit(2);
}
