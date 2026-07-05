// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Demand-driven thumbnail textures for the grid (spec T25).
//!
//! Rides the [`PreviewProvider`] seam (spec §3.6): visible cells request
//! `PreviewClass::Thumb` on `Class::Background`, decoded pixels are uploaded
//! as egui-managed textures, and — the T25 acceptance criteria —
//!
//! * requests for cells that scroll out are **cancelled** (`end_frame`),
//! * concurrent duplicates never happen (one in-flight ticket per image
//!   here, plus the provider's own dedup),
//! * texture memory stays **O(visible)**: an LRU cap evicts non-visible
//!   textures beyond `cap`,
//! * a coarser thumbnail keeps displaying while a finer one decodes and is
//!   swapped **in place** when ready (placeholder → thumbnail → finer).
//!
//! Thumbnails arrive orientation-baked (spec §3.6) — cells draw them as-is;
//! the loupe's unoriented source goes through the render node instead.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use eframe::egui;
use lightbox_jobs::Class;
use lightbox_preview::{PreviewClass, PreviewProvider, PreviewState, PreviewTicket};
use lightbox_types::ImageId;

/// Thumbnail long-edge buckets (px). Requests snap up to a bucket so a
/// cell-size slider drag re-uses decodes instead of issuing one per pixel.
pub const BUCKETS: [u32; 4] = [128, 256, 512, 1024];

/// The smallest bucket covering a cell's physical long edge.
pub fn bucket_for(px: u32) -> u32 {
    for b in BUCKETS {
        if px <= b {
            return b;
        }
    }
    BUCKETS[BUCKETS.len() - 1]
}

struct Entry {
    tex: egui::TextureHandle,
    size: [u32; 2],
    bucket: u32,
    last_used: u64,
}

struct Inflight {
    ticket: PreviewTicket,
    bucket: u32,
}

/// What the grid knows about one image's thumbnail this frame.
pub enum ThumbState<'a> {
    /// Upload done: draw this texture (native size given).
    Ready(&'a egui::TextureHandle, [u32; 2]),
    /// Requested (or about to be) — draw the placeholder.
    Pending,
    /// Terminally failed (e.g. no usable embedded preview) — placeholder +
    /// badge (spec T20: previewless raws get a placeholder until E03).
    Failed(&'a str),
}

/// Counters for the debug overlay (and the T25 cancellation assertions).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ThumbStats {
    /// Ready textures held.
    pub cached: usize,
    /// In-flight preview requests.
    pub inflight: usize,
    /// Images with terminal preview failures.
    pub failed: usize,
    /// Requests issued since start.
    pub requested_total: u64,
    /// Requests cancelled since start (scroll-out).
    pub cancelled_total: u64,
}

/// Byte-bounded-by-count thumbnail texture cache over a [`PreviewProvider`].
pub struct ThumbCache {
    provider: Arc<dyn PreviewProvider>,
    entries: HashMap<ImageId, Entry>,
    inflight: HashMap<ImageId, Inflight>,
    failed: HashMap<ImageId, String>,
    tick: u64,
    requested_total: u64,
    cancelled_total: u64,
}

impl ThumbCache {
    /// A cache over the session's provider.
    pub fn new(provider: Arc<dyn PreviewProvider>) -> ThumbCache {
        ThumbCache {
            provider,
            entries: HashMap::new(),
            inflight: HashMap::new(),
            failed: HashMap::new(),
            tick: 0,
            requested_total: 0,
            cancelled_total: 0,
        }
    }

    /// Ensure a thumbnail at (at least) `bucket` exists or is on its way.
    /// One in-flight request per image; an existing coarser texture keeps
    /// displaying until the finer one lands (upgrade-in-place).
    pub fn want(&mut self, image: ImageId, bucket: u32) {
        if self.failed.contains_key(&image) || self.inflight.contains_key(&image) {
            return;
        }
        if let Some(entry) = self.entries.get(&image) {
            if entry.bucket >= bucket {
                return;
            }
        }
        let ticket = self.provider.request(
            image,
            PreviewClass::Thumb { max_px: bucket },
            Class::Background, // thumbs are Background work (spec §3.6)
        );
        self.requested_total += 1;
        self.inflight.insert(image, Inflight { ticket, bucket });
    }

    /// Per-frame: poll in-flight tickets; upload finished decodes as egui
    /// textures (replacing any coarser entry in place).
    pub fn pump(&mut self, ctx: &egui::Context) {
        type Uploaded = (egui::TextureHandle, [u32; 2], u32);
        self.tick += 1;
        let mut done: Vec<(ImageId, Result<Uploaded, String>)> = Vec::new();
        for (&image, inflight) in &self.inflight {
            match self.provider.poll(&inflight.ticket) {
                PreviewState::Pending => {}
                PreviewState::Ready(img) => {
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [img.width as usize, img.height as usize],
                        &img.px,
                    );
                    let tex = ctx.load_texture(
                        format!("thumb-{}", image.0),
                        color,
                        egui::TextureOptions::LINEAR,
                    );
                    done.push((image, Ok((tex, [img.width, img.height], inflight.bucket))));
                }
                PreviewState::Failed(err) => done.push((image, Err(err.to_string()))),
            }
        }
        for (image, outcome) in done {
            self.inflight.remove(&image);
            match outcome {
                Ok((tex, size, bucket)) => {
                    // Upgrade-in-place: replaces any coarser texture.
                    self.entries.insert(
                        image,
                        Entry {
                            tex,
                            size,
                            bucket,
                            last_used: self.tick,
                        },
                    );
                }
                Err(err) => {
                    self.failed.insert(image, err);
                }
            }
        }
    }

    /// The image's display state this frame (marks LRU use).
    pub fn state(&mut self, image: ImageId) -> ThumbState<'_> {
        if let Some(entry) = self.entries.get_mut(&image) {
            entry.last_used = self.tick;
            let size = entry.size;
            // Reborrow immutably for the caller.
            let entry = &self.entries[&image];
            return ThumbState::Ready(&entry.tex, size);
        }
        if let Some(err) = self.failed.get(&image) {
            return ThumbState::Failed(err);
        }
        ThumbState::Pending
    }

    /// End-of-frame bookkeeping (spec T25):
    /// * cancel in-flight requests whose cell scrolled out of `visible`;
    /// * evict least-recently-used non-visible textures beyond `cap`.
    pub fn end_frame(&mut self, visible: &HashSet<ImageId>, cap: usize) {
        let gone: Vec<ImageId> = self
            .inflight
            .keys()
            .filter(|id| !visible.contains(id))
            .copied()
            .collect();
        for id in gone {
            if let Some(inflight) = self.inflight.remove(&id) {
                self.provider.cancel(&inflight.ticket); // cancel-on-scroll-out
                self.cancelled_total += 1;
            }
        }

        while self.entries.len() > cap {
            let victim = self
                .entries
                .iter()
                .filter(|(id, _)| !visible.contains(id))
                .min_by_key(|(_, e)| e.last_used)
                .map(|(id, _)| *id);
            match victim {
                Some(id) => {
                    self.entries.remove(&id); // TextureHandle drop frees it
                }
                None => break, // everything left is visible
            }
        }
    }

    /// Forget terminal failures (a catalog change may have fixed them).
    pub fn clear_failures(&mut self) {
        self.failed.clear();
    }

    /// Overlay counters.
    pub fn stats(&self) -> ThumbStats {
        ThumbStats {
            cached: self.entries.len(),
            inflight: self.inflight.len(),
            failed: self.failed.len(),
            requested_total: self.requested_total,
            cancelled_total: self.cancelled_total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightbox_preview::{DecodedImage, PreviewError};
    use lightbox_types::SourceTier;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// Scriptable provider: counts requests/cancels, hands out states.
    #[derive(Default)]
    struct FakeProvider {
        next: AtomicU64,
        states: Mutex<HashMap<u64, PreviewState>>,
        requests: Mutex<Vec<(ImageId, PreviewClass, u64)>>,
        cancels: Mutex<Vec<u64>>,
    }

    impl FakeProvider {
        fn set_ready(&self, ticket: u64, w: u32, h: u32) {
            let px: Arc<[u8]> = vec![255u8; (w * h * 4) as usize].into();
            self.states.lock().unwrap().insert(
                ticket,
                PreviewState::Ready(Arc::new(DecodedImage {
                    px,
                    width: w,
                    height: h,
                    orientation_applied: true,
                    tier: SourceTier::EmbeddedPreview,
                })),
            );
        }

        fn set_failed(&self, ticket: u64) {
            self.states
                .lock()
                .unwrap()
                .insert(ticket, PreviewState::Failed(PreviewError::NoEmbedded));
        }

        fn request_count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }

        fn last_ticket(&self) -> u64 {
            self.requests.lock().unwrap().last().unwrap().2
        }
    }

    impl PreviewProvider for FakeProvider {
        fn request(&self, image: ImageId, class: PreviewClass, _prio: Class) -> PreviewTicket {
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            self.requests.lock().unwrap().push((image, class, id));
            PreviewTicket::new(id)
        }

        fn poll(&self, t: &PreviewTicket) -> PreviewState {
            self.states
                .lock()
                .unwrap()
                .get(&t.id())
                .cloned()
                .unwrap_or(PreviewState::Pending)
        }

        fn cancel(&self, t: &PreviewTicket) {
            self.cancels.lock().unwrap().push(t.id());
        }
    }

    fn setup() -> (Arc<FakeProvider>, ThumbCache, egui::Context) {
        let provider = Arc::new(FakeProvider::default());
        let cache = ThumbCache::new(Arc::clone(&provider) as Arc<dyn PreviewProvider>);
        (provider, cache, egui::Context::default())
    }

    fn visible(ids: &[i64]) -> HashSet<ImageId> {
        ids.iter().map(|&i| ImageId(i)).collect()
    }

    #[test]
    fn buckets_snap_up_and_saturate() {
        assert_eq!(bucket_for(1), 128);
        assert_eq!(bucket_for(128), 128);
        assert_eq!(bucket_for(129), 256);
        assert_eq!(bucket_for(512), 512);
        assert_eq!(bucket_for(4096), 1024);
    }

    #[test]
    fn want_is_deduped_while_inflight() {
        let (provider, mut cache, _ctx) = setup();
        cache.want(ImageId(1), 256);
        cache.want(ImageId(1), 256);
        cache.want(ImageId(1), 256);
        assert_eq!(provider.request_count(), 1, "one request per image");
        assert_eq!(cache.stats().inflight, 1);
    }

    #[test]
    fn scroll_out_cancels_inflight_requests() {
        let (provider, mut cache, ctx) = setup();
        for i in 1..=10 {
            cache.want(ImageId(i), 256);
        }
        assert_eq!(cache.stats().inflight, 10);
        cache.pump(&ctx);

        // Scroll: only 3..=12 visible now — 1 and 2 must be cancelled.
        cache.end_frame(&visible(&[3, 4, 5, 6, 7, 8, 9, 10, 11, 12]), 100);
        assert_eq!(provider.cancels.lock().unwrap().len(), 2);
        assert_eq!(cache.stats().inflight, 8);
        assert_eq!(cache.stats().cancelled_total, 2);

        // In-flight never exceeds the visible count (T25 AC).
        assert!(cache.stats().inflight <= 10);
    }

    #[test]
    fn ready_pixels_become_a_texture_in_place() {
        let (provider, mut cache, ctx) = setup();
        cache.want(ImageId(7), 128);
        provider.set_ready(provider.last_ticket(), 16, 12);
        cache.pump(&ctx);
        assert_eq!(cache.stats().inflight, 0);
        assert_eq!(cache.stats().cached, 1);
        match cache.state(ImageId(7)) {
            ThumbState::Ready(_, size) => assert_eq!(size, [16, 12]),
            _ => panic!("expected Ready"),
        }

        // Upgrade-in-place: a bigger bucket re-requests; the old texture
        // stays displayable until the finer one lands.
        cache.want(ImageId(7), 512);
        assert_eq!(provider.request_count(), 2);
        assert!(matches!(cache.state(ImageId(7)), ThumbState::Ready(..)));
        provider.set_ready(provider.last_ticket(), 64, 48);
        cache.pump(&ctx);
        assert_eq!(cache.stats().cached, 1, "replaced, not duplicated");
        match cache.state(ImageId(7)) {
            ThumbState::Ready(_, size) => assert_eq!(size, [64, 48]),
            _ => panic!("expected upgraded Ready"),
        }
        // Same bucket again: no new request.
        cache.want(ImageId(7), 512);
        assert_eq!(provider.request_count(), 2);
    }

    #[test]
    fn failures_badge_and_do_not_rerequest() {
        let (provider, mut cache, ctx) = setup();
        cache.want(ImageId(3), 256);
        provider.set_failed(provider.last_ticket());
        cache.pump(&ctx);
        assert!(matches!(cache.state(ImageId(3)), ThumbState::Failed(_)));
        cache.want(ImageId(3), 256);
        assert_eq!(
            provider.request_count(),
            1,
            "failed images not re-requested"
        );
        cache.clear_failures();
        cache.want(ImageId(3), 256);
        assert_eq!(provider.request_count(), 2, "cleared failures retry");
    }

    #[test]
    fn eviction_keeps_visible_and_caps_memory() {
        let (provider, mut cache, ctx) = setup();
        for i in 1..=5 {
            cache.want(ImageId(i), 128);
            provider.set_ready(provider.last_ticket(), 8, 8);
            cache.pump(&ctx); // pump per image → distinct last_used ticks
        }
        assert_eq!(cache.stats().cached, 5);

        // Cap 2 with image 5 visible: evictions hit the least-recently-used
        // non-visible entries (1, 2, 3).
        cache.end_frame(&visible(&[5]), 2);
        assert_eq!(cache.stats().cached, 2);
        assert!(matches!(cache.state(ImageId(5)), ThumbState::Ready(..)));
        assert!(matches!(cache.state(ImageId(4)), ThumbState::Ready(..)));
        assert!(matches!(cache.state(ImageId(1)), ThumbState::Pending));

        // A cap below the visible count never evicts visible textures.
        cache.end_frame(&visible(&[4, 5]), 0);
        assert_eq!(cache.stats().cached, 2, "visible textures survive");
    }
}
