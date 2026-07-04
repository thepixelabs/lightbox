// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Tracing + panic-hook plumbing (E01 spec §5 T4).
//!
//! - `tracing` with an env-filter (`LIGHTBOX_LOG`, falling back to `RUST_LOG`,
//!   falling back to a default directive).
//! - Console (stderr) layer always; an optional **daily-rotating file layer**
//!   pointed at `<catalog>.lbdata/logs/` once a session opens one.
//! - A panic hook that logs the panic through `tracing` before the default
//!   hook runs. File writes are synchronous (no background writer thread), so
//!   everything logged before a panic or `kill -9` is already flushed to the
//!   OS — there is no in-process buffer to lose.

use std::io::IsTerminal;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Environment variable consulted first for filter directives.
pub const LOG_ENV_VAR: &str = "LIGHTBOX_LOG";

/// Filter used when neither `LIGHTBOX_LOG` nor `RUST_LOG` is set.
pub const DEFAULT_FILTER: &str = "info";

/// Errors from observability setup.
#[derive(Debug, thiserror::Error)]
pub enum ObservabilityError {
    /// The log directory could not be created.
    #[error("cannot create log directory {dir}: {source}")]
    CreateLogDir {
        /// Directory that failed to be created.
        dir: PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },
    /// A global tracing subscriber was already installed.
    #[error("a global tracing subscriber is already set")]
    AlreadyInitialized(#[source] tracing_subscriber::util::TryInitError),
}

/// Options for [`init`].
#[derive(Debug, Clone, Default)]
pub struct ObservabilityOptions {
    /// Directory for the rotating file log (e.g. `<catalog>.lbdata/logs/`).
    /// Created if missing. `None` = console only (CLI default before a
    /// catalog is open).
    pub log_dir: Option<PathBuf>,
    /// Filter directives overriding the environment (tests, `--verbose`).
    pub filter: Option<String>,
}

/// Initializes the global tracing subscriber and installs the panic hook.
///
/// Call once per process, early in `main`. Returns an error if a global
/// subscriber is already set (embedders and tests should build their own via
/// [`try_build_subscriber`] instead).
pub fn init(opts: &ObservabilityOptions) -> Result<(), ObservabilityError> {
    let subscriber = try_build_subscriber(opts)?;
    subscriber
        .try_init()
        .map_err(ObservabilityError::AlreadyInitialized)?;
    install_panic_hook();
    Ok(())
}

/// Builds the layered subscriber without installing it globally.
///
/// Exposed so tests (and embedders) can run it scoped via
/// `tracing::subscriber::with_default`.
pub fn try_build_subscriber(
    opts: &ObservabilityOptions,
) -> Result<impl SubscriberInitExt + tracing::Subscriber + Send + Sync, ObservabilityError> {
    let filter = match &opts.filter {
        Some(f) => EnvFilter::new(f),
        None => std::env::var(LOG_ENV_VAR)
            .map(EnvFilter::new)
            .or_else(|_| EnvFilter::try_from_default_env())
            .unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER)),
    };

    let console = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal());

    let file = match &opts.log_dir {
        Some(dir) => Some(file_layer(dir)?),
        None => None,
    };

    Ok(tracing_subscriber::registry()
        .with(filter)
        .with(console)
        .with(file))
}

/// A daily-rotating, synchronous (write-through) file layer in `dir`.
///
/// Files are named `lightbox.log.<date>`. Synchronous on purpose: log volume
/// at M0 is tiny, and write-through means a panic or `kill -9` cannot lose
/// buffered lines (spec T4: "panic hook that logs + flushes").
pub fn file_layer<S>(dir: &Path) -> Result<impl Layer<S>, ObservabilityError>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    std::fs::create_dir_all(dir).map_err(|source| ObservabilityError::CreateLogDir {
        dir: dir.to_path_buf(),
        source,
    })?;
    let appender = tracing_appender::rolling::daily(dir, "lightbox.log");
    Ok(tracing_subscriber::fmt::layer()
        .with_writer(appender)
        .with_ansi(false))
}

/// Installs a panic hook that logs the panic (message + location) through
/// `tracing`, then delegates to the previously-installed hook. Idempotent.
pub fn install_panic_hook() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        let payload = payload_str(info);
        match info.location() {
            Some(loc) => tracing::error!(
                target: "lightbox::panic",
                file = loc.file(),
                line = loc.line(),
                "panic: {payload}"
            ),
            None => tracing::error!(target: "lightbox::panic", "panic: {payload}"),
        }
        previous(info);
    }));
}

fn payload_str(info: &PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// MakeWriter capturing everything into a shared buffer.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        fn contents(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Capture {
            self.clone()
        }
    }

    #[test]
    fn file_layer_writes_to_log_dir() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        let layer = file_layer(&logs).unwrap();
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(probe = 1, "observability file-layer test line");
        });

        let mut entries: Vec<_> = std::fs::read_dir(&logs).unwrap().collect();
        assert_eq!(entries.len(), 1, "exactly one rotated log file expected");
        let path = entries.pop().unwrap().unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("observability file-layer test line"),
            "log line missing from {}: {text:?}",
            path.display()
        );
    }

    #[test]
    fn file_layer_fails_cleanly_on_uncreatable_dir() {
        let dir = tempfile::tempdir().unwrap();
        // A *file* where the directory should go makes create_dir_all fail.
        let clash = dir.path().join("not-a-dir");
        std::fs::write(&clash, b"x").unwrap();
        let err = file_layer::<tracing_subscriber::Registry>(&clash.join("logs"))
            .err()
            .expect("expected CreateLogDir error");
        assert!(matches!(err, ObservabilityError::CreateLogDir { .. }));
    }

    #[test]
    fn panic_hook_logs_through_tracing() {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_writer(capture.clone())
                .with_ansi(false),
        );

        install_panic_hook();
        // Silence the default hook's stderr backtrace chatter for this scope.
        tracing::subscriber::with_default(subscriber, || {
            let result = std::panic::catch_unwind(|| panic!("deliberate test panic"));
            assert!(result.is_err());
        });

        let text = capture.contents();
        assert!(
            text.contains("deliberate test panic") && text.contains("lightbox::panic"),
            "panic not captured via tracing: {text:?}"
        );
    }

    #[test]
    fn build_subscriber_honors_explicit_filter() {
        let opts = ObservabilityOptions {
            log_dir: None,
            filter: Some("off".to_owned()),
        };
        // Just proving it builds; installing globally is main()'s business.
        let _subscriber = try_build_subscriber(&opts).unwrap();
    }
}
