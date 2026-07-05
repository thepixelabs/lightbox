// SPDX-FileCopyrightText: 2026 Lightbox contributors
// SPDX-License-Identifier: Apache-2.0

//! Safe-ish wrapper over the `src/libraw_shim.c` flat C API (E02 Phase C, C2).
//!
//! Only compiled under the `libraw` feature. The `extern "C"` surface is our
//! own `lbx_lr_*` shim — NOT LibRaw's `libraw_data_t` struct — so the FFI stays
//! stable across LibRaw releases (see `libraw_shim.c` for the rationale).
//!
//! LibRaw is memory-unsafe C++ decoding untrusted input; it runs here in the
//! out-of-process sandbox only. Every pointer returned by the shim is either
//! owned by the `libraw_data_t` handle (raw plane) or by a processed-image
//! object we free explicitly, so lifetimes are bounded by [`RawFile`] / the
//! `process_ahd` call.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};

/// Mirror of the C `lbx_meta` struct. `#[repr(C)]` + identical field order/types
/// makes the layout match what the C compiler produced. Some trailing fields
/// (`make`/`model`) exist only to preserve the layout — camera identity comes
/// from `probe()` on the client, so they are read by the C side, not here.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
struct LbxMeta {
    raw_width: c_uint,
    raw_height: c_uint,
    width: c_uint,
    height: c_uint,
    top_margin: c_uint,
    left_margin: c_uint,
    iwidth: c_uint,
    iheight: c_uint,
    filters: c_uint,
    colors: c_int,
    is_xtrans: c_int,
    xtrans: [i8; 36],
    black_pos: [c_uint; 4],
    white: c_uint,
    cfa2x2: [i8; 4],
    cdesc: [c_char; 8],
    cam_mul: [f32; 4],
    cam_xyz: [f32; 9],
    flip: c_int,
    make: [c_char; 64],
    model: [c_char; 64],
}

extern "C" {
    fn lbx_lr_version() -> *const c_char;
    fn lbx_lr_init() -> *mut c_void;
    fn lbx_lr_free(h: *mut c_void);
    fn lbx_lr_open_unpack(h: *mut c_void, path: *const c_char) -> c_int;
    fn lbx_lr_get_meta(h: *mut c_void, m: *mut LbxMeta) -> c_int;
    fn lbx_lr_raw_image(h: *mut c_void) -> *const u16;
    fn lbx_lr_process_ahd(h: *mut c_void) -> c_int;
    fn lbx_lr_make_mem_image(h: *mut c_void, errc: *mut c_int) -> *mut c_void;
    fn lbx_lr_image_dims(
        pimg: *mut c_void,
        w: *mut c_uint,
        ht: *mut c_uint,
        colors: *mut c_int,
        bits: *mut c_int,
        data_size: *mut c_uint,
    );
    fn lbx_lr_image_bytes(pimg: *mut c_void) -> *const u8;
    fn lbx_lr_clear_mem(pimg: *mut c_void);
    fn lbx_lr_strerror(code: c_int) -> *const c_char;
}

/// The linked LibRaw version string (recorded in provenance).
pub fn version() -> String {
    // SAFETY: `libraw_version()` returns a static NUL-terminated string.
    unsafe {
        let p = lbx_lr_version();
        if p.is_null() {
            return "unknown".to_owned();
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn strerror(code: c_int) -> String {
    // SAFETY: `libraw_strerror` returns a static NUL-terminated string.
    unsafe {
        let p = lbx_lr_strerror(code);
        if p.is_null() {
            format!("libraw error {code}")
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

fn cstr_field(bytes: &[c_char]) -> String {
    let raw: Vec<u8> = bytes
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&raw).into_owned()
}

/// Decoded metadata, translated from the C struct into plain Rust.
#[derive(Clone, Debug)]
pub struct Meta {
    pub raw_width: u32,
    pub raw_height: u32,
    /// Active-area rect `(x, y, w, h)` in the raw plane.
    pub active: (u32, u32, u32, u32),
    /// Default-crop output dims `(w, h)`.
    pub crop: (u32, u32),
    pub filters: u32,
    pub colors: i32,
    pub is_xtrans: bool,
    pub xtrans: [i8; 36],
    /// Effective black per 2x2 CFA tile position.
    pub black_pos: [u32; 4],
    /// Saturation (white) level.
    pub white: u32,
    /// Bayer color index (0..3) per 2x2 tile position.
    pub cfa2x2: [i8; 4],
    /// Color descriptor, e.g. `"RGBG"`.
    pub cdesc: String,
    /// As-shot camera multipliers (R, G, B, G2).
    pub cam_mul: [f32; 4],
    /// XYZ→camera 3x3 (ColorMatrix1 semantics), row-major.
    pub cam_xyz: [[f32; 3]; 3],
    /// LibRaw flip code.
    pub flip: i32,
}

/// An opened + unpacked raw file. Drops the LibRaw handle on `Drop`.
pub struct RawFile {
    handle: *mut c_void,
}

impl RawFile {
    /// Opens and unpacks a raw file. On any LibRaw failure returns a
    /// human-readable error (the supervisor maps it to a structured
    /// `DecodeError`).
    pub fn open(path: &std::path::Path) -> Result<RawFile, String> {
        let c_path = CString::new(path.as_os_str().to_string_lossy().as_bytes())
            .map_err(|_| "path contains an interior NUL".to_owned())?;
        // SAFETY: init returns a valid handle or null; we check both here.
        let handle = unsafe { lbx_lr_init() };
        if handle.is_null() {
            return Err("libraw_init returned null".to_owned());
        }
        let file = RawFile { handle };
        // SAFETY: `handle` is valid; `c_path` is NUL-terminated.
        let rc = unsafe { lbx_lr_open_unpack(file.handle, c_path.as_ptr()) };
        if rc != 0 {
            return Err(format!("open/unpack failed: {}", strerror(rc)));
        }
        Ok(file)
    }

    /// Reads the mosaic/color metadata.
    pub fn meta(&self) -> Result<Meta, String> {
        let mut m = std::mem::MaybeUninit::<LbxMeta>::uninit();
        // SAFETY: handle is valid; `m` is a writable LbxMeta the shim fills.
        let rc = unsafe { lbx_lr_get_meta(self.handle, m.as_mut_ptr()) };
        if rc != 0 {
            return Err("libraw metadata unavailable".to_owned());
        }
        // SAFETY: rc == 0 means the shim memset+filled the struct.
        let m = unsafe { m.assume_init() };
        let cam_xyz = [
            [m.cam_xyz[0], m.cam_xyz[1], m.cam_xyz[2]],
            [m.cam_xyz[3], m.cam_xyz[4], m.cam_xyz[5]],
            [m.cam_xyz[6], m.cam_xyz[7], m.cam_xyz[8]],
        ];
        Ok(Meta {
            raw_width: m.raw_width,
            raw_height: m.raw_height,
            active: (m.left_margin, m.top_margin, m.width, m.height),
            crop: (m.iwidth, m.iheight),
            filters: m.filters,
            colors: m.colors,
            is_xtrans: m.is_xtrans != 0,
            xtrans: m.xtrans,
            black_pos: m.black_pos,
            white: m.white,
            cfa2x2: m.cfa2x2,
            cdesc: cstr_field(&m.cdesc),
            cam_mul: m.cam_mul,
            cam_xyz,
            flip: m.flip,
        })
    }

    /// Copies the unpacked raw mosaic plane (`raw_width * raw_height` `u16`
    /// samples). Fails for images with no single-plane mosaic (e.g. Foveon).
    pub fn raw_plane(&self, raw_width: u32, raw_height: u32) -> Result<Vec<u16>, String> {
        // SAFETY: handle is valid; the returned pointer is owned by the handle.
        let ptr = unsafe { lbx_lr_raw_image(self.handle) };
        if ptr.is_null() {
            return Err("no raw mosaic plane (unsupported sensor layout)".to_owned());
        }
        let len = (raw_width as usize)
            .checked_mul(raw_height as usize)
            .ok_or_else(|| "raw plane dimensions overflow".to_owned())?;
        // SAFETY: LibRaw allocated `raw_width*raw_height` u16 samples for a
        // single-plane mosaic; `ptr` is valid for that many reads.
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        Ok(slice.to_vec())
    }

    /// Runs the interim AHD develop path and returns
    /// `(width, height, channels, samples)` — 16-bit linear camera-native RGB
    /// (`samples.len() == width * height * channels`).
    pub fn process_ahd(&self) -> Result<(u32, u32, u8, Vec<u16>), String> {
        // SAFETY: handle is valid.
        let rc = unsafe { lbx_lr_process_ahd(self.handle) };
        if rc != 0 {
            return Err(format!("dcraw_process failed: {}", strerror(rc)));
        }
        let mut errc: c_int = 0;
        // SAFETY: handle is valid; `errc` is writable.
        let pimg = unsafe { lbx_lr_make_mem_image(self.handle, &mut errc) };
        if pimg.is_null() {
            return Err(format!("make_mem_image failed: {}", strerror(errc)));
        }
        // Ensure the processed image is always freed.
        let img = ProcessedImage { ptr: pimg };
        let (mut w, mut h, mut colors, mut bits, mut data_size) = (0u32, 0u32, 0i32, 0i32, 0u32);
        // SAFETY: `img.ptr` is a valid processed-image object.
        unsafe {
            lbx_lr_image_dims(
                img.ptr,
                &mut w,
                &mut h,
                &mut colors,
                &mut bits,
                &mut data_size,
            );
        }
        if bits != 16 {
            return Err(format!("expected 16-bit output, got {bits}-bit"));
        }
        if !(1..=4).contains(&colors) {
            return Err(format!("unexpected channel count {colors}"));
        }
        let expected = (w as usize) * (h as usize) * (colors as usize) * 2;
        if data_size as usize != expected {
            return Err(format!(
                "processed-image size {data_size} != expected {expected}"
            ));
        }
        // SAFETY: `data_size` bytes are valid at `img_bytes`.
        let bytes =
            unsafe { std::slice::from_raw_parts(lbx_lr_image_bytes(img.ptr), data_size as usize) };
        let samples: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_ne_bytes([c[0], c[1]]))
            .collect();
        Ok((w, h, colors as u8, samples))
    }
}

impl Drop for RawFile {
    fn drop(&mut self) {
        // SAFETY: handle was produced by `lbx_lr_init` and not yet freed.
        unsafe { lbx_lr_free(self.handle) };
    }
}

/// RAII guard freeing a LibRaw processed-image allocation.
struct ProcessedImage {
    ptr: *mut c_void,
}

impl Drop for ProcessedImage {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `make_mem_image` and is freed exactly once.
        unsafe { lbx_lr_clear_mem(self.ptr) };
    }
}
