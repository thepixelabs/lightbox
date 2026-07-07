/* SPDX-FileCopyrightText: 2026 Lightbox contributors
 * SPDX-License-Identifier: Apache-2.0
 *
 * E03 Phase C, T11: the bindgen entry point for the libjxl encoder FFI
 * surface (feature `jxl`, build.rs). Pulls in exactly the two public
 * libjxl headers the minimal one-shot encode path needs — the basic
 * pixel/type vocabulary (jxl/types.h) and the encoder API itself
 * (jxl/encode.h, which itself includes jxl/codestream_header.h for
 * JxlBasicInfo/JxlOrientation etc.). Resolved via the C compiler's normal
 * include search path plus `-I$LIBJXL_INCLUDE_DIR` when that env var is
 * set (see build.rs). UNVERIFIED in this repository's environment — no
 * libjxl installed here to bindgen against (E03-deviations.md, Phase C
 * T11).
 */
#include <jxl/types.h>
#include <jxl/encode.h>
