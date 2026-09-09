// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! [`default_store_dir`], the default edit-store location for a
//! library-less app (E04 spec §4.5).

use std::path::PathBuf;

/// The default edit-store directory: a per-user application-data location.
/// The caller creates it on first use (matches `Catalog::create`'s own
/// directory creation, this function only computes the path). Overridable
/// everywhere the old `--catalog` flag was accepted (`lightbox-cli open
/// --store <dir>`); the store remains plumbing, never a user-facing library
/// (architecture §3.1.1).
///
/// ```text
/// macOS   ~/Library/Application Support/Lightbox/edits.lbdata
/// Windows %APPDATA%\Lightbox\edits.lbdata
/// Linux   $XDG_DATA_HOME (or ~/.local/share)/lightbox/edits.lbdata
/// ```
///
/// Hand-rolled rather than pulling in the `dirs` crate (E04 spec §2 table
/// names either as acceptable, cargo-deny-clean): each of these three
/// platform conventions is one or two env-var reads, not worth a new
/// dependency for. Recorded in `docs/plan/epics/E04-deviations.md`.
pub fn default_store_dir() -> PathBuf {
    build_store_dir(&Env::from_process())
}

// Each platform's `build_store_dir` arm below reads only its own field(s)
// `#[allow(dead_code)]` because exactly one arm compiles per target, so the
// other two fields are legitimately unread on any given platform.
#[allow(dead_code)]
struct Env {
    home: Option<PathBuf>,
    appdata: Option<PathBuf>,
    xdg_data_home: Option<PathBuf>,
}

impl Env {
    fn from_process() -> Env {
        Env {
            home: std::env::var_os("HOME").map(PathBuf::from),
            appdata: std::env::var_os("APPDATA").map(PathBuf::from),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        }
    }
}

fn build_store_dir(env: &Env) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        env.home
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library")
            .join("Application Support")
            .join("Lightbox")
            .join("edits.lbdata")
    }
    #[cfg(target_os = "windows")]
    {
        env.appdata
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Lightbox")
            .join("edits.lbdata")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env.xdg_data_home
            .clone()
            .or_else(|| env.home.clone().map(|h| h.join(".local").join("share")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lightbox")
            .join("edits.lbdata")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_layout() {
        let env = Env {
            home: Some(PathBuf::from("/Users/testuser")),
            appdata: None,
            xdg_data_home: None,
        };
        assert_eq!(
            build_store_dir(&env),
            PathBuf::from("/Users/testuser/Library/Application Support/Lightbox/edits.lbdata")
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_layout() {
        let env = Env {
            home: None,
            appdata: Some(PathBuf::from(r"C:\Users\testuser\AppData\Roaming")),
            xdg_data_home: None,
        };
        assert_eq!(
            build_store_dir(&env),
            PathBuf::from(r"C:\Users\testuser\AppData\Roaming\Lightbox\edits.lbdata")
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_layout_prefers_xdg_data_home() {
        let env = Env {
            home: Some(PathBuf::from("/home/testuser")),
            appdata: None,
            xdg_data_home: Some(PathBuf::from("/home/testuser/.data")),
        };
        assert_eq!(
            build_store_dir(&env),
            PathBuf::from("/home/testuser/.data/lightbox/edits.lbdata")
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_layout_falls_back_to_home_local_share() {
        let env = Env {
            home: Some(PathBuf::from("/home/testuser")),
            appdata: None,
            xdg_data_home: None,
        };
        assert_eq!(
            build_store_dir(&env),
            PathBuf::from("/home/testuser/.local/share/lightbox/edits.lbdata")
        );
    }

    /// Platform-agnostic smoke check (runs on every CI OS): the real,
    /// process-env-backed function returns SOMETHING ending in the fixed
    /// `edits.lbdata` leaf.
    #[test]
    fn default_store_dir_ends_in_edits_lbdata() {
        let dir = default_store_dir();
        assert_eq!(
            dir.file_name().and_then(|n| n.to_str()),
            Some("edits.lbdata")
        );
    }
}
