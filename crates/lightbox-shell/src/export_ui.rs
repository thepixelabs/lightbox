// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E15 core slice, the Export dialog (spec §5.10/§7 T28-T31, narrowed to a
//! single panel: format/quality/depth/resize/color-space/sharpen/naming
//! controls + an Export button, progress on the status bar). Same
//! structural weight and pattern as [`crate::prefs_ui::PrefsWindow`]: one
//! `egui::Window`, a `*PanelCtx` borrowed disjointly from `LightboxApp`
//! once per frame, commands submitted through [`Command::Export`] and
//! tracked by ticket so `lib.rs`'s `drain_events` can route completion back
//! in.
//!
//! **Selection.** Exports the filmstrip's multi-select
//! ([`crate::filmstrip::FilmstripState::selected`], already documented as
//! the batch-ops seam) when non-empty, else the single active/loupe image
//! "single image + basic batch" per the task brief, with no new selection
//! UI of its own.
//!
//! **Deferred** (named, not built, matches `lightbox-export`'s own crate
//! doc comment): watermark panel, metadata/privacy panel, presets,
//! multi-preset batch, Export-with-Previous, collision-policy dialog, JXL/
//! AVIF/DNG/Original. No direct SQL/engine types cross into this module
//! (headless-boundary lint, §2.3 seam 1), only `lightbox_core`/
//! `lightbox_export` types.

use eframe::egui;
use lightbox_core::{Command, CommandTicket, Session};
use lightbox_export::settings::{
    BitDepth, ExportSettings, FileFormat, NamingSpec, OutputColor, OutputColorSpace, OutputSharpen,
    SharpenAmount, SizeRule, Sizing,
};
use lightbox_types::ImageId;

use crate::theme::{paint, tokens as tk};

/// Everything the dialog touches, borrowed disjointly from `LightboxApp`
/// once per frame (mirrors `PrefsPanelCtx`).
pub struct ExportPanelCtx<'a> {
    /// The live session, `Command::Export` submits through here.
    pub session: &'a Session,
    /// The resolved export selection (filmstrip multi-select, or the
    /// single active image), computed by the caller, this module owns no
    /// selection state.
    pub images: &'a [ImageId],
    /// The status-bar message (mirrors every other dialog's convention).
    pub status: &'a mut String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FormatKind {
    Jpeg,
    Png,
    Tiff,
}

impl FormatKind {
    fn label(self) -> &'static str {
        match self {
            FormatKind::Jpeg => "JPEG",
            FormatKind::Png => "PNG",
            FormatKind::Tiff => "TIFF",
        }
    }
}

fn color_label(space: OutputColorSpace) -> &'static str {
    match space {
        OutputColorSpace::Srgb => "sRGB",
        OutputColorSpace::AdobeRgb => "Adobe RGB (compatible)",
        OutputColorSpace::DisplayP3 => "Display P3",
    }
}

fn sharpen_label(amount: SharpenAmount) -> &'static str {
    match amount {
        SharpenAmount::Low => "Low",
        SharpenAmount::Standard => "Standard",
        SharpenAmount::High => "High",
    }
}

/// The Export dialog (toggled by `app.export`, ⌘⇧E).
pub struct ExportDialog {
    open: bool,

    dest_dir: Option<std::path::PathBuf>,
    format: FormatKind,
    quality: u8,
    depth_16: bool,
    resize_on: bool,
    long_edge: u32,
    color: OutputColorSpace,
    sharpen_on: bool,
    sharpen_amount: SharpenAmount,
    suffix: String,

    // ── in-flight tracking (Command::Export progress → status bar) ──────
    ticket: Option<CommandTicket>,
    done: u32,
    total: u32,
    failed: u32,
    last_error: Option<String>,
}

impl Default for ExportDialog {
    fn default() -> ExportDialog {
        ExportDialog {
            open: false,
            dest_dir: None,
            format: FormatKind::Jpeg,
            quality: 90,
            depth_16: false,
            resize_on: false,
            long_edge: 2048,
            color: OutputColorSpace::Srgb,
            sharpen_on: false,
            sharpen_amount: SharpenAmount::Standard,
            suffix: String::new(),
            ticket: None,
            done: 0,
            total: 0,
            failed: 0,
            last_error: None,
        }
    }
}

impl ExportDialog {
    /// Starts closed.
    pub fn new() -> ExportDialog {
        ExportDialog::default()
    }

    /// The `app.export` action target.
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// Keep the frame pump hot while an export is running (progress arrives
    /// as events).
    pub fn busy(&self) -> bool {
        self.ticket.is_some()
    }

    /// `Event::ExportStarted` arrived for our ticket.
    pub fn on_export_started(&mut self, ticket: CommandTicket, total: u32) {
        if self.ticket == Some(ticket) {
            self.total = total;
            self.done = 0;
            self.failed = 0;
        }
    }

    /// `Event::ExportProgress` arrived for our ticket.
    pub fn on_export_progress(&mut self, ticket: CommandTicket, done: u32, total: u32, ok: bool) {
        if self.ticket == Some(ticket) {
            self.done = done;
            self.total = total;
            if !ok {
                self.failed += 1;
            }
        }
    }

    /// `Event::ExportFinished` arrived for our ticket.
    pub fn on_export_finished(&mut self, ticket: CommandTicket) {
        if self.ticket == Some(ticket) {
            self.ticket = None;
        }
    }

    /// `Event::CommandFailed` arrived; claims it when the ticket is ours.
    pub fn on_command_failed(&mut self, ticket: CommandTicket, error: &str) {
        if self.ticket == Some(ticket) {
            self.ticket = None;
            self.last_error = Some(error.to_owned());
        }
    }

    fn settings(&self) -> ExportSettings {
        let depth = if self.depth_16 {
            BitDepth::Sixteen
        } else {
            BitDepth::Eight
        };
        let format = match self.format {
            FormatKind::Jpeg => FileFormat::Jpeg {
                quality: self.quality,
            },
            FormatKind::Png => FileFormat::Png { depth },
            FormatKind::Tiff => FileFormat::Tiff { depth },
        };
        ExportSettings {
            format,
            color: OutputColor { space: self.color },
            sizing: Sizing {
                rule: if self.resize_on {
                    SizeRule::LongEdge {
                        px: self.long_edge.max(1),
                    }
                } else {
                    SizeRule::None
                },
            },
            sharpen: self.sharpen_on.then_some(OutputSharpen {
                amount: self.sharpen_amount,
            }),
            naming: NamingSpec {
                suffix: (!self.suffix.is_empty()).then(|| self.suffix.clone()),
            },
            // E15 metadata policy + watermark: no UI in this dialog yet, so
            // both take their defaults, which are "copyright only, no
            // watermark". That is the safe default by design, see
            // `lightbox_export::settings::MetadataLevel`.
            ..ExportSettings::default()
        }
    }

    /// Renders the window (no-op while closed).
    pub fn ui(&mut self, ctx: &egui::Context, p: &mut ExportPanelCtx<'_>) {
        if !self.open {
            return;
        }

        let mut open = self.open;
        // §11: modal-weight dialog (triggers real file writes), overlay
        // elevation, same tier as Preferences.
        let resp = egui::Window::new("Export")
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .open(&mut open)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(crate::floating_frame(
                tk::ELEV_4_OVERLAY,
                paint::shadow_overlay(),
            ))
            .show(ctx, |ui| {
                ui.label(format!("{} image(s) selected", p.images.len()));
                if p.images.is_empty() {
                    ui.colored_label(ui.visuals().warn_fg_color, "No image selected.");
                }

                ui.separator();
                egui::Grid::new("export_dest_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Destination");
                        ui.horizontal(|ui| {
                            match &self.dest_dir {
                                Some(dir) => {
                                    ui.monospace(dir.display().to_string());
                                }
                                None => {
                                    ui.weak("(choose a folder)");
                                }
                            }
                            if ui.button("Choose…").clicked() {
                                if let Some(dir) =
                                    rfd::FileDialog::new().set_title("Export To").pick_folder()
                                {
                                    self.dest_dir = Some(dir);
                                }
                            }
                        });
                        ui.end_row();

                        ui.label("Format");
                        egui::ComboBox::new("export_format", "")
                            .selected_text(self.format.label())
                            .show_ui(ui, |ui| {
                                for f in [FormatKind::Jpeg, FormatKind::Png, FormatKind::Tiff] {
                                    ui.selectable_value(&mut self.format, f, f.label());
                                }
                            });
                        ui.end_row();

                        if self.format == FormatKind::Jpeg {
                            ui.label("Quality");
                            ui.add(egui::Slider::new(&mut self.quality, 1..=100));
                            ui.end_row();
                        } else {
                            ui.label("Bit depth");
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut self.depth_16, false, "8-bit");
                                ui.selectable_value(&mut self.depth_16, true, "16-bit");
                            });
                            ui.end_row();
                        }

                        ui.label("Resize");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.resize_on, "Long edge");
                            ui.add_enabled(
                                self.resize_on,
                                egui::DragValue::new(&mut self.long_edge)
                                    .range(16..=20_000)
                                    .suffix(" px"),
                            );
                        });
                        ui.end_row();

                        ui.label("Color space");
                        egui::ComboBox::new("export_color", "")
                            .selected_text(color_label(self.color))
                            .show_ui(ui, |ui| {
                                for space in [
                                    OutputColorSpace::Srgb,
                                    OutputColorSpace::AdobeRgb,
                                    OutputColorSpace::DisplayP3,
                                ] {
                                    ui.selectable_value(&mut self.color, space, color_label(space));
                                }
                            });
                        ui.end_row();

                        ui.label("Sharpening");
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.sharpen_on, "");
                            ui.add_enabled_ui(self.sharpen_on, |ui| {
                                egui::ComboBox::new("export_sharpen", "")
                                    .selected_text(sharpen_label(self.sharpen_amount))
                                    .show_ui(ui, |ui| {
                                        for amount in [
                                            SharpenAmount::Low,
                                            SharpenAmount::Standard,
                                            SharpenAmount::High,
                                        ] {
                                            ui.selectable_value(
                                                &mut self.sharpen_amount,
                                                amount,
                                                sharpen_label(amount),
                                            );
                                        }
                                    });
                            });
                        });
                        ui.end_row();

                        ui.label("Filename suffix");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.suffix)
                                .hint_text("(none)")
                                .desired_width(160.0),
                        );
                        ui.end_row();
                    });

                ui.separator();

                if let Some(err) = &self.last_error {
                    ui.colored_label(ui.visuals().error_fg_color, format!("Export failed: {err}"));
                }

                ui.horizontal(|ui| {
                    let can_export =
                        self.ticket.is_none() && self.dest_dir.is_some() && !p.images.is_empty();
                    if self.ticket.is_some() {
                        ui.spinner();
                        ui.label(format!(
                            "Exporting {}/{} ({} failed)…",
                            self.done, self.total, self.failed
                        ));
                    } else if ui
                        .add_enabled(can_export, egui::Button::new("Export"))
                        .clicked()
                    {
                        if let Some(dest_dir) = self.dest_dir.clone() {
                            let settings = self.settings();
                            if let Err(e) = settings.validate() {
                                self.last_error = Some(e.to_string());
                            } else {
                                self.last_error = None;
                                let images = p.images.to_vec();
                                let n = images.len();
                                let ticket = p.session.submit(Command::Export {
                                    images,
                                    dest_dir,
                                    settings,
                                });
                                self.ticket = Some(ticket);
                                self.done = 0;
                                self.total = 0;
                                self.failed = 0;
                                *p.status = format!("exporting {n} image(s)…");
                            }
                        }
                    }
                });
            });
        crate::finish_floating_window(ctx, &resp);
        self.open = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_reflects_jpeg_defaults() {
        let dlg = ExportDialog::new();
        let s = dlg.settings();
        assert_eq!(s.format, FileFormat::Jpeg { quality: 90 });
        assert_eq!(s.sizing.rule, SizeRule::None);
        assert!(s.sharpen.is_none());
        assert!(s.naming.suffix.is_none());
        assert_eq!(s.color.space, OutputColorSpace::Srgb);
    }

    #[test]
    fn settings_reflects_resize_sharpen_and_suffix() {
        let mut dlg = ExportDialog::new();
        dlg.resize_on = true;
        dlg.long_edge = 1600;
        dlg.sharpen_on = true;
        dlg.sharpen_amount = SharpenAmount::High;
        dlg.suffix = "-Export".to_owned();
        dlg.format = FormatKind::Tiff;
        dlg.depth_16 = true;
        dlg.color = OutputColorSpace::DisplayP3;

        let s = dlg.settings();
        assert_eq!(
            s.format,
            FileFormat::Tiff {
                depth: BitDepth::Sixteen
            }
        );
        assert_eq!(s.sizing.rule, SizeRule::LongEdge { px: 1600 });
        assert_eq!(s.sharpen.map(|sh| sh.amount), Some(SharpenAmount::High));
        assert_eq!(s.naming.suffix.as_deref(), Some("-Export"));
        assert_eq!(s.color.space, OutputColorSpace::DisplayP3);
    }

    /// `CommandTicket` has no public constructor (by design, spec §3.8:
    /// callers only ever receive one from `Session::submit`), so this test
    /// mints two real, distinct tickets off a throwaway headless session to
    /// exercise the "ignores a foreign ticket" branch honestly.
    #[test]
    fn progress_and_completion_tracking_ignores_foreign_tickets() {
        use lightbox_core::{Core, CoreConfig};
        let tmp = tempfile::tempdir().unwrap();
        let core = Core::start(CoreConfig::default()).unwrap();
        let session = core
            .create_catalog(&tmp.path().join("t.lbdata"), None)
            .unwrap();
        let set_rating = || {
            session.submit(Command::SetRating {
                image: ImageId(1),
                rating: None,
            })
        };
        let ours = set_rating();
        let foreign = set_rating();

        let mut dlg = ExportDialog::new();
        dlg.ticket = Some(ours);
        dlg.on_export_started(foreign, 5);
        assert_eq!(dlg.total, 0, "a foreign ticket must not update state");
        dlg.on_export_started(ours, 5);
        assert_eq!(dlg.total, 5);
        dlg.on_export_progress(ours, 2, 5, true);
        assert_eq!(dlg.done, 2);
        assert_eq!(dlg.failed, 0);
        dlg.on_export_progress(ours, 3, 5, false);
        assert_eq!(dlg.failed, 1);
        assert!(dlg.busy());
        dlg.on_export_finished(ours);
        assert!(!dlg.busy());

        session
            .close(lightbox_core::CloseOpts::with_backup(
                lightbox_core::ClosePolicy::Skip,
            ))
            .unwrap();
    }
}
