// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! E06 (spec §4.7, T12) headless driver: `jobs
//! list/watch/cancel/pause/resume/demo` over the
//! `Session::{jobs, activity}` + `Command::Jobs` seam, plus `--wait-idle`.
//!
//! **Job state is process-local** (E06 ships no durable/cross-process
//! queue, spec §2 non-goals): a fresh CLI session starts with an empty
//! scheduler, so `list`/`cancel`/… against a catalog show only jobs *this*
//! process spawned. That makes `jobs demo` the E2E workhorse (the T12
//! script shape): it spawns a synthetic N-item Background group on the
//! session scheduler, streams aggregated progress from the activity
//! snapshot, optionally cancels/pauses mid-run **through the command bus**
//! (proving the `Command::Jobs` round-trip), and exits via wait-idle.
//! The other subcommands exercise the same code paths and exist for
//! symmetry with the spec's command surface (and for any future long-lived
//! headless mode).

use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use anyhow::bail;
use lightbox_core::{
    ActivityEntry, ActivityRef, ActivitySnapshot, Class, CloseOpts, ClosePolicy, Command, Core,
    CoreConfig, Event, GroupId, JobCommand, JobId, JobSpec, JobState, Outcome, ProgressStyle,
    Session,
};

use crate::edit::bad_subcommand;
use crate::{parse_num, required_catalog, with_usage, Flags, UsageError};

pub(crate) fn cmd_jobs(args: &[String]) -> anyhow::Result<u8> {
    let Some(sub) = args.first() else {
        return bad_subcommand("jobs", "list|watch|cancel|pause|resume|demo");
    };
    let rest = &args[1..];
    match sub.as_str() {
        "list" => cmd_list(rest),
        "watch" => cmd_watch(rest),
        "cancel" => cmd_control(rest, ControlVerb::Cancel),
        "pause" => cmd_control(rest, ControlVerb::Pause),
        "resume" => cmd_control(rest, ControlVerb::Resume),
        "demo" => cmd_demo(rest),
        _ => bad_subcommand("jobs", "list|watch|cancel|pause|resume|demo"),
    }
}

fn open_session(catalog: &std::path::Path) -> anyhow::Result<(Core, Session)> {
    let core = Core::start(CoreConfig::default())?;
    let session = core.open_catalog(catalog, None)?;
    Ok((core, session))
}

fn parse_class(s: &str) -> Result<Class, UsageError> {
    match s {
        "interactive" => Ok(Class::Interactive),
        "foreground" => Ok(Class::Foreground),
        "background" => Ok(Class::Background),
        other => Err(UsageError(format!(
            "--class expects interactive, foreground, or background, got {other:?}"
        ))),
    }
}

fn class_str(class: Class) -> &'static str {
    match class {
        Class::Interactive => "interactive",
        Class::Foreground => "foreground",
        Class::Background => "background",
    }
}

fn state_str(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Paused => "paused",
        JobState::Cancelling => "cancelling",
        JobState::Done(Outcome::Completed) => "completed",
        JobState::Done(Outcome::Failed) => "failed",
        JobState::Done(Outcome::Cancelled) => "cancelled",
    }
}

fn ref_str(id: ActivityRef) -> String {
    match id {
        ActivityRef::Job(id) => format!("job:{}", id.get()),
        ActivityRef::Group(id) => format!("group:{}", id.get()),
    }
}

fn progress_str(entry: &ActivityEntry) -> String {
    match &entry.progress {
        Some(view) => {
            if let Some((done, total)) = view.items {
                format!("{done}/{total}")
            } else if let Some((done, total)) = view.bytes {
                format!("{done}B/{total}B")
            } else if let Some(f) = view.fraction {
                format!("{:.0}%", f64::from(f) * 100.0)
            } else {
                "—".to_owned()
            }
        }
        None => "—".to_owned(),
    }
}

fn print_snapshot(snapshot: &ActivitySnapshot, json: bool) -> anyhow::Result<()> {
    if json {
        let entries: Vec<serde_json::Value> = snapshot
            .entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "ref": ref_str(e.id),
                    "kind": e.kind,
                    "label": e.label,
                    "detail": e.detail,
                    "class": class_str(e.class),
                    "state": state_str(e.state),
                    "pausable": e.pausable,
                    "progress": e.progress.map(|p| serde_json::json!({
                        "fraction": p.fraction,
                        "items": p.items,
                        "bytes": p.bytes,
                    })),
                    "error": e.error,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "seq": snapshot.seq,
                "class_paused": {
                    "interactive": snapshot.is_class_paused(Class::Interactive),
                    "foreground": snapshot.is_class_paused(Class::Foreground),
                    "background": snapshot.is_class_paused(Class::Background),
                },
                "entries": entries,
            }))?
        );
    } else {
        for e in &snapshot.entries {
            println!(
                "{:<12} {:<22} {:<11} {:<10} {:>12}  {}{}",
                ref_str(e.id),
                e.kind,
                class_str(e.class),
                state_str(e.state),
                progress_str(e),
                e.label,
                e.error
                    .as_deref()
                    .map(|err| format!("  [error: {err}]"))
                    .unwrap_or_default(),
            );
        }
        eprintln!("{} entr{} (seq {})", snapshot.entries.len(),
            if snapshot.entries.len() == 1 { "y" } else { "ies" }, snapshot.seq);
    }
    Ok(())
}

/// `jobs list`: one activity snapshot. Process-local (see module docs).
fn cmd_list(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let json = flags.take_switch("--json");
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        print_snapshot(&session.activity(), json)?;
        session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
        Ok(0)
    })
}

/// `jobs watch`: streams `Event::Jobs` lines for `--duration` seconds
/// (default 10) or until the scheduler goes idle.
fn cmd_watch(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let duration = flags
            .take_value("--duration")?
            .map(|v| parse_num::<u64>("--duration", &v))
            .transpose()?
            .unwrap_or(10);
        flags.finish()?;
        let (_core, session) = open_session(&catalog)?;
        let mut rx = session.events();
        let deadline = Instant::now() + Duration::from_secs(duration);
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(Event::Jobs(ev)) => println!("{ev:?}"),
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
            }
        }
        session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
        Ok(0)
    })
}

enum ControlVerb {
    Cancel,
    Pause,
    Resume,
}

/// `jobs cancel|pause|resume`: issues the matching `Command::Jobs` through
/// the command bus. Targets: `--id <job>`, `--group <group>`, or (pause/
/// resume only) `--class <class>`.
fn cmd_control(args: &[String], verb: ControlVerb) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let id = flags
            .take_value("--id")?
            .map(|v| parse_num::<u64>("--id", &v))
            .transpose()?;
        let group = flags
            .take_value("--group")?
            .map(|v| parse_num::<u64>("--group", &v))
            .transpose()?;
        let class = flags.take_value("--class")?.map(|v| parse_class(&v)).transpose()?;
        flags.finish()?;

        let target = match (id, group) {
            (Some(id), None) => Some(ActivityRef::Job(JobId(
                NonZeroU64::new(id).ok_or_else(|| UsageError("--id must be >= 1".to_owned()))?,
            ))),
            (None, Some(g)) => Some(ActivityRef::Group(GroupId(
                NonZeroU64::new(g)
                    .ok_or_else(|| UsageError("--group must be >= 1".to_owned()))?,
            ))),
            (None, None) => None,
            (Some(_), Some(_)) => {
                return Err(UsageError("pass --id OR --group, not both".to_owned()).into())
            }
        };
        let cmd = match (&verb, target, class) {
            (ControlVerb::Cancel, Some(t), None) => JobCommand::Cancel(t),
            (ControlVerb::Pause, Some(t), None) => JobCommand::Pause(t),
            (ControlVerb::Resume, Some(t), None) => JobCommand::Resume(t),
            (ControlVerb::Pause, None, Some(c)) => JobCommand::PauseClass(c),
            (ControlVerb::Resume, None, Some(c)) => JobCommand::ResumeClass(c),
            (ControlVerb::Cancel, None, Some(_)) => {
                return Err(
                    UsageError("cancel takes --id or --group (no --class)".to_owned()).into(),
                )
            }
            _ => {
                return Err(UsageError(
                    "pass a target: --id <job> | --group <group> | --class <class>".to_owned(),
                )
                .into())
            }
        };
        let (_core, session) = open_session(&catalog)?;
        session.submit(Command::Jobs(cmd));
        // The command bus is async and job commands have no ack event;
        // give the dispatcher a beat, then show the resulting snapshot.
        std::thread::sleep(Duration::from_millis(150));
        print_snapshot(&session.activity(), false)?;
        session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
        Ok(0)
    })
}

/// Polls the scheduler to idle (T12 `--wait-idle`); false on timeout.
fn wait_idle(session: &Session, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if session.jobs().is_idle() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `jobs demo`: the T12 E2E scenario in one process. Spawns `--items`
/// synthetic Background jobs under one group, streams the aggregated
/// activity entry, optionally cancels the group / pauses the class through
/// the command bus mid-run, then waits idle (`--wait-idle`, non-zero exit
/// on timeout).
fn cmd_demo(args: &[String]) -> anyhow::Result<u8> {
    with_usage(|| {
        let mut flags = Flags::new(args);
        let catalog = required_catalog(&mut flags)?;
        let items = flags
            .take_value("--items")?
            .map(|v| parse_num::<u64>("--items", &v))
            .transpose()?
            .unwrap_or(500);
        let steps = flags
            .take_value("--item-steps")?
            .map(|v| parse_num::<u64>("--item-steps", &v))
            .transpose()?
            .unwrap_or(5);
        let fail = flags
            .take_value("--fail")?
            .map(|v| parse_num::<u64>("--fail", &v))
            .transpose()?
            .unwrap_or(0);
        let cancel_after = flags
            .take_value("--cancel-after")?
            .map(|v| parse_num::<u64>("--cancel-after", &v))
            .transpose()?;
        let pause_class_after = flags
            .take_value("--pause-class-after")?
            .map(|v| parse_num::<u64>("--pause-class-after", &v))
            .transpose()?;
        let wait_idle_secs = flags
            .take_value("--wait-idle")?
            .map(|v| parse_num::<u64>("--wait-idle", &v))
            .transpose()?
            .unwrap_or(120);
        let json = flags.take_switch("--json");
        flags.finish()?;

        let (_core, session) = open_session(&catalog)?;
        let sched = session.jobs();
        let group = sched.create_group(lightbox_core::GroupSpec::new(
            "demo.synthetic",
            format!("Synthetic demo ({items} items)"),
            Class::Background,
        ));
        for i in 0..items {
            let mut spec = JobSpec::new("demo.item", format!("item {i}"), Class::Background);
            spec.group = Some(group.id());
            spec.progress = ProgressStyle::Items { total: Some(steps) };
            let fail_this = i < fail;
            let handle = sched.spawn::<(), _, _>(spec, move |ctx| async move {
                for _ in 0..steps {
                    ctx.checkpoint().await?;
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    ctx.progress().advance(1);
                }
                if fail_this {
                    return Err(lightbox_jobs::JobError::Failed(format!(
                        "synthetic failure (item {i})"
                    )));
                }
                Ok(())
            });
            drop(handle); // detached: the group entry is the observer
        }
        group.close();
        let gid = group.id();
        eprintln!("spawned {items} jobs under group:{}", gid.get());

        let started = Instant::now();
        let mut cancelled = false;
        let mut paused = false;
        let mut last_print = Instant::now() - Duration::from_secs(1);
        loop {
            if let Some(ms) = cancel_after {
                if !cancelled && started.elapsed() >= Duration::from_millis(ms) {
                    session.submit(Command::Jobs(JobCommand::Cancel(ActivityRef::Group(gid))));
                    eprintln!("cancel-group submitted via command bus");
                    cancelled = true;
                }
            }
            if let Some(ms) = pause_class_after {
                if !paused && started.elapsed() >= Duration::from_millis(ms) {
                    session.submit(Command::Jobs(JobCommand::PauseClass(Class::Background)));
                    eprintln!("pause-class(background) submitted via command bus");
                    paused = true;
                }
            }
            if last_print.elapsed() >= Duration::from_millis(250) {
                let snapshot = session.activity();
                if let Some(entry) = snapshot.entries.iter().find(|e| e.id == ActivityRef::Group(gid)) {
                    eprintln!(
                        "[{:>6.2}s] {} {} {}",
                        started.elapsed().as_secs_f64(),
                        state_str(entry.state),
                        progress_str(entry),
                        entry.label,
                    );
                }
                last_print = Instant::now();
            }
            if session.jobs().is_idle() {
                break;
            }
            if paused && !cancelled {
                // A paused class never goes idle by itself; resume so the
                // demo terminates (proving the resume path too).
                if started.elapsed() >= Duration::from_millis(pause_class_after.unwrap_or(0) + 500)
                {
                    session.submit(Command::Jobs(JobCommand::ResumeClass(Class::Background)));
                    eprintln!("resume-class(background) submitted via command bus");
                    paused = false;
                }
            }
            if started.elapsed() > Duration::from_secs(wait_idle_secs) {
                bail!("--wait-idle {wait_idle_secs}s elapsed with jobs still live");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !wait_idle(&session, Duration::from_secs(wait_idle_secs)) {
            bail!("--wait-idle {wait_idle_secs}s elapsed with jobs still live");
        }

        // Final snapshot: the group entry lingers (completed_linger), so
        // the aggregated outcome is still visible here.
        print_snapshot(&session.activity(), json)?;
        session.close(CloseOpts::with_backup(ClosePolicy::Skip))?;
        Ok(0)
    })
}
