// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E02 Phase H (H2): the headless decode → color → look reference path.
//!
//! `probe` / `decode` / `render-ref` / `profile {inspect,install,list}` /
//! `look inspect` drive the [`lightbox_decode`] + [`lightbox_color`] surfaces
//! end to end so CI's golden job (and a developer at a terminal) can exercise
//! the whole pipeline without the shell. Mosaic-raw `decode` / `render-ref` go
//! through the out-of-process LibRaw proxy ([`lightbox_decode::ProxySupervisor`]);
//! when the proxy binary was built without the `libraw` feature (the default,
//! license-clean build — E02 §0) they fail with a **structured** error, never a
//! panic and never an in-process fallback (R1). Non-raw files (JPEG/PNG/TIFF)
//! render fully in every build.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use lightbox_catalog::{BundledProfile, Catalog, ProfileKind, ProfileSource};
use lightbox_color::profile::CameraProfile;
use lightbox_color::{camera_matrix_base, resolve_input_transform, WbMode};
use lightbox_decode::{
    decode_for_develop, decode_image, normalize_camera, probe, DecodeOpts, ProbedFormat,
    ProxySupervisor, RawDecode, SourceColor, SourceImage,
};
use lightbox_jobs::CancelToken;

use crate::{parse_num, with_usage, Flags, UsageError};

/// Proxy decode budget (spec §7.5 default).
const DECODE_TIMEOUT: Duration = Duration::from_secs(30);

/// `--file <path>` is required by the file-oriented subcommands.
fn required_file(flags: &mut Flags) -> Result<PathBuf, UsageError> {
    flags
        .take_value("--file")?
        .map(PathBuf::from)
        .ok_or_else(|| UsageError("--file <path> is required".to_owned()))
}

/// Renders a `ProfileId` (or any 16-byte id) as lowercase hex.
fn hex16(bytes: &[u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The sRGB opto-electronic transfer function (linear → encoded), for the
/// non-raw reference path (§5.3: display-referred sources are not run through
/// the camera matrix or the default look).
fn srgb_oetf(l: f32) -> f32 {
    let l = l.clamp(0.0, 1.0);
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

fn to_u8(e: f32) -> u8 {
    (e * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

// ---------------------------------------------------------------------------
// probe
// ---------------------------------------------------------------------------

pub(crate) fn cmd_probe(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let file = required_file(&mut flags)?;
        let json = flags.take_switch("--json");
        flags.finish()?;

        let p = probe(&file).with_context(|| format!("probing {}", file.display()))?;
        let previews: Vec<_> = p
            .embedded
            .iter()
            .map(|e| {
                serde_json::json!({
                    "width": e.width,
                    "height": e.height,
                    "offset": e.byte_range.start,
                    "len": e.byte_range.end - e.byte_range.start,
                })
            })
            .collect();
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "path": file.display().to_string(),
                    "format": p.format.catalog_tag(),
                    "width": p.width,
                    "height": p.height,
                    "orientation": p.orientation.exif_value(),
                    "camera_make": p.camera_make,
                    "camera_model": p.camera_model,
                    "capture_time": p.capture_time,
                    "file_bytes": p.file_bytes,
                    "embedded_previews": previews,
                }))?
            );
        } else {
            println!("path        {}", file.display());
            println!("format      {}", p.format.catalog_tag());
            println!("dimensions  {}x{}", p.width, p.height);
            println!("orientation {}", p.orientation.exif_value());
            println!(
                "camera      {} {}",
                p.camera_make.as_deref().unwrap_or("-"),
                p.camera_model.as_deref().unwrap_or("-"),
            );
            println!("capture     {}", p.capture_time.as_deref().unwrap_or("-"));
            println!("bytes       {}", p.file_bytes);
            println!("previews    {}", p.embedded.len());
        }
        Ok(0)
    })
}

// ---------------------------------------------------------------------------
// decode
// ---------------------------------------------------------------------------

pub(crate) fn cmd_decode(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let file = required_file(&mut flags)?;
        let json = flags.take_switch("--json");
        flags.finish()?;

        let p = probe(&file).with_context(|| format!("probing {}", file.display()))?;
        let (backend, dims, extra) = decode_summary(&file, &p)?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "path": file.display().to_string(),
                    "format": p.format.catalog_tag(),
                    "backend": backend,
                    "width": dims.0,
                    "height": dims.1,
                    "detail": extra,
                }))?
            );
        } else {
            println!(
                "decoded {} via {backend} ({}x{}) {extra}",
                file.display(),
                dims.0,
                dims.1
            );
        }
        Ok(0)
    })
}

/// Decodes `file` and returns `(backend, (w,h), detail)`. Non-raw → the image
/// codecs; raw → the LibRaw proxy (structured error when unavailable).
fn decode_summary(
    file: &Path,
    p: &lightbox_decode::AssetProbe,
) -> anyhow::Result<(String, (u32, u32), String)> {
    match &p.format {
        ProbedFormat::Jpeg | ProbedFormat::Png | ProbedFormat::Tiff => {
            let src = decode_image(file).with_context(|| format!("decoding {}", file.display()))?;
            Ok((
                format!("{:?}", src.provenance.backend),
                (src.width, src.height),
                format!("{} channels, {}", src.channels, color_tag(&src.color)),
            ))
        }
        ProbedFormat::Raw(tag) => {
            let src = decode_raw_via_proxy(file)?;
            Ok((
                "libraw_proxy".to_owned(),
                (src.width, src.height),
                format!(
                    "{tag} interim_demosaic={} params_hash={:016x}",
                    src.provenance.interim_demosaic, src.provenance.decode_params_hash
                ),
            ))
        }
        ProbedFormat::Unsupported(hint) => {
            bail!("unsupported format ({hint}): nothing to decode")
        }
    }
}

fn color_tag(c: &SourceColor) -> &'static str {
    match c {
        SourceColor::CameraNative(_) => "camera-native",
        SourceColor::Tagged(_) => "icc-tagged",
        SourceColor::AssumedSrgb => "assumed-srgb",
    }
}

/// Mosaic/raw decode through the warm LibRaw proxy pool (interim AHD develop
/// path). A missing proxy binary or a proxy built without `--features libraw`
/// surfaces as a clear, actionable error — never a panic (R1).
fn decode_raw_via_proxy(file: &Path) -> anyhow::Result<SourceImage> {
    let sup = ProxySupervisor::autodetect().ok_or_else(|| {
        anyhow!(
            "LibRaw proxy binary not found — mosaic raw decode needs lightbox-rawproxy \
             built with `--features libraw` (E02 §0/§5.1). Set LIGHTBOX_RAWPROXY_BIN or \
             build the workspace so the sibling binary exists."
        )
    })?;
    let opts = DecodeOpts {
        backend: lightbox_decode::BackendPolicy::Auto,
        interim_demosaic: true,
        timeout: DECODE_TIMEOUT,
    };
    match decode_for_develop(&sup, file, &opts).with_context(|| {
        format!(
            "raw decode via the LibRaw proxy failed for {} (proxy built without libraw?)",
            file.display()
        )
    })? {
        RawDecode::DemosaicedInterim(src) | RawDecode::Linear(src) => Ok(src),
        RawDecode::Mosaic(_) => {
            bail!("proxy returned a mosaic buffer for an interim-demosaic request (bug)")
        }
    }
}

// ---------------------------------------------------------------------------
// render-ref
// ---------------------------------------------------------------------------

pub(crate) fn cmd_render_ref(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let file = required_file(&mut flags)?;
        let out = flags
            .take_value("--out")?
            .map(PathBuf::from)
            .ok_or_else(|| UsageError("render-ref requires --out <out.png>".to_owned()))?;
        let look_path = flags.take_value("--look")?.map(PathBuf::from);
        let amount = flags
            .take_value("--amount")?
            .map(|v| parse_num::<f32>("--amount", &v))
            .transpose()?
            .unwrap_or(1.0);
        flags.finish()?;

        let p = probe(&file).with_context(|| format!("probing {}", file.display()))?;
        let (w, h, rgba, path_desc) = match &p.format {
            ProbedFormat::Raw(_) => render_raw_reference(&file, &p, look_path.as_deref(), amount)?,
            ProbedFormat::Jpeg | ProbedFormat::Png | ProbedFormat::Tiff => {
                if look_path.is_some() {
                    // §5.3: non-raw sources already carry a rendered look.
                    eprintln!("note: --look is ignored for non-raw sources (§5.3)");
                }
                render_nonraw_reference(&file)?
            }
            ProbedFormat::Unsupported(hint) => {
                bail!("unsupported format ({hint}): nothing to render")
            }
        };

        lbx_image_compare::Rgba8Image::new(w, h, rgba)
            .map_err(|e| anyhow!("reference render malformed: {e}"))?
            .write_png(&out)
            .map_err(|e| anyhow!("writing {}: {e}", out.display()))?;
        println!("wrote {} ({w}x{h}, {path_desc})", out.display());
        Ok(0)
    })
}

/// Full raw reference: decode (proxy AHD) → camera-matrix base → resolved input
/// transform (WB as-shot ⊕ optional default look) → display-sRGB (spec §5.2).
fn render_raw_reference(
    file: &Path,
    p: &lightbox_decode::AssetProbe,
    look_path: Option<&Path>,
    amount: f32,
) -> anyhow::Result<(u32, u32, Vec<u8>, String)> {
    let src = decode_raw_via_proxy(file)?;
    let colorimetry = match &src.color {
        SourceColor::CameraNative(c) => c.clone(),
        other => bail!(
            "raw decode returned {} pixels, expected camera-native (bug)",
            color_tag(other)
        ),
    };
    let camera = normalize_camera(
        p.camera_make.as_deref().unwrap_or(""),
        p.camera_model.as_deref().unwrap_or(""),
    );
    let profile = camera_matrix_base(&colorimetry, &camera)
        .map_err(|e| anyhow!("building the camera-matrix base: {e}"))?;

    let look = match look_path {
        Some(lp) => {
            let bytes = std::fs::read(lp).with_context(|| format!("reading {}", lp.display()))?;
            Some(
                lightbox_color::look::load_look(&bytes)
                    .map_err(|e| anyhow!("{}: {e}", lp.display()))?,
            )
        }
        None => None,
    };
    let transform = resolve_input_transform(
        &profile,
        &WbMode::AsShot,
        colorimetry.as_shot_neutral,
        look.as_ref(),
        amount,
    )
    .map_err(|e| anyhow!("resolving the input transform: {e}"))?;

    let ch = src.channels as usize;
    let (w, h) = (src.width, src.height);
    let mut rgba = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for px in src.data.chunks_exact(ch.max(1)) {
        let cam = [
            px[0],
            px.get(1).copied().unwrap_or(px[0]),
            px.get(2).copied().unwrap_or(px[0]),
        ];
        let [r, g, b] = transform.render_reference_srgb8(cam);
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    let look_desc = look.as_ref().map_or("no look".to_owned(), |l| {
        format!("look={:?}@{amount}", l.name)
    });
    Ok((w, h, rgba, format!("camera-matrix base, {look_desc}")))
}

/// Non-raw reference (§5.3): the decoded display-referred linear pixels →
/// sRGB encode. The default look does not apply.
fn render_nonraw_reference(file: &Path) -> anyhow::Result<(u32, u32, Vec<u8>, String)> {
    let src = decode_image(file).with_context(|| format!("decoding {}", file.display()))?;
    let ch = src.channels as usize;
    let (w, h) = (src.width, src.height);
    let mut rgba = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for px in src.data.chunks_exact(ch.max(1)) {
        let r = to_u8(srgb_oetf(px[0]));
        let g = to_u8(srgb_oetf(px.get(1).copied().unwrap_or(px[0])));
        let b = to_u8(srgb_oetf(px.get(2).copied().unwrap_or(px[0])));
        rgba.extend_from_slice(&[r, g, b, 255]);
    }
    Ok((
        w,
        h,
        rgba,
        format!("non-raw sRGB ({})", color_tag(&src.color)),
    ))
}

// ---------------------------------------------------------------------------
// profile {inspect, install, list}
// ---------------------------------------------------------------------------

pub(crate) fn cmd_profile(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let Some(sub) = args.first() else {
            return Err(
                UsageError("profile needs a subcommand: inspect|install|list".to_owned()).into(),
            );
        };
        let rest = &args[1..];
        match sub.as_str() {
            "inspect" => profile_inspect(rest),
            "install" => profile_install(rest),
            "list" => profile_list(rest),
            other => Err(UsageError(format!("unknown profile subcommand {other:?}")).into()),
        }
    })
}

/// A parsed color asset (a `.dcp` camera profile or a `.lblook` look), unified
/// for `inspect` / `install`.
enum ParsedAsset {
    // CameraProfile carries the full calibration block and is much larger than a
    // Look; box it to keep the enum small (clippy::large_enum_variant).
    Dcp(Box<CameraProfile>),
    Look(lightbox_color::look::Look),
}

fn parse_asset(file: &Path) -> anyhow::Result<ParsedAsset> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "dcp" => lightbox_color::dcp::parse_dcp(&bytes)
            .map(|cp| ParsedAsset::Dcp(Box::new(cp)))
            .map_err(|e| anyhow!("{}: {e}", file.display())),
        "lblook" => lightbox_color::look::load_look(&bytes)
            .map(ParsedAsset::Look)
            .map_err(|e| anyhow!("{}: {e}", file.display())),
        _ => bail!(
            "unrecognized profile extension {ext:?} — expected .dcp or .lblook ({})",
            file.display()
        ),
    }
}

fn profile_inspect(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let file = required_file(&mut flags)?;
        let json = flags.take_switch("--json");
        flags.finish()?;

        match parse_asset(&file)? {
            ParsedAsset::Dcp(cp) => print_dcp(&file, &cp, json)?,
            ParsedAsset::Look(look) => print_look(&file, &look, json)?,
        }
        Ok(0)
    })
}

fn print_dcp(file: &Path, cp: &CameraProfile, json: bool) -> anyhow::Result<()> {
    let id = hex16(&cp.id.0);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": file.display().to_string(),
                "kind": "dcp",
                "id": id,
                "name": cp.name,
                "source": format!("{:?}", cp.source),
                "illuminant1": format!("{:?}", cp.calibration.illuminant1),
                "illuminant2": cp.calibration.illuminant2.map(|i| format!("{i:?}")),
                "has_hue_sat_map": cp.hue_sat_map.is_some(),
                "has_look_table": cp.look_table.is_some(),
                "has_tone_curve": cp.tone_curve.is_some(),
                "baseline_exposure_offset": cp.baseline_exposure_offset,
                "copyright": cp.copyright,
            }))?
        );
    } else {
        println!("kind        dcp");
        println!("id          {id}");
        println!("name        {}", cp.name);
        println!("source      {:?}", cp.source);
        println!("illuminant1 {:?}", cp.calibration.illuminant1);
        println!(
            "illuminant2 {}",
            cp.calibration
                .illuminant2
                .map_or("-".to_owned(), |i| format!("{i:?}"))
        );
        println!("hue_sat_map {}", cp.hue_sat_map.is_some());
        println!("look_table  {}", cp.look_table.is_some());
        println!("tone_curve  {}", cp.tone_curve.is_some());
        println!("baseline    {} stops", cp.baseline_exposure_offset);
        println!("copyright   {}", cp.copyright.as_deref().unwrap_or("-"));
    }
    Ok(())
}

fn print_look(file: &Path, look: &lightbox_color::look::Look, json: bool) -> anyhow::Result<()> {
    let id = hex16(&look.id.0);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": file.display().to_string(),
                "kind": "look",
                "id": id,
                "name": look.name,
                "version": look.version,
                "has_hue_sat": look.hue_sat.is_some(),
                "author": look.provenance.author,
                "license": look.provenance.license,
                "review_record": look.provenance.review_record,
            }))?
        );
    } else {
        println!("kind        look");
        println!("id          {id}");
        println!("name        {}", look.name);
        println!("version     {}", look.version);
        println!("hue_sat     {}", look.hue_sat.is_some());
        println!("author      {}", look.provenance.author);
        println!("license     {}", look.provenance.license);
        println!(
            "review      {}",
            look.provenance.review_record.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

fn profile_install(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = crate::required_catalog(&mut flags)?;
        let file = required_file(&mut flags)?;
        flags.finish()?;

        let record = build_install_record(&file)?;
        let cat = Catalog::open(&catalog)
            .with_context(|| format!("opening catalog {}", catalog.display()))?;
        let report = cat
            .sync_bundled_profiles(std::slice::from_ref(&record))
            .context("registering the profile")?;
        let verb = if report.inserted > 0 {
            "installed"
        } else if report.updated > 0 {
            "updated"
        } else {
            "already present"
        };
        println!(
            "{verb} {} {:?} (id {}) in {}",
            record.kind.as_str(),
            record.name,
            record.id,
            catalog.display()
        );
        Ok(0)
    })
}

/// Parses a profile file and derives its [`BundledProfile`] install record
/// (`source = user`).
fn build_install_record(file: &Path) -> anyhow::Result<BundledProfile> {
    let (id, name, kind) = match parse_asset(file)? {
        ParsedAsset::Dcp(cp) => (hex16(&cp.id.0), cp.name, ProfileKind::Dcp),
        ParsedAsset::Look(look) => (hex16(&look.id.0), look.name, ProfileKind::Look),
    };
    let file_hash = lightbox_decode::hash_file(file, &CancelToken::new())
        .map(|h| h.to_hex())
        .with_context(|| format!("hashing {}", file.display()))?;
    Ok(BundledProfile {
        id,
        kind,
        name,
        camera_make: None,
        camera_model: None,
        source: ProfileSource::User,
        license: "user".to_owned(),
        file_path: file.display().to_string(),
        file_hash,
    })
}

fn profile_list(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = crate::required_catalog(&mut flags)?;
        let json = flags.take_switch("--json");
        flags.finish()?;

        let cat = Catalog::open(&catalog)
            .with_context(|| format!("opening catalog {}", catalog.display()))?;
        let rows = cat.reader().camera_profiles().context("listing profiles")?;
        if json {
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            for r in &rows {
                println!(
                    "{:<5} {:<24} {:<20} {:<8} {}",
                    r.kind,
                    r.name,
                    format!(
                        "{} {}",
                        r.camera_make.as_deref().unwrap_or("-"),
                        r.camera_model.as_deref().unwrap_or("-")
                    ),
                    r.source,
                    &r.id
                );
            }
            eprintln!("{} profile(s)", rows.len());
        }
        Ok(0)
    })
}

// ---------------------------------------------------------------------------
// look inspect
// ---------------------------------------------------------------------------

pub(crate) fn cmd_look(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let Some(sub) = args.first() else {
            return Err(UsageError("look needs a subcommand: inspect".to_owned()).into());
        };
        let rest = &args[1..];
        match sub.as_str() {
            "inspect" => {
                let mut flags = Flags::new(rest);
                let file = required_file(&mut flags)?;
                let json = flags.take_switch("--json");
                flags.finish()?;
                let bytes =
                    std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
                let look = lightbox_color::look::load_look(&bytes)
                    .map_err(|e| anyhow!("{}: {e}", file.display()))?;
                print_look(&file, &look, json)?;
                Ok(0)
            }
            other => Err(UsageError(format!("unknown look subcommand {other:?}")).into()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex16_is_lowercase_32_chars() {
        let s = hex16(&[0x00, 0x0a, 0xff, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]);
        assert_eq!(s.len(), 32);
        assert!(s.starts_with("000aff01"));
    }

    #[test]
    fn srgb_oetf_endpoints() {
        assert_eq!(to_u8(srgb_oetf(0.0)), 0);
        assert_eq!(to_u8(srgb_oetf(1.0)), 255);
        // 18% grey ≈ 0.461 encoded → ~118.
        let mid = to_u8(srgb_oetf(0.18));
        assert!((116..=120).contains(&mid), "got {mid}");
    }
}
