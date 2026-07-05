// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! `lightbox-color` — the E02 color foundation: the license-clean camera-matrix
//! base (§1.7 tier 1), the Lightbox default look (tier 2), the DCP evaluator
//! (tier 3), white balance, and ICC color management (working / display /
//! output). Pure math + LCMS2 FFI; every output is plain-data transform spec
//! any backend (E05 WGSL, E15 export) can consume.
//!
//! # Scaffold state (E02 Phase A)
//!
//! Phase A lands the **full module surface** so the Wave-1..3 phases fill
//! DISJOINT files. Every §3 public type is a real definition; function bodies a
//! later phase owns are `unimplemented!("<phase> …")` and named in the module
//! docs. Owner map (E02 §0 / §6):
//!
//! | module | owner | tasks |
//! |--------|-------|-------|
//! | [`matrix`], [`cct`], [`wb`], [`lut`], [`transform`] | **B** | B1–B8 |
//! | [`cms`], [`display`], [`output`] | **D** | D1–D6 |
//! | [`look`] | **E** | E1–E2 |
//! | [`dcp`], [`profile`] | **F** | F1–F7 |
//! | [`error`] | A | taxonomy (this phase) |
//!
//! Colorimetry inputs ([`lightbox_decode::RawColorimetry`],
//! [`lightbox_decode::CameraId`]) flow in from `lightbox-decode`; this crate
//! never depends the other way.

pub mod cct;
pub mod cms;
pub mod dcp;
pub mod display;
pub mod error;
pub mod look;
pub mod lut;
pub mod matrix;
pub mod output;
pub mod profile;
pub mod transform;
pub mod wb;

pub use error::{ColorError, IccError, LookError};
pub use matrix::{Mat3, Vec3};
pub use profile::{camera_matrix_base, CameraProfile, ProfileId, ProfileSource};
pub use transform::{resolve_input_transform, ResolvedInputTransform};
pub use wb::WbMode;
