// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
//
// LibRaw C shim (E02 Phase C, task C2). Compiled + linked ONLY under the
// `libraw` cargo feature; the default build never touches LibRaw (which is
// LGPL-2.1 and lives out-of-process in this sandbox, never in an app binary).
//
// This file exposes a small, FLAT C API of our own design over LibRaw's C
// entry points. The point is FFI stability: the Rust side declares only the
// `lbx_lr_*` functions below, whose signatures we control, instead of mirroring
// LibRaw's large `libraw_data_t` struct (whose layout drifts across releases).
// The C compiler, which includes the real headers, reads the struct fields.

#include <libraw/libraw.h>
#include <string.h>

// dcraw-style CFA color at (row, col) for a Bayer `filters` code (0..3).
static int lbx_fc(unsigned filters, int row, int col) {
  return (filters >> ((((row << 1) & 14) | (col & 1)) << 1)) & 3;
}

const char *lbx_lr_version(void) { return libraw_version(); }

void *lbx_lr_init(void) { return (void *)libraw_init(0); }

void lbx_lr_free(void *h) {
  if (h) {
    libraw_close((libraw_data_t *)h);
  }
}

// Open + unpack the raw plane. Returns the LibRaw error code (0 == success).
int lbx_lr_open_unpack(void *h, const char *path) {
  libraw_data_t *lr = (libraw_data_t *)h;
  int rc = libraw_open_file(lr, path);
  if (rc != LIBRAW_SUCCESS) {
    return rc;
  }
  return libraw_unpack(lr);
}

typedef struct {
  unsigned raw_width, raw_height;   // full raw plane
  unsigned width, height;           // visible/active area size
  unsigned top_margin, left_margin; // active area origin in the raw plane
  unsigned iwidth, iheight;         // default-crop output dims
  unsigned filters;                 // CFA code (9 => X-Trans, 0 => full-color)
  int colors;                       // channel count (1 => mono)
  int is_xtrans;                    // filters == 9
  signed char xtrans[36];           // row-major 6x6 X-Trans color indices
  unsigned black_pos[4];            // effective black per 2x2 tile position
  unsigned white;                   // saturation level (maximum)
  signed char cfa2x2[4];            // Bayer color index (0..3) per tile position
  char cdesc[8];                    // color descriptor, e.g. "RGBG"
  float cam_mul[4];                 // as-shot camera multipliers
  float cam_xyz[9];                 // XYZ->camera (ColorMatrix1), row-major 3x3
  int flip;                         // LibRaw flip code
  char make[64];
  char model[64];
} lbx_meta;

// Fills `m` with the mosaic/color metadata. Returns 0 on success.
int lbx_lr_get_meta(void *h, lbx_meta *m) {
  libraw_data_t *lr = (libraw_data_t *)h;
  if (!lr || !m) {
    return -1;
  }
  memset(m, 0, sizeof(*m));

  m->raw_width = lr->sizes.raw_width;
  m->raw_height = lr->sizes.raw_height;
  m->width = lr->sizes.width;
  m->height = lr->sizes.height;
  m->top_margin = lr->sizes.top_margin;
  m->left_margin = lr->sizes.left_margin;
  m->iwidth = lr->sizes.iwidth;
  m->iheight = lr->sizes.iheight;
  m->flip = lr->sizes.flip;

  m->filters = lr->idata.filters;
  m->colors = lr->idata.colors;
  m->is_xtrans = (lr->idata.filters == 9) ? 1 : 0;
  memcpy(m->cdesc, lr->idata.cdesc, sizeof(m->cdesc) < sizeof(lr->idata.cdesc)
                                        ? sizeof(m->cdesc)
                                        : sizeof(lr->idata.cdesc));
  m->cdesc[7] = 0;
  memcpy(m->make, lr->idata.make, 63);
  m->make[63] = 0;
  memcpy(m->model, lr->idata.model, 63);
  m->model[63] = 0;

  for (int r = 0; r < 6; r++) {
    for (int c = 0; c < 6; c++) {
      m->xtrans[r * 6 + c] = (signed char)lr->idata.xtrans[r][c];
    }
  }

  m->white = lr->color.maximum;
  for (int i = 0; i < 4; i++) {
    m->cam_mul[i] = lr->color.cam_mul[i];
  }
  for (int r = 0; r < 3; r++) {
    for (int c = 0; c < 3; c++) {
      m->cam_xyz[r * 3 + c] = lr->color.cam_xyz[r][c];
    }
  }

  // Effective black per 2x2 CFA tile position (r in {0,1}, c in {0,1}).
  unsigned base = lr->color.black;
  int have_block = (lr->color.cblack[4] == 2 && lr->color.cblack[5] == 2);
  for (int pos = 0; pos < 4; pos++) {
    int r = pos / 2, c = pos % 2;
    int color;
    if (m->is_xtrans) {
      color = lr->idata.xtrans[r][c];
    } else if (m->filters == 0) {
      color = 0; // mono / full-color: single black
    } else {
      color = lbx_fc(m->filters, r, c);
    }
    if (color < 0 || color > 3) {
      color = 0;
    }
    unsigned bl = base + lr->color.cblack[color];
    if (have_block) {
      bl += lr->color.cblack[6 + (r % 2) * 2 + (c % 2)];
    }
    m->black_pos[pos] = bl;
    m->cfa2x2[pos] = (signed char)color;
  }

  return 0;
}

// The unpacked raw mosaic plane (raw_width*raw_height u16 samples). NULL if the
// decoder produced no single-plane mosaic (e.g. Foveon full-color).
const unsigned short *lbx_lr_raw_image(void *h) {
  libraw_data_t *lr = (libraw_data_t *)h;
  return lr ? lr->rawdata.raw_image : NULL;
}

// Configure LibRaw for the interim develop path (spec §3.2): AHD demosaic,
// 16-bit linear output, RAW camera color (no output-space conversion), no auto
// bright, NO white balance (user_mul = 1,1,1,1), then run the pipeline. Returns
// the LibRaw error code (0 == success).
int lbx_lr_process_ahd(void *h) {
  libraw_data_t *lr = (libraw_data_t *)h;
  libraw_set_demosaic(lr, 3);     // AHD
  libraw_set_output_bps(lr, 16);  // 16-bit linear
  libraw_set_output_color(lr, 0); // raw camera color
  libraw_set_gamma(lr, 0, 1.0);   // linear TRC
  libraw_set_gamma(lr, 1, 1.0);
  libraw_set_no_auto_bright(lr, 1);
  libraw_set_highlight(lr, 0); // clip highlights (deterministic)
  libraw_set_user_mul(lr, 0, 1.0f);
  libraw_set_user_mul(lr, 1, 1.0f);
  libraw_set_user_mul(lr, 2, 1.0f);
  libraw_set_user_mul(lr, 3, 1.0f);
  // Belt-and-suspenders: never let auto/camera WB re-enter.
  lr->params.use_camera_wb = 0;
  lr->params.use_auto_wb = 0;
  // Emit in sensor orientation (no rotation); the EXIF orientation travels in
  // metadata so both the mosaic and demosaiced paths are oriented downstream
  // identically.
  lr->params.user_flip = 0;
  return libraw_dcraw_process(lr);
}

void *lbx_lr_make_mem_image(void *h, int *errc) {
  return (void *)libraw_dcraw_make_mem_image((libraw_data_t *)h, errc);
}

void lbx_lr_image_dims(void *pimg, unsigned *w, unsigned *ht, int *colors,
                       int *bits, unsigned *data_size) {
  libraw_processed_image_t *img = (libraw_processed_image_t *)pimg;
  if (w)
    *w = img->width;
  if (ht)
    *ht = img->height;
  if (colors)
    *colors = img->colors;
  if (bits)
    *bits = img->bits;
  if (data_size)
    *data_size = img->data_size;
}

const unsigned char *lbx_lr_image_bytes(void *pimg) {
  return ((libraw_processed_image_t *)pimg)->data;
}

void lbx_lr_clear_mem(void *pimg) {
  libraw_dcraw_clear_mem((libraw_processed_image_t *)pimg);
}

const char *lbx_lr_strerror(int code) { return libraw_strerror(code); }
