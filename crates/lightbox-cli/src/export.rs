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
//!     [--sharpen low|standard|high] [--cpu]
//!
//! lightbox-cli export --catalog <dir> --images <id>[,<id>...] --out-dir <dir> \
//!     --format jpeg|png|tiff [--quality 1..=100] [--depth 8|16] \
//!     [--long-edge N] [--color srgb|adobe-rgb|display-p3] \
//!     [--sharpen low|standard|high] [--suffix <text>] [--cpu]
//! ```
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
    BitDepth, ExportSettings, FileFormat, NamingSpec, OutputColor, OutputColorSpace, OutputSharpen,
    SharpenAmount, SizeRule, Sizing,
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

/// Shared flag parsing for both export forms; returns the fully-built
/// settings plus the flags neither form has consumed yet.
struct CommonFlags {
    settings: ExportSettings,
    cpu: bool,
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
    let item = ExportItem {
        image,
        pv: recipe.pv,
        recipe,
        full_extent: extent_of(&detail),
        out_path: out.to_path_buf(),
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
