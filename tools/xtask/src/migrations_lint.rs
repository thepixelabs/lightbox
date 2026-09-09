// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! `cargo xtask lint-migrations`, the migration-number registry lint
//! (E01 spec §5 T9, §4.4/OQ-2).
//!
//! Cross-checks `docs/plan/migrations.md` (the committed registry that
//! parallel epics reserve numbers in) against the embedded migration files
//! in `crates/lightbox-catalog/migrations/`. Fails on: duplicate numbers in
//! either place, a migration file without a registry row, a name mismatch,
//! or a gap in the shipped sequence (the runner applies strictly in order).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;

/// A row of the registry table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegistryRow {
    pub number: u32,
    pub name: String,
}

/// Runs the lint against the workspace; prints a summary on success.
pub(crate) fn lint(workspace_root: &Path) -> anyhow::Result<()> {
    let registry_path = workspace_root.join("docs/plan/migrations.md");
    let registry_text = std::fs::read_to_string(&registry_path)
        .with_context(|| format!("reading {}", registry_path.display()))?;
    let migrations_dir = workspace_root.join("crates/lightbox-catalog/migrations");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&migrations_dir)
        .with_context(|| format!("reading {}", migrations_dir.display()))?
    {
        files.push(entry?.file_name().to_string_lossy().into_owned());
    }

    let (rows, shipped) = check(&registry_text, &files).map_err(|problems| {
        anyhow::anyhow!(
            "migration registry lint failed:\n  - {}",
            problems.join("\n  - ")
        )
    })?;
    println!(
        "migration registry ok: {} reserved number{}, {} shipped migration file{}",
        rows,
        if rows == 1 { "" } else { "s" },
        shipped,
        if shipped == 1 { "" } else { "s" },
    );
    Ok(())
}

/// Pure lint core: registry markdown + migration-dir file names →
/// `(reserved_rows, shipped_files)` or the list of problems.
pub(crate) fn check(
    registry_text: &str,
    migration_files: &[String],
) -> Result<(usize, usize), Vec<String>> {
    let mut problems = Vec::new();

    // Parse registry table rows: `| 0001 | E01 | spine | … |`.
    let mut registry: BTreeMap<u32, RegistryRow> = BTreeMap::new();
    for line in registry_text.lines() {
        let Some(row) = parse_registry_row(line) else {
            continue;
        };
        if let Some(prev) = registry.get(&row.number) {
            problems.push(format!(
                "duplicate registry number {:04}: {:?} and {:?}",
                row.number, prev.name, row.name
            ));
            continue;
        }
        registry.insert(row.number, row);
    }
    if registry.is_empty() {
        problems.push("registry table has no rows".to_owned());
    }

    // Parse migration files: `NNNN_name.sql`.
    let mut shipped: BTreeMap<u32, String> = BTreeMap::new();
    for file in migration_files {
        let Some((number, name)) = parse_migration_filename(file) else {
            problems.push(format!(
                "migration file {file:?} does not match NNNN_name.sql"
            ));
            continue;
        };
        if let Some(prev) = shipped.get(&number) {
            problems.push(format!(
                "duplicate migration file number {number:04}: {prev:?} and {name:?}"
            ));
            continue;
        }
        shipped.insert(number, name);
    }

    // Every shipped file must be registered under the same name…
    for (number, name) in &shipped {
        match registry.get(number) {
            None => problems.push(format!(
                "migration file {number:04}_{name}.sql has no registry row \
                 (reserve the number in docs/plan/migrations.md)"
            )),
            Some(row) if row.name != *name => problems.push(format!(
                "migration {number:04} name mismatch: file says {name:?}, \
                 registry says {:?}",
                row.name
            )),
            Some(_) => {}
        }
    }
    // …and shipped numbers must be contiguous from 1.
    for (i, number) in shipped.keys().enumerate() {
        let expected = u32::try_from(i).expect("count fits u32") + 1;
        if *number != expected {
            problems.push(format!(
                "shipped migrations must be contiguous from 0001: expected \
                 {expected:04}, found {number:04}"
            ));
            break;
        }
    }

    if problems.is_empty() {
        Ok((registry.len(), shipped.len()))
    } else {
        Err(problems)
    }
}

/// Parses `| 0001 | E01 | spine | status |`; `None` for non-row lines and
/// the header/divider.
fn parse_registry_row(line: &str) -> Option<RegistryRow> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix('|')?;
    let cells: Vec<&str> = rest
        .strip_suffix('|')
        .unwrap_or(rest)
        .split('|')
        .map(str::trim)
        .collect();
    if cells.len() < 3 {
        return None;
    }
    let number_cell = cells[0];
    if number_cell.len() != 4 || !number_cell.bytes().all(|b| b.is_ascii_digit()) {
        return None; // header, divider, or prose
    }
    Some(RegistryRow {
        number: number_cell.parse().ok()?,
        name: cells[2].to_owned(),
    })
}

/// Parses `NNNN_name.sql` → `(NNNN, name)`.
fn parse_migration_filename(file: &str) -> Option<(u32, String)> {
    let stem = file.strip_suffix(".sql")?;
    let (num, name) = stem.split_once('_')?;
    if num.len() != 4 || !num.bytes().all(|b| b.is_ascii_digit()) || name.is_empty() {
        return None;
    }
    Some((num.parse().ok()?, name.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTRY: &str = "\
# registry\n\
| number | epic | name | status |\n\
|--------|------|------|--------|\n\
| 0001   | E01  | spine | shipped |\n\
| 0002   | E03  | preview | reserved |\n";

    fn files(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn clean_registry_passes() {
        let (rows, shipped) = check(REGISTRY, &files(&["0001_spine.sql"])).unwrap();
        assert_eq!((rows, shipped), (2, 1));
    }

    #[test]
    fn duplicate_registry_number_fails() {
        let registry = format!("{REGISTRY}| 0002 | E07 | collections | reserved |\n");
        let problems = check(&registry, &files(&["0001_spine.sql"])).unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("duplicate registry number 0002")),
            "{problems:?}"
        );
    }

    #[test]
    fn duplicate_migration_file_number_fails() {
        let problems = check(REGISTRY, &files(&["0001_spine.sql", "0001_other.sql"])).unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("duplicate migration file number 0001")),
            "{problems:?}"
        );
    }

    #[test]
    fn unregistered_migration_file_fails() {
        let problems = check(
            REGISTRY,
            &files(&["0001_spine.sql", "0002_preview.sql", "0003_rogue.sql"]),
        )
        .unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("0003_rogue.sql has no registry row")),
            "{problems:?}"
        );
    }

    #[test]
    fn name_mismatch_fails() {
        let problems = check(REGISTRY, &files(&["0001_backbone.sql"])).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("name mismatch")),
            "{problems:?}"
        );
    }

    #[test]
    fn gap_in_shipped_sequence_fails() {
        let registry = format!("{REGISTRY}| 0003 | E07 | collections | shipped |\n");
        let problems = check(
            &registry,
            &files(&["0001_spine.sql", "0003_collections.sql"]),
        )
        .unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("contiguous")),
            "{problems:?}"
        );
    }

    #[test]
    fn malformed_filename_fails() {
        let problems = check(REGISTRY, &files(&["0001_spine.sql", "extra.txt"])).unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("does not match NNNN_name.sql")),
            "{problems:?}"
        );
    }

    #[test]
    fn real_workspace_registry_is_clean() {
        // The committed registry + the real migrations dir must lint clean.
        let root = crate::workspace_root();
        lint(&root).expect("workspace migration registry must lint clean");
    }
}
