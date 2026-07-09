// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The folder-explorer UI (mandate v2.2, E08 spec §2.1 ★): a transient,
//! live-filesystem Finder/Explorer-style surface reachable from the
//! empty-state (a "Browse Folders…" affordance). It drives
//! [`lightbox_core::browse_dir`] — E04's headless folder-explorer primitive
//! — to navigate into folders and nested subfolders, lists directly-
//! contained images as pickable rows, and opens a chosen image or the whole
//! current folder via `Command::OpenWorkingSet`.
//!
//! **What this is not:** a library. Every navigation re-reads the live
//! filesystem (`browse_dir` persists nothing — no catalog rows, no
//! `folder` table writes); this module holds only transient UI state
//! (current directory + its listing + a back/up history stack), never
//! anything durable. Closing the explorer (or the app) discards all of it.
//!
//! **Phase-A scope note:** contained images are listed as filename **rows**,
//! not real pixel thumbnails — the spec's §2.1 wording explicitly offers
//! "thumbnails/rows" as alternatives. Generating real thumbnails would need
//! an `ImageId` (a catalog concept: the file has to be opened/registered
//! first — exactly what this surface is for), so it is out of reach without
//! either a working-set loader dependency (an explicit non-goal, §2.3: "no
//! lightbox-ingest dependency from the shell") or ad hoc decoding outside
//! the established preview seam. Rows keep this simple and honestly scoped,
//! per the phase brief ("keep it simple + tested; polish can follow").

use std::path::PathBuf;

use eframe::egui;
use lightbox_core::{browse_dir, DirListing};

/// What the explorer asks the app to do.
pub enum ExplorerAction {
    /// Open one picked image (adds it as a single-file working set).
    OpenImage(PathBuf),
    /// Open every image directly inside the current folder (non-recursive —
    /// matches what's listed; "descend into a subfolder" is a separate
    /// navigation gesture, not folded into this).
    OpenFolder(PathBuf),
}

/// Transient folder-explorer state (mandate v2.2). Not `Default`-derived on
/// purpose — [`FolderExplorer::closed`] is the explicit "nothing open yet"
/// constructor, mirroring the rest of this crate's state types.
pub struct FolderExplorer {
    open: bool,
    current: PathBuf,
    listing: DirListing,
    error: Option<String>,
    /// Ancestor directories visited this session, for the "Up" affordance —
    /// transient navigation history, never persisted (mandate v2.2: "live
    /// and ephemerally").
    history: Vec<PathBuf>,
}

impl FolderExplorer {
    /// Not shown; no live-filesystem read has happened yet.
    pub fn closed() -> FolderExplorer {
        FolderExplorer {
            open: false,
            current: PathBuf::new(),
            listing: DirListing::default(),
            error: None,
            history: Vec::new(),
        }
    }

    /// `true` while the explorer window should render.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the explorer at `start_dir` (a fresh live read; clears any
    /// prior navigation history).
    pub fn open_at(&mut self, start_dir: PathBuf) {
        self.history.clear();
        self.navigate_to(start_dir);
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Re-reads `dir` live via `browse_dir` and makes it current. A read
    /// failure (permission denied, the folder vanished) surfaces as a
    /// non-fatal in-window error — the explorer stays open on the
    /// last-good listing rather than closing on the user.
    fn navigate_to(&mut self, dir: PathBuf) {
        match browse_dir(&dir) {
            Ok(listing) => {
                self.current = dir;
                self.listing = listing;
                self.error = None;
            }
            Err(err) => {
                self.error = Some(format!("{}: {err}", dir.display()));
            }
        }
    }

    /// Descend into a clicked subfolder (Finder-style).
    fn descend(&mut self, subdir: PathBuf) {
        self.history.push(self.current.clone());
        self.navigate_to(subdir);
    }

    /// Ascend to the parent directory, or pop the back-history — whichever
    /// is available (Up and Back collapse to the same affordance at this
    /// simplicity level; both just re-read a shallower/previous directory).
    fn ascend(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.navigate_to(prev);
        } else if let Some(parent) = self.current.parent() {
            self.navigate_to(parent.to_path_buf());
        }
    }

    /// Renders the explorer window (transient — not embedded in the main
    /// layout). Returns an action the app should act on.
    pub fn ui(&mut self, ctx: &egui::Context) -> Option<ExplorerAction> {
        if !self.open {
            return None;
        }
        let mut action = None;
        let mut still_open = true;

        egui::Window::new("Browse Folders")
            .open(&mut still_open)
            .default_size([420.0, 480.0])
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let can_ascend = !self.history.is_empty() || self.current.parent().is_some();
                    if ui
                        .add_enabled(can_ascend, egui::Button::new("⬆ Up"))
                        .clicked()
                    {
                        self.ascend();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new(self.current.display().to_string()).monospace());
                });
                ui.separator();

                if let Some(err) = &self.error {
                    ui.colored_label(egui::Color32::from_rgb(200, 80, 70), err);
                    ui.separator();
                }

                let open_folder_clicked = ui
                    .add_enabled(
                        !self.listing.images.is_empty(),
                        egui::Button::new(format!(
                            "Open this folder ({} image{})",
                            self.listing.images.len(),
                            if self.listing.images.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        )),
                    )
                    .clicked();
                if open_folder_clicked {
                    action = Some(ExplorerAction::OpenFolder(self.current.clone()));
                }
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut descend_to: Option<PathBuf> = None;
                    for dir in &self.listing.subdirs {
                        let name = dir
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| dir.display().to_string());
                        if ui.button(format!("📁 {name}")).clicked() {
                            descend_to = Some(dir.clone());
                        }
                    }
                    if let Some(dir) = descend_to {
                        self.descend(dir);
                    }

                    if !self.listing.subdirs.is_empty() && !self.listing.images.is_empty() {
                        ui.separator();
                    }

                    for image in &self.listing.images {
                        if ui
                            .selectable_label(false, format!("🖼 {}", image.filename))
                            .clicked()
                        {
                            action = Some(ExplorerAction::OpenImage(image.path.clone()));
                        }
                    }

                    if self.listing.subdirs.is_empty() && self.listing.images.is_empty() {
                        ui.weak("empty folder");
                    }
                });
            });

        if !still_open {
            self.close();
        }
        if action.is_some() {
            self.close();
        }
        action
    }
}

/// Best-effort starting directory for a fresh explorer session: the
/// platform home directory, falling back to the process's current
/// directory. No `directories` crate dependency (Phase A doesn't need one —
/// see `E08-deviations.md`; that crate lands with the Phase G prefs store).
pub fn default_start_dir() -> PathBuf {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if home.is_dir() {
            return home;
        }
    }
    #[cfg(target_os = "windows")]
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let profile = PathBuf::from(profile);
        if profile.is_dir() {
            return profile;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn open_at_reads_the_live_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("trip")).unwrap();
        touch(tmp.path(), "a.jpg");

        let mut explorer = FolderExplorer::closed();
        assert!(!explorer.is_open());
        explorer.open_at(tmp.path().to_path_buf());
        assert!(explorer.is_open());
        assert_eq!(explorer.listing.subdirs, vec![tmp.path().join("trip")]);
        assert_eq!(explorer.listing.images.len(), 1);
        assert!(explorer.error.is_none());
    }

    #[test]
    fn descend_then_ascend_round_trips_the_listing() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("trip")).unwrap();
        touch(&tmp.path().join("trip"), "img.nef");

        let mut explorer = FolderExplorer::closed();
        explorer.open_at(tmp.path().to_path_buf());
        explorer.descend(tmp.path().join("trip"));
        assert_eq!(explorer.current, tmp.path().join("trip"));
        assert_eq!(explorer.listing.images.len(), 1);
        assert!(explorer.listing.subdirs.is_empty());

        explorer.ascend();
        assert_eq!(explorer.current, tmp.path());
        assert_eq!(explorer.listing.subdirs, vec![tmp.path().join("trip")]);
    }

    #[test]
    fn navigating_into_a_missing_directory_surfaces_an_error_without_closing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut explorer = FolderExplorer::closed();
        explorer.open_at(tmp.path().to_path_buf());
        explorer.descend(tmp.path().join("does-not-exist"));
        assert!(explorer.is_open(), "stays open on a read failure");
        assert!(explorer.error.is_some());
        // The last-good listing is still whatever `tmp.path()` had.
        assert!(explorer.listing.subdirs.is_empty());
    }

    #[test]
    fn every_open_re_reads_the_filesystem_nothing_cached() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut explorer = FolderExplorer::closed();
        explorer.open_at(tmp.path().to_path_buf());
        assert!(explorer.listing.images.is_empty());

        touch(tmp.path(), "new.jpg");
        explorer.open_at(tmp.path().to_path_buf());
        assert_eq!(
            explorer.listing.images.len(),
            1,
            "live re-read sees the new file"
        );
    }

    #[test]
    fn default_start_dir_is_an_existing_directory() {
        let dir = default_start_dir();
        assert!(dir.is_dir(), "{} should exist", dir.display());
    }
}
