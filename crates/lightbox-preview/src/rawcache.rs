// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! The raw decode cache: container format, put/get, and accounting/LRU/
//! reconcile (E03 spec §3.3/§5.4, Phase E T17/T18).
//!
//! **Scope reminder (task-prompt instruction, worth repeating here): this
//! module stores OPAQUE bytes.** The payload a caller hands to [`RawCache::put`]
//! is never interpreted, demosaiced, or color-managed here — [`RawStageMeta`]
//! carries only the *declared* geometry/sample-format tag the producer
//! (E02/E05, M1+) supplies. This module's entire job is: does the container
//! round-trip bit-exact, is corruption detected and self-healed, and does
//! the store stay within its configured byte cap.
//!
//! ## Container format (T17, spec §3.3)
//!
//! `rawcache/<hh>/<content_hash_hex32>.<params_hash_hex16>.zst` — a single
//! zstd frame (built-in content checksum ON, level from config) whose
//! decompressed stream is a 28-byte header followed by the planar payload:
//!
//! ```text
//! [0..4)   magic "LBRC"
//! [4..6)   container_ver: u16 = 1
//! [6..8)   payload_schema: u16      // opaque to this crate
//! [8..16)  width: u32, height: u32
//! [16]     channels: u8
//! [17]     sample_format: u8        // 0=F16 1=F32 2=U16
//! [18..20) color_state: u16
//! [20..28) payload_len: u64
//! [28.. )  planar payload
//! ```
//!
//! All multi-byte header fields are little-endian — this crate's own
//! internal-cache-key convention (see `pyramid.rs`'s `VariantHash::
//! to_le_bytes` doc comment); nothing outside this module ever parses this
//! header directly, so there is no cross-tool wire-format requirement
//! pulling toward big-endian the way `ContentHash`/hex-displayed ids do.
//!
//! **Corruption handling.** zstd's own frame checksum (xxh64 over the
//! decompressed stream) is the sole integrity mechanism — no second,
//! redundant checksum column in the catalog (spec §4's `raw_cache_entry`
//! DDL carries none either). A checksum failure — or any other reason the
//! frame fails to parse — is a **typed miss**: [`RawCache::get`] returns
//! `Ok(None)` (never `Err`, spec §5.4: "never blocks render... always
//! recoverable by re-decoding upstream"), after best-effort dropping the
//! catalog row and deleting the file so a future `put()` heals it.
//!
//! ## Accounting/LRU/reconcile (T18, spec §5.4/§4)
//!
//! [`RawCache`] composes the container primitives above with
//! `lightbox_catalog::rawcache_dao`'s `raw_cache_entry` accounting: every
//! `put()` upserts a row and evicts LRU-oldest entries back under
//! [`crate::CacheLimits::rawcache_cap_bytes`] (default 5 GiB); [`RawCache::
//! reconcile`] walks the on-disk `rawcache/` tree and the catalog's rows in
//! both directions (spec T18 AC: "repairs BOTH divergence directions").
//!
//! **Touch is synchronous, not batched (a documented, deliberate scope
//! narrowing vs. `index.rs`'s preview touch-batcher).** Spec §3.2's
//! "Accounting write pressure" note (touches batched ≤5s/≥64 entries) is
//! written in the context of the *preview* index, which is touched on every
//! grid-scroll cull swap — a much hotter path than a raw-cache `get()`
//! (Develop-open, not scroll-rate). [`RawCache::get`] does one direct
//! single-row `UPDATE` via the catalog writer per hit instead of building a
//! second in-memory batcher just for this table. If profiling ever shows
//! writer contention from this, adopting `index.rs`'s batching shape here is
//! a self-contained follow-up (no `RawCache` API change needed — `get`
//! already returns before the touch's result is observable to the caller).
//! Recorded in `docs/plan/epics/E03-deviations.md`, Phase E.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use lightbox_catalog::{Catalog, NewRawCacheEntryRow};
use lightbox_types::ContentHash;

use crate::config::CacheLimits;
use crate::pyramid::RelPath;
use crate::sched::{CacheKind, EventSink, PreviewEvent};
use crate::store::{atomic_write, Store};

/// Free-space probe seam (Phase F, T21 ENOSPC pre-flight). Injectable so
/// tests can simulate "the disk is full" deterministically without real
/// quotas/root privileges. [`RealDiskSpaceProbe`] is the production impl.
pub(crate) trait DiskSpaceProbe: Send + Sync {
    /// Bytes free on the filesystem backing `path` (a directory that
    /// exists). Errs only on a genuine OS-level failure to query.
    fn available_bytes(&self, path: &Path) -> std::io::Result<u64>;
}

/// Real free-space query. Unix: `statvfs` (via `libc`, already an in-graph
/// workspace dependency — E02's LibRaw sandbox). Windows: no
/// `GetDiskFreeSpaceExW` binding exists in this workspace's dependency graph
/// yet (would need `windows-sys`, not currently pulled in by anything) — this
/// reports `u64::MAX` ("assume plenty of room") on that platform, which
/// means the ENOSPC pre-flight is a real, tested mechanism on unix and an
/// inert no-op on Windows today. Recorded as DEFERRED in
/// `docs/plan/epics/E03-deviations.md`, Phase F — coding a real Windows probe
/// is future work, not faked here.
pub(crate) struct RealDiskSpaceProbe;

impl DiskSpaceProbe for RealDiskSpaceProbe {
    fn available_bytes(&self, path: &Path) -> std::io::Result<u64> {
        diskspace::available_bytes(path)
    }
}

mod diskspace;
mod mmap_io;

// ── container format (T17) ──────────────────────────────────────────────

const MAGIC: [u8; 4] = *b"LBRC";
const CONTAINER_VER: u16 = 1;
const HEADER_LEN: usize = 28;

/// The planar payload's per-sample encoding (spec §3.3). Opaque beyond this
/// tag — this crate never reads a sample value.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
#[repr(u8)]
pub enum SampleFormat {
    F16 = 0,
    F32 = 1,
    U16 = 2,
}

impl SampleFormat {
    fn from_u8(v: u8) -> Option<SampleFormat> {
        match v {
            0 => Some(SampleFormat::F16),
            1 => Some(SampleFormat::F32),
            2 => Some(SampleFormat::U16),
            _ => None,
        }
    }
}

/// The accounting/store key (spec §5.4): content-addressed by the asset's
/// bytes plus the producer's canonical early-stage parameter hash.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct RawCacheKey {
    pub content_hash: ContentHash,
    /// xxh3-64 of the producer's canonical early-stage parameter encoding
    /// (E02/E05-owned; this crate only requires it be stable/deterministic).
    pub params_hash: u64,
}

impl RawCacheKey {
    /// `rawcache/<hh>/<content_hash_hex32>.<params_hash_hex16>.zst` (spec
    /// §3.3). `<hh>` is the content hash's first byte, matching `BlobStore`'s
    /// own fan-out convention (`store.rs`) — unlike the preview pyramid's
    /// derived `StoreKey`, a raw-cache entry's identity IS the
    /// `(content_hash, params_hash)` pair itself, so there is no separate
    /// xxh3-128 key-derivation step here.
    pub fn rel_path(&self) -> RelPath {
        RelPath(format!(
            "rawcache/{:02x}/{}.{:016x}.zst",
            self.content_hash.0[0],
            self.content_hash.to_hex(),
            self.params_hash
        ))
    }

    /// Big-endian bytes (matches the filename's hex and the catalog's
    /// storage convention — see `rawcache_dao::RawCacheEntryRow::
    /// params_hash`'s doc comment).
    fn params_hash_be(&self) -> [u8; 8] {
        self.params_hash.to_be_bytes()
    }
}

/// Declared geometry/format of a stored payload (spec §5.4). Producer-owned
/// content; this crate carries it, never validates it against the payload
/// bytes (e.g. does not assert `payload.len() == channels*width*height*
/// sample_size` — stride/padding conventions are E02/E05's to define).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct RawStageMeta {
    pub payload_schema: u16,
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub sample: SampleFormat,
    pub color_state: u16,
}

/// Opaque payload bytes handed to [`RawCache::put`]. A thin wrapper (not a
/// bare `&[u8]`) matching the spec's own `PlaneData<'_>` naming — makes call
/// sites self-documenting about which argument is "the pixels" versus
/// geometry metadata.
#[derive(Copy, Clone, Debug)]
pub struct PlaneData<'a>(pub &'a [u8]);

/// An owned, decompressed hit (spec §5.4 `RawCacheHit`). Stores the whole
/// decompressed stream (header + payload) once, with [`PlanarBuf::as_bytes`]
/// slicing past the header — avoids a second allocation/copy of a
/// potentially 100+ MB payload just to strip 28 bytes.
#[derive(Clone, Debug)]
pub struct PlanarBuf {
    raw: Vec<u8>,
    payload_start: usize,
}

impl PlanarBuf {
    pub fn as_bytes(&self) -> &[u8] {
        &self.raw[self.payload_start..]
    }

    pub fn len(&self) -> usize {
        self.raw.len() - self.payload_start
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::ops::Deref for PlanarBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// One successful [`RawCache::get`] (spec §5.4).
#[derive(Clone, Debug)]
pub struct RawCacheHit {
    pub meta: RawStageMeta,
    pub data: PlanarBuf,
}

/// Everything that can go wrong reading/writing a raw-cache container or its
/// accounting row. **Corruption is deliberately NOT a variant here** — spec
/// §3.3/§5.4: a bad checksum is a typed `Ok(None)` miss, not an `Err` (see
/// the module doc comment). This enum is for genuine unexpected failures
/// (permission denied, disk full, a catalog write rejected).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RawCacheError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("catalog: {0}")]
    Catalog(String),
    /// Phase F (T21) ENOSPC pre-flight: even after evicting to cap, the
    /// filesystem does not report enough free space for this write. Never
    /// surfaced as a panic or a torn write — `put()` returns this BEFORE
    /// `atomic_write` is attempted.
    #[error("disk full: need ~{needed} bytes headroom, {available} available")]
    DiskFull { needed: u64, available: u64 },
}

fn encode_header(meta: &RawStageMeta, payload_len: u64) -> [u8; HEADER_LEN] {
    let mut buf = [0u8; HEADER_LEN];
    buf[0..4].copy_from_slice(&MAGIC);
    buf[4..6].copy_from_slice(&CONTAINER_VER.to_le_bytes());
    buf[6..8].copy_from_slice(&meta.payload_schema.to_le_bytes());
    buf[8..12].copy_from_slice(&meta.width.to_le_bytes());
    buf[12..16].copy_from_slice(&meta.height.to_le_bytes());
    buf[16] = meta.channels;
    buf[17] = meta.sample as u8;
    buf[18..20].copy_from_slice(&meta.color_state.to_le_bytes());
    buf[20..28].copy_from_slice(&payload_len.to_le_bytes());
    buf
}

/// Parses the 28-byte header. `Err(reason)` (a human-readable string, not a
/// public type — see [`RawCacheError`]'s doc comment for why corruption
/// never becomes a typed `Err` at the public API) on anything that doesn't
/// look like a container this build wrote.
fn decode_header(bytes: &[u8]) -> Result<(RawStageMeta, u64), String> {
    if bytes.len() < HEADER_LEN {
        return Err(format!(
            "truncated header: {} bytes, need {HEADER_LEN}",
            bytes.len()
        ));
    }
    if bytes[0..4] != MAGIC {
        return Err("bad magic".to_owned());
    }
    let ver = u16::from_le_bytes(bytes[4..6].try_into().expect("checked len"));
    if ver != CONTAINER_VER {
        return Err(format!("unsupported container_ver {ver}"));
    }
    let payload_schema = u16::from_le_bytes(bytes[6..8].try_into().expect("checked len"));
    let width = u32::from_le_bytes(bytes[8..12].try_into().expect("checked len"));
    let height = u32::from_le_bytes(bytes[12..16].try_into().expect("checked len"));
    let channels = bytes[16];
    let sample = SampleFormat::from_u8(bytes[17])
        .ok_or_else(|| format!("bad sample_format byte {}", bytes[17]))?;
    let color_state = u16::from_le_bytes(bytes[18..20].try_into().expect("checked len"));
    let payload_len = u64::from_le_bytes(bytes[20..28].try_into().expect("checked len"));
    Ok((
        RawStageMeta {
            payload_schema,
            width,
            height,
            channels,
            sample,
            color_state,
        },
        payload_len,
    ))
}

/// Compresses `header ++ payload` into one checksummed zstd frame (spec
/// §3.3: "built-in xxh64 content checksum enabled").
fn compress_container(
    meta: &RawStageMeta,
    payload: &[u8],
    zstd_level: i32,
) -> std::io::Result<Vec<u8>> {
    let header = encode_header(meta, payload.len() as u64);
    let mut raw = Vec::with_capacity(HEADER_LEN + payload.len());
    raw.extend_from_slice(&header);
    raw.extend_from_slice(payload);

    let mut compressor = zstd::bulk::Compressor::new(zstd_level)?;
    compressor.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(true))?;
    compressor.compress(&raw)
}

/// Decompresses (and, via zstd's own frame checksum, verifies) one
/// container, returning its parsed header + owned payload. Every failure
/// path returns `Err(reason)` — see [`decode_header`]'s doc comment on why
/// that's a plain string, not a public error type.
fn decode_hit(compressed: &[u8]) -> Result<(RawStageMeta, PlanarBuf), String> {
    let content_size = zstd::zstd_safe::get_frame_content_size(compressed)
        .map_err(|_| "not a valid zstd frame".to_owned())?
        .ok_or_else(|| "zstd frame has no embedded content size".to_owned())?;
    if content_size < HEADER_LEN as u64 {
        return Err("decompressed content shorter than the container header".to_owned());
    }
    let mut decompressor =
        zstd::bulk::Decompressor::new().map_err(|e| format!("zstd decompressor init: {e}"))?;
    // This call verifies the frame's content checksum as part of
    // decompressing (zstd's default `forceIgnoreChecksum = false`) — a
    // single-byte flip anywhere in the compressed stream, including the
    // trailing checksum itself, surfaces here as an `Err`.
    let raw = decompressor
        .decompress(compressed, content_size as usize)
        .map_err(|e| format!("zstd decompress/checksum: {e}"))?;
    let (meta, payload_len) = decode_header(&raw[..HEADER_LEN])?;
    if raw.len() as u64 != HEADER_LEN as u64 + payload_len {
        return Err(format!(
            "payload_len mismatch: header says {payload_len}, decompressed {} bytes",
            raw.len() - HEADER_LEN
        ));
    }
    Ok((
        meta,
        PlanarBuf {
            raw,
            payload_start: HEADER_LEN,
        },
    ))
}

// ── accounting/LRU/reconcile (T18) ──────────────────────────────────────

/// [`RawCache::evict_to_cap`]'s report.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct EvictReport {
    pub evicted: u64,
    pub bytes_reclaimed: u64,
}

/// [`RawCache::purge_all`]'s report. Named distinctly from
/// `service::PurgeReport` (the preview-pyramid equivalent) since both are
/// re-exported at the crate root.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RawCachePurgeReport {
    pub rows_deleted: u64,
}

/// [`RawCache::reconcile`]'s report (spec T18 AC: "repairs BOTH divergence
/// directions").
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Catalog rows whose file no longer exists on disk — dropped.
    pub dangling_rows_dropped: u64,
    /// On-disk files with no catalog row that decoded/checksummed cleanly —
    /// re-adopted into the catalog.
    pub orphans_adopted: u64,
    /// On-disk files with no catalog row that failed to decode/checksum —
    /// deleted (unrecoverable, per spec §3.3's "a miss is always
    /// recoverable by re-decoding upstream" — there is no upstream decode
    /// to recover an orphan's producer-side inputs from here, so the only
    /// sound action is to drop it and let a future `put()` rebuild it).
    pub orphans_removed: u64,
    /// Files under `rawcache/` that don't even parse as a `<hash>.<hash>
    /// .zst` name (e.g. a leftover `.tmp-*` from a killed write) — removed
    /// outright; they can never be adopted.
    pub unrecognized_files_removed: u64,
}

/// The raw decode cache (spec §5.4): composes the T17 container primitives
/// above with catalog-backed accounting/LRU/reconcile (T18).
pub struct RawCache {
    store: Arc<Store>,
    catalog: Arc<Catalog>,
    limits: Mutex<CacheLimits>,
    zstd_level: i32,
    /// Serializes `evict_to_cap` sweeps within this process (spec T18 AC:
    /// "concurrent get/put during eviction is race-free"). Does not protect
    /// against multi-PROCESS eviction races — this store is single-process
    /// per catalog, per architecture §3.1's single-writer model.
    evict_lock: Mutex<()>,
    /// Phase F (T21): the ENOSPC pre-flight's free-space source. Injectable
    /// (see [`DiskSpaceProbe`]) so tests can simulate a full disk
    /// deterministically.
    probe: Arc<dyn DiskSpaceProbe>,
    /// Phase F (T21): notified with `PreviewEvent::CachePressure` when a
    /// `put()` had to evict to make room, or ultimately failed for lack of
    /// space. `None` (the default via [`RawCache::new`]) is a valid,
    /// silent no-op sink.
    pressure: Option<EventSink>,
    /// Bytes of headroom the pre-flight check demands beyond the payload's
    /// own compressed size — real filesystems keep reserved blocks/metadata
    /// overhead; a fixed 16 MiB margin is a simple, conservative default.
    enospc_margin_bytes: u64,
}

impl RawCache {
    pub fn new(
        store: Arc<Store>,
        catalog: Arc<Catalog>,
        limits: CacheLimits,
        zstd_level: i32,
    ) -> RawCache {
        RawCache {
            store,
            catalog,
            limits: Mutex::new(limits),
            zstd_level,
            evict_lock: Mutex::new(()),
            probe: Arc::new(RealDiskSpaceProbe),
            pressure: None,
            enospc_margin_bytes: 16 * 1024 * 1024,
        }
    }

    /// [`Self::new`] plus an injected free-space probe and an optional
    /// `CachePressure` sink (Phase F, T21). The production path
    /// (`lightbox-core`'s session wiring) uses [`Self::new`]; tests use this
    /// to simulate ENOSPC deterministically.
    pub(crate) fn with_probe(
        store: Arc<Store>,
        catalog: Arc<Catalog>,
        limits: CacheLimits,
        zstd_level: i32,
        probe: Arc<dyn DiskSpaceProbe>,
        pressure: Option<EventSink>,
    ) -> RawCache {
        RawCache {
            store,
            catalog,
            limits: Mutex::new(limits),
            zstd_level,
            evict_lock: Mutex::new(()),
            probe,
            pressure,
            enospc_margin_bytes: 16 * 1024 * 1024,
        }
    }

    /// Runtime cap update (spec §5.6 `SetCacheLimits`). Takes effect on the
    /// next `evict_to_cap`/`put` call.
    pub fn set_limits(&self, limits: CacheLimits) {
        *self.limits.lock().unwrap_or_else(PoisonError::into_inner) = limits;
    }

    fn limits(&self) -> CacheLimits {
        *self.limits.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Miss on absent OR checksum-failed (spec §5.4). Never returns `Err`
    /// for corruption — see the module doc comment. A hit best-effort
    /// touches `last_used_at`; a failure to touch (e.g. the row was already
    /// reconciled away) never turns a real hit into a miss.
    pub fn get(&self, key: &RawCacheKey) -> Result<Option<RawCacheHit>, RawCacheError> {
        let abs = self.store.resolve(&key.rel_path());
        let file = match std::fs::File::open(&abs) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(RawCacheError::Io(e)),
        };
        // An empty file (e.g. a zero-length leftover) can't be mmap'd on
        // some platforms; treat it the same as "not found" rather than
        // erroring — it is not a container anyone ever finished writing.
        let is_empty = file.metadata().map(|m| m.len() == 0).unwrap_or(false);
        if is_empty {
            self.drop_entry(key, &abs);
            return Ok(None);
        }
        let mmap = match mmap_io::map_readonly(&file) {
            Ok(m) => m,
            Err(e) => return Err(RawCacheError::Io(e)),
        };
        match decode_hit(&mmap) {
            Ok((meta, data)) => {
                self.touch_best_effort(key);
                Ok(Some(RawCacheHit { meta, data }))
            }
            Err(reason) => {
                tracing::warn!(
                    target: "lightbox_preview::rawcache",
                    content_hash = %key.content_hash.to_hex(),
                    params_hash = %format!("{:016x}", key.params_hash),
                    %reason,
                    "raw cache entry failed its integrity check; dropping"
                );
                self.drop_entry(key, &abs);
                Ok(None)
            }
        }
    }

    /// Atomic write (temp+rename) at `zstd_level` from config, then evicts
    /// to cap (spec §5.4). The write and the catalog upsert are each
    /// individually durable (`atomic_write`'s fsync+rename; the catalog's
    /// own WAL commit) but not jointly atomic across the two — if the
    /// process dies between them, [`Self::reconcile`] heals either
    /// direction (an on-disk orphan with no row, or — impossible here since
    /// the row upsert happens strictly after the file write — a dangling
    /// row).
    ///
    /// **ENOSPC pre-flight (Phase F, T21).** Before touching disk, checks
    /// [`DiskSpaceProbe::available_bytes`] against the payload's compressed
    /// size (estimated conservatively as the uncompressed size — zstd only
    /// ever shrinks) plus [`Self::enospc_margin_bytes`]. If short, evicts to
    /// cap FIRST (reclaiming space is tried before failing) and re-checks;
    /// only if STILL short does this return [`RawCacheError::DiskFull`] —
    /// never a panic, never a torn write (nothing was written yet at that
    /// point). Either the eviction or the final failure fires
    /// [`PreviewEvent::CachePressure`] on the configured sink, if any.
    pub fn put(
        &self,
        key: RawCacheKey,
        meta: RawStageMeta,
        planes: PlaneData<'_>,
    ) -> Result<(), RawCacheError> {
        let estimated_needed = planes.0.len() as u64 + self.enospc_margin_bytes;
        if let Some(available) = self.available_bytes() {
            if available < estimated_needed {
                self.notify_pressure(available);
                self.evict_to_cap();
                let available = self.available_bytes().unwrap_or(available);
                if available < estimated_needed {
                    self.notify_pressure(available);
                    return Err(RawCacheError::DiskFull {
                        needed: estimated_needed,
                        available,
                    });
                }
            }
        }

        let compressed = compress_container(&meta, planes.0, self.zstd_level)?;
        let rel = key.rel_path();
        let abs = self.store.resolve(&rel);
        atomic_write(&abs, &compressed)?;

        let bytes = compressed.len() as u64;
        let row = NewRawCacheEntryRow {
            content_hash: key.content_hash,
            params_hash: key.params_hash_be(),
            payload_schema: meta.payload_schema,
            store_path: rel.as_str().to_owned(),
            bytes,
        };
        self.catalog
            .writer()
            .with_txn(move |txn| txn.upsert_rawcache_entry(row))
            .map_err(|e| RawCacheError::Catalog(e.to_string()))?;

        self.evict_to_cap();
        Ok(())
    }

    fn available_bytes(&self) -> Option<u64> {
        self.probe.available_bytes(self.store.root()).ok()
    }

    fn notify_pressure(&self, _available: u64) {
        if let Some(sink) = &self.pressure {
            (sink)(PreviewEvent::CachePressure {
                kind: CacheKind::RawCache,
                used_bytes: self.total_bytes(),
                cap_bytes: self.limits().rawcache_cap_bytes,
            });
        }
    }

    /// Index-only existence check (spec §5.4) — trusts the catalog, never
    /// touches the filesystem.
    pub fn contains(&self, key: &RawCacheKey) -> bool {
        self.catalog
            .reader()
            .rawcache_lookup(key.content_hash, key.params_hash_be())
            .map(|r| r.is_some())
            .unwrap_or(false)
    }

    /// Sum of on-disk (compressed) bytes across every accounted entry
    /// (diagnostics/tests; `0` on any catalog read failure).
    pub fn total_bytes(&self) -> u64 {
        self.catalog.reader().rawcache_total_bytes().unwrap_or(0)
    }

    /// Accounted row count (diagnostics/tests; `0` on any catalog read
    /// failure).
    pub fn entry_count(&self) -> u64 {
        self.catalog.reader().rawcache_entry_count().unwrap_or(0)
    }

    /// Evicts LRU-oldest entries until the store is at or under
    /// [`CacheLimits::rawcache_cap_bytes`] (spec §5.4/§3.2). Called
    /// automatically after every [`Self::put`]; also safe to call directly
    /// (e.g. after [`CacheLimits`] changes).
    ///
    /// **Race guard (spec T18 AC "concurrent get/put during eviction is
    /// race-free").** Each candidate is deleted via `delete_rawcache_entry_
    /// if_unchanged` — a compare-and-delete on `last_used_at` — so a
    /// concurrent `put()`/touch that refreshed this exact entry between
    /// candidate selection and delete is never evicted out from under that
    /// fresher write; this loop simply moves on and re-selects. See that
    /// DAO method's doc comment for the one residual, self-healing race
    /// (same-wall-clock-second refresh) this does NOT close.
    pub fn evict_to_cap(&self) -> EvictReport {
        let _guard = self
            .evict_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut report = EvictReport::default();
        // Bounds the "lost every race in a row" pathological case (a
        // concurrent writer perpetually refreshing the very candidate this
        // loop keeps selecting) to a bounded amount of work per call rather
        // than spinning indefinitely — the next `put()`'s automatic
        // `evict_to_cap()` call, or a later explicit one, simply picks up
        // where this one gave up. Cap enforcement is a converging property
        // across calls, not a single-call hard guarantee under adversarial
        // concurrent load.
        let mut consecutive_races = 0u32;
        const MAX_CONSECUTIVE_RACES: u32 = 8;
        loop {
            if self.total_bytes() <= self.limits().rawcache_cap_bytes {
                break;
            }
            let Some(row) = self
                .catalog
                .reader()
                .rawcache_evict_candidates(1)
                .unwrap_or_default()
                .into_iter()
                .next()
            else {
                break; // accounting says over-cap but nothing left to evict; give up cleanly
            };
            let id = row.id;
            let expected = row.last_used_at;
            let removed = self
                .catalog
                .writer()
                .with_txn(move |txn| txn.delete_rawcache_entry_if_unchanged(id, expected))
                .unwrap_or(false);
            if !removed {
                // Raced with a concurrent refresh of this exact entry — do
                // NOT delete its file. Try again; the next candidate fetch
                // reflects the fresher `last_used_at`.
                consecutive_races += 1;
                if consecutive_races >= MAX_CONSECUTIVE_RACES {
                    break;
                }
                continue;
            }
            consecutive_races = 0;
            let abs = self.store.resolve(&RelPath(row.store_path.clone()));
            let _ = std::fs::remove_file(&abs);
            report.evicted += 1;
            report.bytes_reclaimed += row.bytes;
        }
        report
    }

    /// Deletes every raw-cache row and wipes the `rawcache/` tree (spec
    /// §5.4 `purge_all`), then recreates it empty.
    pub fn purge_all(&self) -> Result<RawCachePurgeReport, RawCacheError> {
        let rows = self
            .catalog
            .reader()
            .all_rawcache_rows()
            .map_err(|e| RawCacheError::Catalog(e.to_string()))?;
        let n = rows.len() as u64;
        let ids: Vec<_> = rows.iter().map(|r| r.id).collect();
        self.catalog
            .writer()
            .with_txn(move |txn| {
                for id in ids {
                    txn.delete_rawcache_entry(id)?;
                }
                Ok(())
            })
            .map_err(|e| RawCacheError::Catalog(e.to_string()))?;

        let dir = self.store.root().join("rawcache");
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        Ok(RawCachePurgeReport { rows_deleted: n })
    }

    /// Startup/idle reconcile scan (spec T18 AC: "repairs BOTH divergence
    /// directions"). Not called automatically anywhere in this crate —
    /// `lightbox-core`/Phase F wires it to the same idle/startup trigger the
    /// preview store's own `verify_store(Full)` uses.
    pub fn reconcile(&self) -> ReconcileReport {
        let mut report = ReconcileReport::default();

        // Direction 1: dangling catalog rows (row exists, file doesn't).
        if let Ok(rows) = self.catalog.reader().all_rawcache_rows() {
            for row in &rows {
                let abs = self.store.resolve(&RelPath(row.store_path.clone()));
                if !abs.exists() {
                    let id = row.id;
                    let _ = self
                        .catalog
                        .writer()
                        .with_txn(move |txn| txn.delete_rawcache_entry(id));
                    report.dangling_rows_dropped += 1;
                }
            }
        }

        // Direction 2: orphan/unrecognized files (file exists, row doesn't).
        let root = self.store.root().join("rawcache");
        for path in walk_files(&root) {
            let Some(key) = parse_key_from_filename(&path) else {
                let _ = std::fs::remove_file(&path);
                report.unrecognized_files_removed += 1;
                continue;
            };
            let already_indexed = self
                .catalog
                .reader()
                .rawcache_lookup(key.content_hash, key.params_hash_be())
                .map(|r| r.is_some())
                .unwrap_or(false);
            if already_indexed {
                continue;
            }
            match std::fs::read(&path)
                .ok()
                .and_then(|bytes| decode_hit(&bytes).ok())
            {
                Some((meta, _data)) => {
                    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let row = NewRawCacheEntryRow {
                        content_hash: key.content_hash,
                        params_hash: key.params_hash_be(),
                        payload_schema: meta.payload_schema,
                        store_path: key.rel_path().as_str().to_owned(),
                        bytes,
                    };
                    let adopted = self
                        .catalog
                        .writer()
                        .with_txn(move |txn| txn.upsert_rawcache_entry(row))
                        .is_ok();
                    if adopted {
                        report.orphans_adopted += 1;
                    } else {
                        let _ = std::fs::remove_file(&path);
                        report.orphans_removed += 1;
                    }
                }
                None => {
                    let _ = std::fs::remove_file(&path);
                    report.orphans_removed += 1;
                }
            }
        }

        report
    }

    fn touch_best_effort(&self, key: &RawCacheKey) {
        let content_hash = key.content_hash;
        let params_hash = key.params_hash_be();
        let _ = self
            .catalog
            .writer()
            .with_txn(move |txn| txn.touch_rawcache_last_used_by_key(content_hash, params_hash));
    }

    fn drop_entry(&self, key: &RawCacheKey, abs: &Path) {
        let _ = std::fs::remove_file(abs);
        let content_hash = key.content_hash;
        let params_hash = key.params_hash_be();
        let _ = self
            .catalog
            .writer()
            .with_txn(move |txn| txn.delete_rawcache_entry_by_key(content_hash, params_hash));
    }
}

/// `rawcache/<hh>/<hash32>.<params16>.zst` → [`RawCacheKey`]. The identity
/// lives entirely in the filename (unlike the preview pyramid's opaque
/// derived `StoreKey`), so this is exact recovery, not a heuristic — see the
/// module doc comment's key-scheme note.
fn parse_key_from_filename(path: &Path) -> Option<RawCacheKey> {
    let file_name = path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".zst")?;
    let (hash_hex, params_hex) = stem.split_once('.')?;
    let content_hash = ContentHash::from_hex(hash_hex)?;
    if params_hex.len() != 16 {
        return None;
    }
    let params_hash = u64::from_str_radix(params_hex, 16).ok()?;
    Some(RawCacheKey {
        content_hash,
        params_hash,
    })
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_files(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[cfg(test)]
mod tests;
