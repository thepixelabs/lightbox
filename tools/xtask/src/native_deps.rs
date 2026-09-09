// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `cargo xtask lint-native-deps`, the SBOM surface-2 placeholder check
//! (E01 spec §8 DoD 6: "verified by the SBOM-inventory placeholder check
//! in CI"; real SBOM tooling is E16).
//!
//! Cross-checks every `*-sys` package in `Cargo.lock` against the
//! committed classification in `native-inventory.toml`: a new native
//! dependency fails CI until a human classifies it (and, for `bundled-c`,
//! owns the license surface-2/3 consequences). Stale inventory entries
//! fail too, the inventory never drifts from the lockfile.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, Context};
use serde::Deserialize;

#[derive(Deserialize)]
struct Inventory {
    #[serde(default, rename = "package")]
    packages: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    name: String,
    kind: String,
    note: String,
}

#[derive(Deserialize)]
struct Lockfile {
    #[serde(default, rename = "package")]
    packages: Vec<LockPackage>,
}

#[derive(Deserialize)]
struct LockPackage {
    name: String,
}

const KINDS: &[&str] = &["bundled-c", "os-binding", "wasm-binding"];

pub fn lint(root: &Path) -> anyhow::Result<()> {
    let lock_path = root.join("Cargo.lock");
    let inv_path = root.join("native-inventory.toml");
    let lock: Lockfile = toml::from_str(
        &std::fs::read_to_string(&lock_path)
            .with_context(|| format!("reading {}", lock_path.display()))?,
    )
    .context("parsing Cargo.lock")?;
    let inventory: Inventory = toml::from_str(
        &std::fs::read_to_string(&inv_path)
            .with_context(|| format!("reading {}", inv_path.display()))?,
    )
    .context("parsing native-inventory.toml")?;

    let locked: BTreeSet<&str> = lock
        .packages
        .iter()
        .map(|p| p.name.as_str())
        .filter(|n| n.ends_with("-sys"))
        .collect();
    let inventoried: BTreeSet<&str> = inventory.packages.iter().map(|e| e.name.as_str()).collect();

    let mut errors: Vec<String> = Vec::new();
    for name in locked.difference(&inventoried) {
        errors.push(format!(
            "unclassified native-linkage crate in Cargo.lock: {name} \
             — add it to native-inventory.toml (bundled-c entries grow \
             license surface 2; see the file header)"
        ));
    }
    for name in inventoried.difference(&locked) {
        errors.push(format!(
            "stale native-inventory.toml entry: {name} is not in Cargo.lock"
        ));
    }
    for entry in &inventory.packages {
        if !KINDS.contains(&entry.kind.as_str()) {
            errors.push(format!(
                "{}: unknown kind {:?} (expected one of {KINDS:?})",
                entry.name, entry.kind
            ));
        }
        if entry.note.trim().is_empty() {
            errors.push(format!("{}: empty note — say why it's here", entry.name));
        }
    }
    if !errors.is_empty() {
        for err in &errors {
            eprintln!("lint-native-deps: {err}");
        }
        bail!(
            "native-dependency inventory check failed ({} error(s))",
            errors.len()
        );
    }

    let bundled: Vec<&str> = inventory
        .packages
        .iter()
        .filter(|e| e.kind == "bundled-c")
        .map(|e| e.name.as_str())
        .collect();
    println!(
        "lint-native-deps ok: {} -sys crate(s) classified; license surface-2 \
         placeholder inventory (bundled C): {}",
        locked.len(),
        if bundled.is_empty() {
            "EMPTY".to_owned()
        } else {
            bundled.join(", ")
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_inventory_is_in_sync() {
        lint(&crate::workspace_root()).expect("inventory must match Cargo.lock");
    }
}
