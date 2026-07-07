// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E09 Phase B follow-up (T11): `edit`/`history`/`step-to`/`undo`/`redo`/
//! `clear-history`/`snapshot`/`preset`/`xmp` subcommands over the
//! `Command::Edit`/`EditHub` seam — the headless proof of the §3.1.1
//! promise (spec §9 DoD 2): import → edit → history → snapshot, all through
//! `lightbox-core` alone, exit codes `0`/`1`/`2` kept.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use lightbox_core::{
    CloseOpts, ClosePolicy, Command, Core, CoreConfig, EditCommand, Event, ParamDelta, ParamGroup,
    ParamId, ParamSubset, ParamValue, PresetId, Session, StepLabel,
};
use lightbox_types::{ImageId, SnapshotId};

use crate::{parse_num, required_catalog, with_usage, Flags, UsageError, EXIT_USAGE};

fn open_session(catalog: &std::path::Path) -> anyhow::Result<(Core, Session)> {
    let core = Core::start(CoreConfig::default())?;
    let session = core.open_catalog(catalog, None)?;
    Ok((core, session))
}

fn required_image(flags: &mut Flags) -> Result<ImageId, UsageError> {
    flags
        .take_value("--image")?
        .map(|v| parse_num::<i64>("--image", &v).map(ImageId))
        .transpose()?
        .ok_or_else(|| UsageError("--image <id> is required".to_owned()))
}

fn required_snapshot(flags: &mut Flags) -> Result<SnapshotId, UsageError> {
    flags
        .take_value("--id")?
        .map(|v| parse_num::<i64>("--id", &v).map(SnapshotId))
        .transpose()?
        .ok_or_else(|| UsageError("--id <snapshot-id> is required".to_owned()))
}

fn required_preset_id(flags: &mut Flags) -> Result<PresetId, UsageError> {
    flags
        .take_value("--id")?
        .map(PresetId)
        .ok_or_else(|| UsageError("--id <preset-id> is required".to_owned()))
}

fn required_name(flags: &mut Flags) -> Result<String, UsageError> {
    flags
        .take_value("--name")?
        .ok_or_else(|| UsageError("--name <name> is required".to_owned()))
}

fn bad_subcommand(name: &str, want: &str) -> anyhow::Result<u8> {
    eprintln!("error: {name} requires a subcommand: {want}");
    Ok(EXIT_USAGE)
}

/// Submits one durable `EditCommand` and waits for its completion —
/// `Event::CatalogChanged` (the dispatcher's generic ack, emitted for every
/// successful dispatch, no-ops included) or `Event::CommandFailed`
/// correlated by ticket. Returns every event observed along the way (so
/// callers can pull out e.g. the `EditCommitted.seq`).
fn run_edit_command(session: &Session, cmd: EditCommand) -> anyhow::Result<Vec<Event>> {
    let mut rx = session.events();
    let ticket = session.submit(Command::Edit(cmd));
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut seen = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(Event::CommandFailed {
                ticket: failed,
                error,
            }) if failed == ticket => bail!("edit command failed: {error}"),
            Ok(ev @ Event::CatalogChanged { .. }) => {
                seen.push(ev);
                return Ok(seen);
            }
            Ok(ev) => seen.push(ev),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                if Instant::now() > deadline {
                    bail!("edit command timed out");
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                bail!("event stream closed before the edit command finished")
            }
        }
    }
}

fn close_quiet(session: Session) -> anyhow::Result<()> {
    session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// `edit set` / `edit get`
// ---------------------------------------------------------------------------

pub(crate) fn cmd_edit(args: &[String]) -> anyhow::Result<u8> {
    match args.first().map(String::as_str) {
        Some("set") => cmd_edit_set(&args[1..]),
        Some("get") => cmd_edit_get(&args[1..]),
        _ => bad_subcommand("edit", "set|get"),
    }
}

/// The scalar (F32) params `edit set`/`edit get` support — the "basic
/// panel" tone + presence groups the M1 exit line names ("basic-panel edits
/// auto-persist"). Geometry/curve/HSL/etc. are structured types outside a
/// hand-rolled `key=value` CLI's scope; they're reachable through
/// `preset apply`/`snapshot restore` instead.
const SCALAR_PARAMS: &[(&str, ParamId)] = &[
    ("exposure", ParamId::Exposure),
    ("contrast", ParamId::Contrast),
    ("highlights", ParamId::Highlights),
    ("shadows", ParamId::Shadows),
    ("whites", ParamId::Whites),
    ("blacks", ParamId::Blacks),
    ("vibrance", ParamId::Vibrance),
    ("saturation", ParamId::Saturation),
    ("clarity", ParamId::Clarity),
    ("texture", ParamId::Texture),
    ("dehaze", ParamId::Dehaze),
    ("angle", ParamId::Angle),
];

fn parse_param_kv(s: &str) -> Result<(ParamId, ParamValue), UsageError> {
    let (key, val) = s.split_once('=').ok_or_else(|| {
        UsageError(format!(
            "malformed edit set argument {s:?} (want key=value)"
        ))
    })?;
    let id = SCALAR_PARAMS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, id)| *id)
        .ok_or_else(|| {
            let names: Vec<&str> = SCALAR_PARAMS.iter().map(|(n, _)| *n).collect();
            UsageError(format!(
                "unknown edit param {key:?} (supported: {})",
                names.join(", ")
            ))
        })?;
    let v: f32 = val
        .parse()
        .map_err(|_| UsageError(format!("edit set {key}: expected a number, got {val:?}")))?;
    Ok((id, ParamValue::F32(v)))
}

fn cmd_edit_set(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let kvs = flags.take_positionals();
        flags.finish()?;
        if kvs.is_empty() {
            return Err(UsageError(
                "edit set requires at least one param=value argument".to_owned(),
            )
            .into());
        }
        let mut parsed = Vec::with_capacity(kvs.len());
        for kv in &kvs {
            parsed.push(parse_param_kv(kv)?);
        }

        let (_core, session) = open_session(&catalog)?;
        let hub = session.edits();
        hub.open(image)
            .with_context(|| format!("opening image {}", image.0))?;
        for (id, value) in parsed {
            hub.begin_gesture(image, StepLabel::Param(id))?;
            let mut delta = ParamDelta::new();
            delta.0.insert(id, value);
            hub.update_gesture(image, delta)?;
            run_edit_command(&session, EditCommand::CommitGesture { image })?;
        }

        let state = session.query().edit_state(image)?;
        println!(
            "image {} — head_seq {} (persisted={})",
            image.0, state.head_seq, state.persisted
        );
        close_quiet(session)?;
        Ok(0)
    })
}

fn cmd_edit_get(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let json = flags.take_switch("--json");
        flags.finish()?;

        let (_core, session) = open_session(&catalog)?;
        let state = session.query().edit_state(image)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&state.recipe)?);
        } else {
            println!(
                "image {} — head_seq {} persisted={} neutral={}",
                image.0,
                state.head_seq,
                state.persisted,
                state.recipe.is_neutral()
            );
            for (name, id) in SCALAR_PARAMS {
                if let ParamValue::F32(v) = state.recipe.get(*id) {
                    println!("  {name:<12} {v}");
                }
            }
        }
        close_quiet(session)?;
        Ok(0)
    })
}

// ---------------------------------------------------------------------------
// `history` / `step-to` / `undo` / `redo` / `clear-history`
// ---------------------------------------------------------------------------

pub(crate) fn cmd_history(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let json = flags.take_switch("--json");
        flags.finish()?;

        let (_core, session) = open_session(&catalog)?;
        let steps = session.query().edit_history(image)?;
        if json {
            let rows: Vec<serde_json::Value> = steps
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "seq": s.seq,
                        "kind": s.label.kind(),
                        "ts": s.ts,
                        "is_head": s.is_head,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            for s in &steps {
                println!(
                    "{:>4}{} {:<18} {}",
                    s.seq,
                    if s.is_head { "*" } else { " " },
                    s.label.kind(),
                    s.ts
                );
            }
            eprintln!("{} step(s)", steps.len());
        }
        close_quiet(session)?;
        Ok(0)
    })
}

fn print_and_close(session: Session, image: ImageId) -> anyhow::Result<u8> {
    let state = session.query().edit_state(image)?;
    println!(
        "image {} — head_seq {} (persisted={})",
        image.0, state.head_seq, state.persisted
    );
    close_quiet(session)?;
    Ok(0)
}

pub(crate) fn cmd_step_to(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let seq = flags
            .take_value("--seq")?
            .map(|v| parse_num::<u64>("--seq", &v))
            .transpose()?
            .ok_or_else(|| UsageError("step-to requires --seq <n>".to_owned()))?;
        flags.finish()?;

        let (_core, session) = open_session(&catalog)?;
        run_edit_command(&session, EditCommand::StepTo { image, seq })?;
        print_and_close(session, image)
    })
}

pub(crate) fn cmd_undo(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(&session, EditCommand::Undo { image })?;
        print_and_close(session, image)
    })
}

pub(crate) fn cmd_redo(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(&session, EditCommand::Redo { image })?;
        print_and_close(session, image)
    })
}

pub(crate) fn cmd_clear_history(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(&session, EditCommand::ClearHistory { image })?;
        print_and_close(session, image)
    })
}

// ---------------------------------------------------------------------------
// `snapshot create|restore|list|delete|rename`
// ---------------------------------------------------------------------------

pub(crate) fn cmd_snapshot(args: &[String]) -> anyhow::Result<u8> {
    match args.first().map(String::as_str) {
        Some("create") => cmd_snapshot_create(&args[1..]),
        Some("restore") => cmd_snapshot_restore(&args[1..]),
        Some("list") => cmd_snapshot_list(&args[1..]),
        Some("delete") => cmd_snapshot_delete(&args[1..]),
        Some("rename") => cmd_snapshot_rename(&args[1..]),
        _ => bad_subcommand("snapshot", "create|restore|list|delete|rename"),
    }
}

fn cmd_snapshot_create(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let name = required_name(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(&session, EditCommand::CreateSnapshot { image, name })?;
        let snaps = session.query().snapshots(image)?;
        for s in &snaps {
            println!("{} {} {}", s.id.0, s.name, s.ts);
        }
        close_quiet(session)?;
        Ok(0)
    })
}

fn cmd_snapshot_restore(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let snapshot = required_snapshot(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(&session, EditCommand::RestoreSnapshot { image, snapshot })?;
        print_and_close(session, image)
    })
}

fn cmd_snapshot_list(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let json = flags.take_switch("--json");
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        let snaps = session.query().snapshots(image)?;
        if json {
            let rows: Vec<serde_json::Value> = snaps
                .iter()
                .map(|s| serde_json::json!({"id": s.id.0, "name": s.name, "ts": s.ts}))
                .collect();
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            for s in &snaps {
                println!("{} {} {}", s.id.0, s.name, s.ts);
            }
            eprintln!("{} snapshot(s)", snaps.len());
        }
        close_quiet(session)?;
        Ok(0)
    })
}

fn cmd_snapshot_delete(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let snapshot = required_snapshot(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(&session, EditCommand::DeleteSnapshot { image, snapshot })?;
        close_quiet(session)?;
        Ok(0)
    })
}

fn cmd_snapshot_rename(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let snapshot = required_snapshot(&mut flags)?;
        let name = required_name(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(
            &session,
            EditCommand::RenameSnapshot {
                image,
                snapshot,
                name,
            },
        )?;
        close_quiet(session)?;
        Ok(0)
    })
}

// ---------------------------------------------------------------------------
// `preset create|list|apply|import|export|delete|rename`
// ---------------------------------------------------------------------------

const ALL_GROUPS: &[(&str, ParamGroup)] = &[
    ("profile", ParamGroup::BaseProfile),
    ("wb", ParamGroup::WhiteBalance),
    ("tone", ParamGroup::Tone),
    ("curve", ParamGroup::Curve),
    ("hsl", ParamGroup::ColorMixer),
    ("grade", ParamGroup::ColorGrading),
    ("bw", ParamGroup::BwMix),
    ("presence", ParamGroup::Presence),
    ("detail", ParamGroup::Detail),
    ("optics", ParamGroup::Optics),
    ("geometry", ParamGroup::Geometry),
    ("effects", ParamGroup::Effects),
    ("masks", ParamGroup::Masks),
    ("retouch", ParamGroup::Retouch),
];

fn parse_groups(s: &str) -> Result<ParamSubset, UsageError> {
    let mut groups = std::collections::BTreeSet::new();
    for tok in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let g = ALL_GROUPS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(tok))
            .map(|(_, g)| *g)
            .ok_or_else(|| UsageError(format!("unknown param group {tok:?}")))?;
        groups.insert(g);
    }
    Ok(ParamSubset { groups })
}

fn all_groups() -> ParamSubset {
    ParamSubset {
        groups: ALL_GROUPS.iter().map(|(_, g)| *g).collect(),
    }
}

pub(crate) fn cmd_preset(args: &[String]) -> anyhow::Result<u8> {
    match args.first().map(String::as_str) {
        Some("create") => cmd_preset_create(&args[1..]),
        Some("list") => cmd_preset_list(&args[1..]),
        Some("apply") => cmd_preset_apply(&args[1..]),
        Some("import") => cmd_preset_import(&args[1..]),
        Some("export") => cmd_preset_export(&args[1..]),
        Some("delete") => cmd_preset_delete(&args[1..]),
        Some("rename") => cmd_preset_rename(&args[1..]),
        _ => bad_subcommand("preset", "create|list|apply|import|export|delete|rename"),
    }
}

fn cmd_preset_create(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let name = required_name(&mut flags)?;
        let group = flags.take_value("--group")?;
        let groups_flag = flags.take_value("--groups")?;
        flags.finish()?;
        let subset = groups_flag
            .as_deref()
            .map(parse_groups)
            .transpose()?
            .unwrap_or_else(all_groups);

        let (_core, session) = open_session(&catalog)?;
        run_edit_command(
            &session,
            EditCommand::CreatePreset {
                from: image,
                name,
                group,
                subset,
            },
        )?;
        for p in session.query().presets()? {
            println!("{} {} {:?}", p.id.0, p.name, p.group);
        }
        close_quiet(session)?;
        Ok(0)
    })
}

fn cmd_preset_list(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let json = flags.take_switch("--json");
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        let presets = session.query().presets()?;
        if json {
            let rows: Vec<serde_json::Value> = presets
                .iter()
                .map(|p| serde_json::json!({"id": p.id.0, "name": p.name, "group": p.group}))
                .collect();
            println!("{}", serde_json::to_string_pretty(&rows)?);
        } else {
            for p in &presets {
                println!(
                    "{} {} {}",
                    p.id.0,
                    p.group.as_deref().unwrap_or("-"),
                    p.name
                );
            }
            eprintln!("{} preset(s)", presets.len());
        }
        close_quiet(session)?;
        Ok(0)
    })
}

fn cmd_preset_apply(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let preset = required_preset_id(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(
            &session,
            EditCommand::ApplyPreset {
                images: vec![image],
                preset,
            },
        )?;
        print_and_close(session, image)
    })
}

fn cmd_preset_import(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let paths: Vec<PathBuf> = flags
            .take_positionals()
            .into_iter()
            .map(PathBuf::from)
            .collect();
        flags.finish()?;
        if paths.is_empty() {
            return Err(
                UsageError("preset import requires at least one .xmp path".to_owned()).into(),
            );
        }
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(&session, EditCommand::ImportPresetFiles { paths })?;
        for p in session.query().presets()? {
            println!(
                "{} {} {}",
                p.id.0,
                p.group.as_deref().unwrap_or("-"),
                p.name
            );
        }
        close_quiet(session)?;
        Ok(0)
    })
}

fn cmd_preset_export(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let preset = required_preset_id(&mut flags)?;
        let dest = flags
            .take_value("--out")?
            .map(PathBuf::from)
            .ok_or_else(|| UsageError("preset export requires --out <path>".to_owned()))?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(
            &session,
            EditCommand::ExportPreset {
                preset,
                dest: dest.clone(),
            },
        )?;
        println!("wrote {}", dest.display());
        close_quiet(session)?;
        Ok(0)
    })
}

fn cmd_preset_delete(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let preset = required_preset_id(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(&session, EditCommand::DeletePreset { preset })?;
        close_quiet(session)?;
        Ok(0)
    })
}

fn cmd_preset_rename(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let preset = required_preset_id(&mut flags)?;
        let name = required_name(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(&session, EditCommand::RenamePreset { preset, name })?;
        close_quiet(session)?;
        Ok(0)
    })
}

// ---------------------------------------------------------------------------
// `xmp write|read|status`
// ---------------------------------------------------------------------------

pub(crate) fn cmd_xmp(args: &[String]) -> anyhow::Result<u8> {
    match args.first().map(String::as_str) {
        Some("write") => cmd_xmp_write(&args[1..]),
        Some("read") => cmd_xmp_read(&args[1..]),
        Some("status") => cmd_xmp_status(&args[1..]),
        _ => bad_subcommand("xmp", "write|read|status"),
    }
}

fn cmd_xmp_write(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(
            &session,
            EditCommand::WriteMetadata {
                images: vec![image],
            },
        )?;
        println!("wrote sidecar for image {}", image.0);
        close_quiet(session)?;
        Ok(0)
    })
}

fn cmd_xmp_read(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        run_edit_command(
            &session,
            EditCommand::ReadMetadata {
                images: vec![image],
            },
        )?;
        print_and_close(session, image)
    })
}

fn cmd_xmp_status(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let image = required_image(&mut flags)?;
        let json = flags.take_switch("--json");
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        let statuses = session.query().xmp_status(&[image])?;
        let status = statuses.first().map(|(_, s)| *s);
        if json {
            println!(
                "{}",
                serde_json::json!({"image": image.0, "status": format!("{:?}", status)})
            );
        } else {
            println!("image {} — xmp status: {:?}", image.0, status);
        }
        close_quiet(session)?;
        Ok(0)
    })
}
