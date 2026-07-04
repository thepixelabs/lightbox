// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The single serialized writer (spec §5.1 / §3.2).
//!
//! One dedicated OS thread owns the sole write connection. Mutations are
//! closures marshalled to that thread by [`WriterHandle::with_txn`]; each
//! closure runs inside **exactly one** WAL transaction (`BEGIN IMMEDIATE`) —
//! the crash-safety invariant of architecture §3.1. There is no
//! `SQLITE_BUSY` between workspace writers by construction: they all queue
//! here.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc;
use std::thread::JoinHandle;

use rusqlite::{Connection, TransactionBehavior};

use crate::error::{CatalogError, Result};

/// A unit of work for the writer thread.
type Job = Box<dyn FnOnce(&mut Connection) + Send>;

enum Msg {
    Job(Job),
    Shutdown,
}

/// Cheap-to-clone handle that enqueues write transactions onto the dedicated
/// writer thread (spec §3.2).
#[derive(Clone)]
pub struct WriterHandle {
    tx: mpsc::Sender<Msg>,
}

impl WriterHandle {
    /// Runs `f` on the writer thread inside one WAL transaction.
    ///
    /// Commits when `f` returns `Ok`; rolls back when it returns `Err` (the
    /// error is returned unchanged) or panics (mapped to
    /// [`CatalogError::Internal`]; the writer thread survives). Blocks the
    /// calling thread until the transaction completes — callers that must not
    /// block (the UI) enqueue commands via `lightbox-core` instead.
    pub fn with_txn<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut CatalogTxn<'_>) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply_tx, reply_rx) = mpsc::sync_channel::<Result<T>>(1);
        let job: Job = Box::new(move |conn| {
            let outcome =
                catch_unwind(AssertUnwindSafe(|| run_txn(conn, f))).unwrap_or_else(|panic| {
                    let what = panic_message(&*panic);
                    tracing::error!(panic = %what, "write transaction panicked; rolled back");
                    Err(CatalogError::Internal(format!(
                        "write transaction panicked (rolled back): {what}"
                    )))
                });
            let _ = reply_tx.send(outcome);
        });
        self.tx
            .send(Msg::Job(job))
            .map_err(|_| CatalogError::WriterGone)?;
        reply_rx.recv().map_err(|_| CatalogError::WriterGone)?
    }
}

fn run_txn<T>(
    conn: &mut Connection,
    f: impl FnOnce(&mut CatalogTxn<'_>) -> Result<T>,
) -> Result<T> {
    // IMMEDIATE: take the write lock up front so the transaction can never
    // fail with a lock upgrade mid-way.
    let txn = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut ctx = CatalogTxn { txn };
    match f(&mut ctx) {
        Ok(value) => {
            ctx.txn.commit()?;
            Ok(value)
        }
        // Dropping the transaction rolls it back.
        Err(err) => Err(err),
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_owned()
    }
}

/// An open write transaction. All mutation DAOs (spec §3.2) hang off this
/// type — see `dao.rs`. No SQL leaks upward.
pub struct CatalogTxn<'conn> {
    pub(crate) txn: rusqlite::Transaction<'conn>,
}

/// The writer thread itself; owned by `Catalog`, joined on drop.
pub(crate) struct Writer {
    tx: mpsc::Sender<Msg>,
    join: Option<JoinHandle<()>>,
}

impl Writer {
    /// Moves `conn` (the sole write connection) onto a dedicated thread.
    pub(crate) fn spawn(conn: Connection) -> std::io::Result<Writer> {
        let (tx, rx) = mpsc::channel::<Msg>();
        let join = std::thread::Builder::new()
            .name("lightbox-catalog-writer".to_owned())
            .spawn(move || {
                let mut conn = conn;
                while let Ok(msg) = rx.recv() {
                    match msg {
                        Msg::Job(job) => job(&mut conn),
                        Msg::Shutdown => break,
                    }
                }
                // Best-effort WAL checkpoint so a clean close leaves a
                // compact main file (crash safety never depends on this).
                let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
            })?;
        Ok(Writer {
            tx,
            join: Some(join),
        })
    }

    pub(crate) fn handle(&self) -> WriterHandle {
        WriterHandle {
            tx: self.tx.clone(),
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
