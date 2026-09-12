// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! E15 core slice (spec §8 `lightbox-cli export`, narrowed): the headless
//! proof of `import → edit → export`. Two forms:
//!
//! ```text
//! lightbox-cli export --catalog <dir> --image <id> --out <file> \
//!     --format jpeg|png|tiff [--quality 1..=100] [--depth 8|16] \
//!     [--long-edge N] [--color srgb|adobe-rgb|display-p3] \
//!     [--sharpen low|standard|high] [--metadata <level>] \
//!     [--copyright <text>] [--creator <name>] [--contact-email <addr>] \
//!     [--contact-url <url>] [--watermark <text>] \
//!     [--watermark-position <anchor>] [--watermark-opacity 0..=1] \
//!     [--watermark-size 0..=1] [--watermark-inset 0..0.5] \
//!     [--watermark-color rrggbb] [--no-watermark-halo] [--cpu]
//!
//! lightbox-cli export --catalog <dir> --images <id>[,<id>...] --out-dir <dir> \
//!     --format jpeg|png|tiff [... the same optional flags ...] \
//!     [--suffix <text>] [--cpu]
//! ```
//!
//! **`--metadata` defaults to `copyright`**, the safe end of
//! [`lightbox_export::settings::MetadataLevel`], not the maximal one: a
//! scripted export usually sends a file somewhere, and the level that
//! carries GPS out of the machine is the one you opt into. `--metadata all`
//! is the only level that writes a location.
//!
//! The single-image form calls [`lightbox_export::run::export_one`]
//! directly against `Session::engine()`, the exact "bypass the command
//! bus for a synchronous one-shot" convention `lightbox-cli render` already
//! uses, and it is the only way to honor an exact `--out <file>` path (the
//! batch façade names files itself, spec §5.1 `NamingSpec`). The batch form
//! drives `Command::Export`/`Event::Export*` end-to-end, the headless
//! proof of the core façade (spec §5.10) this task names as its own
//! deliverable.
//!
//! Exit codes: `0` every item exported; `1` at least one item failed
//! (or a bad flag combination); `2` usage error.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use lightbox_core::{CloseOpts, ClosePolicy, Command, Core, CoreConfig, Event, ImageDetail};
use lightbox_export::settings::{
    BitDepth, ExportSettings, FileFormat, MetadataLevel, MetadataPolicy, NamingSpec, OutputColor,
    OutputColorSpace, OutputSharpen, RightsInfo, SharpenAmount, SizeRule, Sizing, Watermark,
    WatermarkAnchor,
};
use lightbox_export::ExportItem;
use lightbox_jobs::CancelToken;
use lightbox_render::ng::Extent;
use lightbox_render::GpuContext;
use lightbox_types::ImageId;

use crate::{parse_num, required_catalog, with_usage, Flags, UsageError};

fn parse_format(name: &str, quality: u8, depth: BitDepth) -> Result<FileFormat, UsageError> {
    match name.to_ascii_lowercase().as_str() {
        "jpeg" | "jpg" => Ok(FileFormat::Jpeg { quality }),
        "png" => Ok(FileFormat::Png { depth }),
        "tiff" | "tif" => Ok(FileFormat::Tiff { depth }),
        other => Err(UsageError(format!(
            "--format must be jpeg|png|tiff, got {other:?}"
        ))),
    }
}

fn parse_depth(v: &str) -> Result<BitDepth, UsageError> {
    match v {
        "8" => Ok(BitDepth::Eight),
        "16" => Ok(BitDepth::Sixteen),
        other => Err(UsageError(format!(
            "--depth must be 8 or 16, got {other:?}"
        ))),
    }
}

fn parse_color(v: &str) -> Result<OutputColorSpace, UsageError> {
    match v.to_ascii_lowercase().replace(['_', ' '], "-").as_str() {
        "srgb" => Ok(OutputColorSpace::Srgb),
        "adobe-rgb" | "adobergb" => Ok(OutputColorSpace::AdobeRgb),
        "display-p3" | "displayp3" | "p3" => Ok(OutputColorSpace::DisplayP3),
        other => Err(UsageError(format!(
            "--color must be srgb|adobe-rgb|display-p3, got {other:?}"
        ))),
    }
}

fn parse_sharpen(v: &str) -> Result<SharpenAmount, UsageError> {
    match v.to_ascii_lowercase().as_str() {
        "low" => Ok(SharpenAmount::Low),
        "standard" => Ok(SharpenAmount::Standard),
        "high" => Ok(SharpenAmount::High),
        other => Err(UsageError(format!(
            "--sharpen must be low|standard|high, got {other:?}"
        ))),
    }
}

fn parse_metadata_level(v: &str) -> Result<MetadataLevel, UsageError> {
    match v.to_ascii_lowercase().replace(['_', ' '], "-").as_str() {
        "copyright" | "copyright-only" => Ok(MetadataLevel::CopyrightOnly),
        "copyright-contact" | "contact" => Ok(MetadataLevel::CopyrightAndContact),
        "all-except-camera-location" | "no-camera-location" => {
            Ok(MetadataLevel::AllExceptCameraAndLocation)
        }
        "all" => Ok(MetadataLevel::All),
        other => Err(UsageError(format!(
            "--metadata must be copyright|copyright-contact|all-except-camera-location|all, \
             got {other:?}"
        ))),
    }
}

fn parse_anchor(v: &str) -> Result<WatermarkAnchor, UsageError> {
    match v.to_ascii_lowercase().replace(['_', ' '], "-").as_str() {
        "top-left" => Ok(WatermarkAnchor::TopLeft),
        "top-center" | "top-centre" | "top" => Ok(WatermarkAnchor::TopCenter),
        "top-right" => Ok(WatermarkAnchor::TopRight),
        "middle-left" | "left" => Ok(WatermarkAnchor::MiddleLeft),
        "center" | "centre" | "middle" => Ok(WatermarkAnchor::Center),
        "middle-right" | "right" => Ok(WatermarkAnchor::MiddleRight),
        "bottom-left" => Ok(WatermarkAnchor::BottomLeft),
        "bottom-center" | "bottom-centre" | "bottom" => Ok(WatermarkAnchor::BottomCenter),
        "bottom-right" => Ok(WatermarkAnchor::BottomRight),
        other => Err(UsageError(format!(
            "--watermark-position must be one of the nine anchors \
             (top|middle|bottom)-(left|center|right), got {other:?}"
        ))),
    }
}

/// `rrggbb`, with or without a leading `#`.
fn parse_color_hex(v: &str) -> Result<[u8; 3], UsageError> {
    let hex = v.trim_start_matches('#');
    let bad = || UsageError(format!("--watermark-color must be rrggbb hex, got {v:?}"));
    if hex.len() != 6 {
        return Err(bad());
    }
    let byte = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).map_err(|_| bad());
    Ok([byte(0)?, byte(2)?, byte(4)?])
}

/// Shared flag parsing for both export forms; returns the fully-built
/// settings plus the flags neither form has consumed yet.
struct CommonFlags {
    settings: ExportSettings,
    cpu: bool,
}

fn parse_metadata(flags: &mut Flags) -> anyhow::Result<MetadataPolicy> {
    let level = flags
        .take_value("--metadata")?
        .map(|v| parse_metadata_level(&v))
        .transpose()?
        .unwrap_or_default();
    Ok(MetadataPolicy {
        level,
        rights: RightsInfo {
            creator: flags.take_value("--creator")?,
            copyright: flags.take_value("--copyright")?,
            contact_email: flags.take_value("--contact-email")?,
            contact_url: flags.take_value("--contact-url")?,
        },
    })
}

fn parse_watermark(flags: &mut Flags) -> anyhow::Result<Option<Watermark>> {
    let text = flags.take_value("--watermark")?;
    let anchor = flags
        .take_value("--watermark-position")?
        .map(|v| parse_anchor(&v))
        .transpose()?;
    let opacity = flags
        .take_value("--watermark-opacity")?
        .map(|v| parse_num::<f32>("--watermark-opacity", &v))
        .transpose()?;
    let size = flags
        .take_value("--watermark-size")?
        .map(|v| parse_num::<f32>("--watermark-size", &v))
        .transpose()?;
    let inset = flags
        .take_value("--watermark-inset")?
        .map(|v| parse_num::<f32>("--watermark-inset", &v))
        .transpose()?;
    let color = flags
        .take_value("--watermark-color")?
        .map(|v| parse_color_hex(&v))
        .transpose()?;
    let no_halo = flags.take_switch("--no-watermark-halo");

    let Some(text) = text else {
        // Tuning flags without a `--watermark` is a typo, not an intent to
        // export unmarked; say so rather than silently ignoring them.
        if anchor.is_some()
            || opacity.is_some()
            || size.is_some()
            || inset.is_some()
            || color.is_some()
            || no_halo
        {
            return Err(
                UsageError("--watermark-* flags need --watermark <text>".to_owned()).into(),
            );
        }
        return Ok(None);
    };
    let base = Watermark::default();
    Ok(Some(Watermark {
        text,
        anchor: anchor.unwrap_or(base.anchor),
        opacity: opacity.unwrap_or(base.opacity),
        size: size.unwrap_or(base.size),
        inset: inset.unwrap_or(base.inset),
        color: color.unwrap_or(base.color),
        halo: !no_halo,
    }))
}

fn parse_common(flags: &mut Flags) -> anyhow::Result<CommonFlags> {
    let format_name = flags
        .take_value("--format")?
        .ok_or_else(|| UsageError("export requires --format jpeg|png|tiff".to_owned()))?;
    let quality = flags
        .take_value("--quality")?
        .map(|v| parse_num::<u8>("--quality", &v))
        .transpose()?
        .unwrap_or(90);
    let depth = flags
        .take_value("--depth")?
        .map(|v| parse_depth(&v))
        .transpose()?
        .unwrap_or_default();
    let long_edge = flags
        .take_value("--long-edge")?
        .map(|v| parse_num::<u32>("--long-edge", &v))
        .transpose()?;
    let color = flags
        .take_value("--color")?
        .map(|v| parse_color(&v))
        .transpose()?
        .unwrap_or_default();
    let sharpen = flags
        .take_value("--sharpen")?
        .map(|v| parse_sharpen(&v))
        .transpose()?;
    let suffix = flags.take_value("--suffix")?;
    let metadata = parse_metadata(flags)?;
    let watermark = parse_watermark(flags)?;
    let cpu = flags.take_switch("--cpu");

    let format = parse_format(&format_name, quality, depth)?;
    let settings = ExportSettings {
        format,
        color: OutputColor { space: color },
        sizing: Sizing {
            rule: match long_edge {
                Some(px) => SizeRule::LongEdge { px },
                None => SizeRule::None,
            },
        },
        sharpen: sharpen.map(|amount| OutputSharpen { amount }),
        naming: NamingSpec { suffix },
        metadata,
        watermark,
    };
    settings
        .validate()
        .map_err(|e| anyhow!("invalid export settings: {e}"))?;
    Ok(CommonFlags { settings, cpu })
}

pub(crate) fn cmd_export(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = flags
            .take_value("--image")?
            .map(|v| parse_num::<i64>("--image", &v).map(ImageId))
            .transpose()?;
        let images = flags.take_value("--images")?;
        let out = flags.take_value("--out")?.map(PathBuf::from);
        let out_dir = flags.take_value("--out-dir")?.map(PathBuf::from);
        let common = parse_common(&mut flags)?;
        flags.finish()?;

        match (image, images, out, out_dir) {
            (Some(image), None, Some(out), None) => cmd_export_one(&catalog, image, &out, common),
            (None, Some(ids), None, Some(dir)) => {
                let images = ids
                    .split(',')
                    .map(|s| parse_num::<i64>("--images", s.trim()).map(ImageId))
                    .collect::<Result<Vec<_>, _>>()?;
                if images.is_empty() {
                    return Err(UsageError("--images requires at least one id".to_owned()).into());
                }
                cmd_export_batch(&catalog, images, &dir, common)
            }
            _ => Err(UsageError(
                "export requires either --image <id> --out <file>, or --images <id,id,...> \
                 --out-dir <dir>"
                    .to_owned(),
            )
            .into()),
        }
    })
}

/// The single-image form: renders straight against `Session::engine()`,
/// bypassing the command bus (mirrors `lightbox-cli render`, see the
/// module doc comment for why this is the only way to honor an exact
/// `--out <file>` path).
fn cmd_export_one(
    catalog: &std::path::Path,
    image: ImageId,
    out: &std::path::Path,
    common: CommonFlags,
) -> anyhow::Result<u8> {
    let gpu = if common.cpu {
        None
    } else {
        GpuContext::headless()
    };
    let core = Core::start(CoreConfig::default())?;
    let session = core.open_catalog(catalog, gpu)?;

    let detail = session
        .query()
        .image_detail(image)
        .with_context(|| format!("image {} not found in the catalog", image.0))?;
    let recipe = session.query().edit_state(image)?.recipe;
    // The per-image metadata comes from the original file, the same way the
    // batch path resolves it (`lightbox_core`'s `resolve_export_items`).
    // A source we cannot locate or cannot parse exports with no per-image
    // metadata rather than failing; the rights block still goes out.
    let source_metadata = session
        .query()
        .asset_abs_path(detail.asset)
        .ok()
        .map(|path| lightbox_export::metadata::read_source(&path));
    let item = ExportItem {
        image,
        pv: recipe.pv,
        recipe,
        full_extent: extent_of(&detail),
        out_path: out.to_path_buf(),
        source_metadata,
    };

    let engine = session.engine();
    let cancel = CancelToken::new();
    let result = lightbox_export::run::export_one(&engine, &item, &common.settings, &cancel);

    let code = match &result {
        Ok(()) => {
            println!("wrote {}", out.display());
            0
        }
        Err(err) => {
            eprintln!("error: export failed: {err}");
            1
        }
    };
    session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
    Ok(code)
}

/// The batch form: drives `Command::Export`/`Event::Export*` end-to-end
/// the headless proof of the core façade (spec §5.10, this task's own
/// deliverable 4).
fn cmd_export_batch(
    catalog: &std::path::Path,
    images: Vec<ImageId>,
    dest_dir: &std::path::Path,
    common: CommonFlags,
) -> anyhow::Result<u8> {
    let gpu = if common.cpu {
        None
    } else {
        GpuContext::headless()
    };
    let core = Core::start(CoreConfig::default())?;
    let session = core.open_catalog(catalog, gpu)?;

    let mut rx = session.events();
    let ticket = session.submit(Command::Export {
        images,
        dest_dir: dest_dir.to_path_buf(),
        settings: common.settings,
    });

    let deadline = Instant::now() + Duration::from_secs(600);
    let report = loop {
        match rx.try_recv() {
            Ok(Event::ExportProgress {
                ticket: t,
                image,
                out_path,
                error,
                done,
                total,
            }) if t == ticket => match &error {
                None => eprintln!(
                    "[{done}/{total}] ok    image {} -> {}",
                    image.0,
                    out_path.display()
                ),
                Some(e) => eprintln!("[{done}/{total}] FAILED image {}: {e}", image.0),
            },
            Ok(Event::ExportFinished { ticket: t, report }) if t == ticket => break report,
            Ok(Event::CommandFailed {
                ticket: failed,
                error,
            }) if failed == ticket => bail!("export failed: {error}"),
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                if Instant::now() > deadline {
                    bail!("export timed out");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                bail!("event stream closed before the export finished")
            }
        }
    };

    for (image, err) in &report.failed {
        eprintln!("error: image {}: {err}", image.0);
    }
    println!(
        "exported {} of {} ({} failed)",
        report.ok,
        report.total(),
        report.failed.len()
    );
    session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
    Ok(u8::from(!report.failed.is_empty()))
}

fn extent_of(detail: &ImageDetail) -> Extent {
    Extent {
        w: detail.width.max(1),
        h: detail.height.max(1),
    }
}
