// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! G1, `dcamprof` **subprocess** orchestration.
//!
//! `dcamprof` (GPL-3) is invoked as a child process only. It is **never** a
//! crate dependency, **never** linked, and this whole tool is excluded from app
//! packaging (spec §1.1/§7.6). The GPL boundary is the process boundary: the
//! only artifact that crosses back is a `.dcp` file, which
//! [`validate`](crate::validate) re-parses with our own permissive-licensed
//! engine before it is allowed near `assets/`.
//!
//! ## Feature gate (dcamprof is ABSENT here)
//!
//! The build machine has no `dcamprof` (spec §0). The argv builders and the
//! binary-discovery logic compile and unit-test unconditionally, they touch no
//! GPL code, only [`std::process`]. The one thing gated behind the
//! **off-by-default `dcamprof` cargo feature** is the actual [`Dcamprof::run`]
//! spawn: with the feature off (the default), `run` returns
//! [`DcamprofError::FeatureDisabled`] and nothing is executed, so the default
//! build never tries to launch an absent tool. Enable it (and install
//! `dcamprof`) to actually generate profiles:
//!
//! ```sh
//! cargo run -p lightbox-profgen --features dcamprof -- generate <session-dir>
//! ```

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::session::Session;

/// Environment variable naming the `dcamprof` binary explicitly. Checked before
/// `PATH` so a sandbox can pin an exact vetted build.
pub const DCAMPROF_ENV: &str = "DCAMPROF_BIN";

/// Failures from the dcamprof orchestration.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DcamprofError {
    /// The tool was built without the `dcamprof` feature, the default on a
    /// machine where dcamprof is absent. No subprocess is attempted.
    #[error(
        "dcamprof generation is disabled: rebuild with `--features dcamprof` and install dcamprof \
         (see tools/lightbox-profgen/docs/runbook.md). G6 real-profile content is DEFERRED."
    )]
    FeatureDisabled,
    /// `dcamprof` could not be located (neither `$DCAMPROF_BIN` nor `PATH`).
    #[error("dcamprof binary not found (set ${0} or add it to PATH)")]
    NotFound(&'static str),
    /// The subprocess exited non-zero.
    #[error("dcamprof {stage} exited with {code}: {stderr}")]
    NonZeroExit {
        /// Which pipeline stage failed.
        stage: &'static str,
        /// The process exit code (or -1 if signalled).
        code: i32,
        /// Captured stderr.
        stderr: String,
    },
    /// An I/O error spawning or communicating with the child.
    #[error("dcamprof io: {0}")]
    Io(String),
}

/// The intermediate + final artifacts a generation run produces inside its work
/// dir. Paths are computed even when execution is gated off, so packaging and
/// tests can reason about the layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationPlan {
    /// The dcamprof binary that would run.
    pub bin: PathBuf,
    /// The extracted target patch data (`dcamprof make-target` output).
    pub target_ti3: PathBuf,
    /// The final camera profile (`dcamprof make-profile -o … .dcp`).
    pub output_dcp: PathBuf,
    /// The `make-target` argv (stage 1), for inspection / tests.
    pub make_target_argv: Vec<OsString>,
    /// The `make-profile` argv (stage 2), for inspection / tests.
    pub make_profile_argv: Vec<OsString>,
}

/// A discovered `dcamprof` binary + argv builder. Constructing one performs no
/// I/O beyond locating the executable.
#[derive(Clone, Debug)]
pub struct Dcamprof {
    bin: PathBuf,
}

impl Dcamprof {
    /// Wraps an explicit binary path (no discovery). Useful in tests and when a
    /// caller already vetted the executable.
    pub fn at(bin: impl Into<PathBuf>) -> Dcamprof {
        Dcamprof { bin: bin.into() }
    }

    /// Locates `dcamprof` via `$DCAMPROF_BIN`, else the first `dcamprof` on
    /// `PATH`. Returns [`DcamprofError::NotFound`] if neither resolves.
    pub fn discover() -> Result<Dcamprof, DcamprofError> {
        if let Some(explicit) = std::env::var_os(DCAMPROF_ENV) {
            let p = PathBuf::from(explicit);
            if !p.as_os_str().is_empty() {
                return Ok(Dcamprof { bin: p });
            }
        }
        if let Some(found) = search_path("dcamprof") {
            return Ok(Dcamprof { bin: found });
        }
        Err(DcamprofError::NotFound(DCAMPROF_ENV))
    }

    /// The binary this instance would execute.
    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// Computes the full two-stage generation plan (argv + artifact paths) for a
    /// session, writing intermediates under `work_dir`. **Pure**, builds the
    /// command lines without executing anything, so the wiring is unit-tested on
    /// a machine with no dcamprof.
    ///
    /// Stage 1 `make-target` extracts the chart patches from the target-shot
    /// raw(s); stage 2 `make-profile` fits the dual-illuminant DCP. The exact
    /// dcamprof invocation is pinned by the G2 capture protocol; this is the
    /// canonical wiring the content line uses.
    pub fn plan(&self, session: &Session, work_dir: impl AsRef<Path>) -> GenerationPlan {
        let work = work_dir.as_ref();
        let id = &session.meta.session_id;
        let target_ti3 = work.join(format!("{id}.ti3"));
        let output_dcp = work.join(format!("{id}.dcp"));

        let make_target_argv = self.make_target_argv(session, &target_ti3);
        let make_profile_argv = self.make_profile_argv(session, &target_ti3, &output_dcp);

        GenerationPlan {
            bin: self.bin.clone(),
            target_ti3,
            output_dcp,
            make_target_argv,
            make_profile_argv,
        }
    }

    /// The `dcamprof make-target` argv (stage 1). The first target-shot raw is
    /// the layout reference; dcamprof reads the remaining patches from it.
    fn make_target_argv(&self, session: &Session, out_ti3: &Path) -> Vec<OsString> {
        let mut argv: Vec<OsString> = vec![
            OsString::from("make-target"),
            OsString::from("-c"),
            chart_flag(session.meta.chart).into(),
        ];
        for raw in session.raw_paths() {
            argv.push(OsString::from("-p"));
            argv.push(raw.into_os_string());
        }
        argv.push(out_ti3.as_os_str().to_os_string());
        argv
    }

    /// The `dcamprof make-profile` argv (stage 2): fit the DCP from the patch
    /// data. Naming the DNG-style dual-illuminant output keeps our F-phase
    /// evaluator's interpolation path exercised.
    fn make_profile_argv(
        &self,
        _session: &Session,
        in_ti3: &Path,
        out_dcp: &Path,
    ) -> Vec<OsString> {
        vec![
            OsString::from("make-profile"),
            OsString::from("-o"),
            out_dcp.as_os_str().to_os_string(),
            in_ti3.as_os_str().to_os_string(),
        ]
    }

    /// Runs the two-stage pipeline and returns the produced `.dcp` bytes +
    /// dcamprof version for provenance.
    ///
    /// **Gated:** with the default (no `dcamprof` feature) this performs no I/O
    /// and returns [`DcamprofError::FeatureDisabled`]. G6 real-profile content
    /// is DEFERRED; nothing is fabricated.
    pub fn run(
        &self,
        session: &Session,
        work_dir: impl AsRef<Path>,
    ) -> Result<GenerationRecord, DcamprofError> {
        let plan = self.plan(session, work_dir);
        self.run_plan(&plan)
    }

    #[cfg(not(feature = "dcamprof"))]
    fn run_plan(&self, _plan: &GenerationPlan) -> Result<GenerationRecord, DcamprofError> {
        Err(DcamprofError::FeatureDisabled)
    }

    #[cfg(feature = "dcamprof")]
    fn run_plan(&self, plan: &GenerationPlan) -> Result<GenerationRecord, DcamprofError> {
        if let Some(parent) = plan.output_dcp.parent() {
            std::fs::create_dir_all(parent).map_err(|e| DcamprofError::Io(e.to_string()))?;
        }
        self.exec("make-target", &plan.make_target_argv)?;
        self.exec("make-profile", &plan.make_profile_argv)?;
        let dcp = std::fs::read(&plan.output_dcp).map_err(|e| DcamprofError::Io(e.to_string()))?;
        Ok(GenerationRecord {
            dcp,
            output_dcp: plan.output_dcp.clone(),
            dcamprof_version: self.version().unwrap_or_else(|_| "unknown".into()),
        })
    }

    /// Runs `dcamprof` with the given argv, capturing stderr. Compiled only when
    /// the feature is on (it is the sole path that actually spawns the GPL tool).
    #[cfg(feature = "dcamprof")]
    fn exec(&self, stage: &'static str, argv: &[OsString]) -> Result<(), DcamprofError> {
        let out = std::process::Command::new(&self.bin)
            .args(argv)
            .output()
            .map_err(|e| DcamprofError::Io(e.to_string()))?;
        if !out.status.success() {
            return Err(DcamprofError::NonZeroExit {
                stage,
                code: out.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        Ok(())
    }

    /// Best-effort `dcamprof --version` (feature-gated exec).
    #[cfg(feature = "dcamprof")]
    fn version(&self) -> Result<String, DcamprofError> {
        let out = std::process::Command::new(&self.bin)
            .arg("--version")
            .output()
            .map_err(|e| DcamprofError::Io(e.to_string()))?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}

/// The result of a successful generation run.
#[derive(Clone, Debug)]
pub struct GenerationRecord {
    /// The produced `.dcp` bytes (re-parsed by the validation harness).
    pub dcp: Vec<u8>,
    /// Where the `.dcp` was written.
    pub output_dcp: PathBuf,
    /// The dcamprof version string, recorded in provenance.
    pub dcamprof_version: String,
}

/// The `dcamprof make-target -c` chart flag for a [`ChartKind`].
fn chart_flag(chart: crate::session::ChartKind) -> &'static str {
    use crate::session::ChartKind::*;
    match chart {
        ColorChecker24 => "cc24",
        ColorCheckerSg => "ccsg",
        It8 => "it8",
    }
}

/// Finds an executable named `name` on `PATH` (no shell, no globbing).
fn search_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Whether `p` is a regular file (on Unix, additionally with an exec bit).
fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Renders an argv as a shell-ish string for logs (display only, never fed to
/// a shell).
pub fn display_argv(bin: &Path, argv: &[OsString]) -> String {
    let mut s = bin.display().to_string();
    for a in argv {
        s.push(' ');
        s.push_str(&OsStr::new(a).to_string_lossy());
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{BodyMeta, CaptureIlluminant, CaptureRaw, ChartKind, SessionMeta};

    fn session() -> Session {
        let meta = SessionMeta {
            session_id: "sess-x".into(),
            body: BodyMeta {
                make: "Nikon".into(),
                model: "Z6".into(),
            },
            chart: ChartKind::ColorChecker24,
            illuminants: vec![CaptureIlluminant {
                name: "daylight".into(),
                cct: Some(6504.0),
            }],
            raws: vec![CaptureRaw {
                illuminant: "daylight".into(),
                path: "d65.nef".into(),
            }],
            captured_at: None,
            operator: None,
            notes: None,
        };
        Session::from_meta("/sessions/sess-x", meta).unwrap()
    }

    #[test]
    fn plan_builds_both_stage_argvs_and_paths() {
        let d = Dcamprof::at("/opt/dcamprof");
        let plan = d.plan(&session(), "/work");

        assert_eq!(plan.bin, PathBuf::from("/opt/dcamprof"));
        assert_eq!(plan.target_ti3, PathBuf::from("/work/sess-x.ti3"));
        assert_eq!(plan.output_dcp, PathBuf::from("/work/sess-x.dcp"));

        // Stage 1: make-target with the chart flag and the raw as a patch source.
        let t = &plan.make_target_argv;
        assert_eq!(t[0], OsStr::new("make-target"));
        assert!(t.iter().any(|a| a == OsStr::new("cc24")));
        assert!(t
            .iter()
            .any(|a| a == OsStr::new("/sessions/sess-x/d65.nef")));

        // Stage 2: make-profile -o <out.dcp> <in.ti3>.
        let p = &plan.make_profile_argv;
        assert_eq!(p[0], OsStr::new("make-profile"));
        assert!(p.iter().any(|a| a == OsStr::new("/work/sess-x.dcp")));
        assert!(p.iter().any(|a| a == OsStr::new("/work/sess-x.ti3")));
    }

    #[test]
    fn chart_flags_cover_every_kind() {
        assert_eq!(chart_flag(ChartKind::ColorChecker24), "cc24");
        assert_eq!(chart_flag(ChartKind::ColorCheckerSg), "ccsg");
        assert_eq!(chart_flag(ChartKind::It8), "it8");
    }

    #[cfg(not(feature = "dcamprof"))]
    #[test]
    fn run_is_deferred_without_the_feature() {
        // On this machine (dcamprof absent, feature off) run() executes nothing
        // and reports the deferred status, no fabricated profile.
        let d = Dcamprof::at("/opt/dcamprof");
        let err = d.run(&session(), "/work").unwrap_err();
        assert!(matches!(err, DcamprofError::FeatureDisabled), "{err}");
    }

    #[test]
    fn discover_reports_not_found_when_env_unset_and_absent() {
        // Point discovery at an empty PATH with no override so it deterministically
        // fails on the machine under test.
        temp_env_scope(|| {
            std::env::remove_var(DCAMPROF_ENV);
            std::env::set_var("PATH", "");
            assert!(matches!(
                Dcamprof::discover(),
                Err(DcamprofError::NotFound(_))
            ));
        });
    }

    /// Serializes env-mutating tests (process-global state).
    fn temp_env_scope(f: impl FnOnce()) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_path = std::env::var_os("PATH");
        let saved_bin = std::env::var_os(DCAMPROF_ENV);
        f();
        match saved_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        match saved_bin {
            Some(v) => std::env::set_var(DCAMPROF_ENV, v),
            None => std::env::remove_var(DCAMPROF_ENV),
        }
    }

    #[test]
    fn display_argv_is_readable() {
        let d = Dcamprof::at("/opt/dcamprof");
        let plan = d.plan(&session(), "/work");
        let s = display_argv(d.bin(), &plan.make_target_argv);
        assert!(s.starts_with("/opt/dcamprof make-target"));
    }
}
