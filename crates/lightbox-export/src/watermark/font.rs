// SPDX-FileCopyrightText: 2026 PixeLabs
// SPDX-License-Identifier: AGPL-3.0-or-later
// Additional terms under AGPL section 7 apply: see COPYING.additional-terms.

//! The built-in single-stroke watermark font.
//!
//! Every glyph is a set of polylines on an integer grid: `y = 0` is the
//! baseline, `y = 14` the cap height ([`CAP_HEIGHT`]), `y = 10` the
//! x-height, `y = -4` the descender depth. `x` starts at the glyph's left
//! edge and [`Glyph::advance`] is how far the pen moves afterwards, before
//! [`LETTER_SPACING`] is added.
//!
//! It is a *stroke* font, not an outline font: the parent module's
//! rasteriser draws each polyline with a constant-width round-capped pen.
//! That is why there is no thick/thin modulation and why round shapes are
//! polygons. See [`crate::watermark`]'s module doc comment for the honest
//! description of what the result looks like and why it is not a TrueType
//! rasteriser.
//!
//! Coverage is ASCII `0x20..=0x7E` plus `©`, `°` and `~`; [`glyph`]
//! returns a "tofu" box for everything else, so a missing character is
//! visible rather than silently dropped.

/// Cap height in grid units. A rendered cap height of `N` pixels means a
/// scale factor of `N / CAP_HEIGHT`.
pub const CAP_HEIGHT: f32 = 14.0;
/// Baseline-to-baseline distance in grid units.
pub const LINE_HEIGHT: f32 = 20.0;
/// Extra advance between glyphs, in grid units.
pub const LETTER_SPACING: f32 = 2.0;

/// One glyph: its advance width and its polylines.
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    /// Pen advance in grid units, before [`LETTER_SPACING`].
    pub advance: i8,
    /// Polylines. A one-point polyline is a dot (the tittle on `i`, a full
    /// stop) and rasterizes as a round blob of the pen's width.
    pub strokes: &'static [&'static [(i8, i8)]],
}

/// The width of one line of text in grid units (no leading or trailing
/// letter spacing). Zero for an empty line.
#[must_use]
pub fn line_width(line: &str) -> f32 {
    let mut width = 0.0f32;
    let mut any = false;
    for c in line.chars() {
        width += f32::from(glyph(c).advance) + LETTER_SPACING;
        any = true;
    }
    if any {
        (width - LETTER_SPACING).max(0.0)
    } else {
        0.0
    }
}

/// The glyph for `c`, or the tofu box when `c` is outside the covered set.
#[must_use]
pub fn glyph(c: char) -> Glyph {
    lookup(c).unwrap_or(Glyph {
        advance: 8,
        strokes: G_TOFU,
    })
}

/// True when `c` has a real glyph rather than the tofu box.
#[must_use]
pub fn has_glyph(c: char) -> bool {
    lookup(c).is_some()
}

#[allow(clippy::too_many_lines)] // one arm per glyph; a table is the point
fn lookup(c: char) -> Option<Glyph> {
    let (advance, strokes): (i8, &'static [&'static [(i8, i8)]]) = match c {
        ' ' => (5, &[]),
        '!' => (2, G_BANG),
        '"' => (4, G_DQUOTE),
        '#' => (9, G_HASH),
        '$' => (8, G_DOLLAR),
        '%' => (10, G_PERCENT),
        '&' => (9, G_AMP),
        '\'' => (1, G_QUOTE),
        '(' => (4, G_LPAREN),
        ')' => (4, G_RPAREN),
        '*' => (7, G_STAR),
        '+' => (8, G_PLUS),
        ',' => (3, G_COMMA),
        '-' => (6, G_HYPHEN),
        '.' => (2, G_PERIOD),
        '/' => (7, G_SLASH),
        '0' => (8, G_D0),
        '1' => (8, G_D1),
        '2' => (8, G_D2),
        '3' => (8, G_D3),
        '4' => (8, G_D4),
        '5' => (8, G_D5),
        '6' => (8, G_D6),
        '7' => (8, G_D7),
        '8' => (8, G_D8),
        '9' => (8, G_D9),
        ':' => (2, G_COLON),
        ';' => (3, G_SEMICOLON),
        '<' => (7, G_LT),
        '=' => (8, G_EQ),
        '>' => (7, G_GT),
        '?' => (8, G_QUESTION),
        '@' => (12, G_AT),
        'A' => (10, G_UA),
        'B' => (10, G_UB),
        'C' => (10, G_UC),
        'D' => (10, G_UD),
        'E' => (9, G_UE),
        'F' => (9, G_UF),
        'G' => (10, G_UG),
        'H' => (10, G_UH),
        'I' => (2, G_UI),
        'J' => (8, G_UJ),
        'K' => (9, G_UK),
        'L' => (8, G_UL),
        'M' => (12, G_UM),
        'N' => (10, G_UN),
        'O' => (10, G_UO),
        'P' => (10, G_UP),
        'Q' => (10, G_UQ),
        'R' => (10, G_UR),
        'S' => (9, G_US),
        'T' => (10, G_UT),
        'U' => (10, G_UU),
        'V' => (10, G_UV),
        'W' => (13, G_UW),
        'X' => (10, G_UX),
        'Y' => (10, G_UY),
        'Z' => (9, G_UZ),
        '[' => (4, G_LBRACKET),
        '\\' => (7, G_BACKSLASH),
        ']' => (4, G_RBRACKET),
        '^' => (8, G_CARET),
        '_' => (8, G_UNDERSCORE),
        '`' => (3, G_BACKTICK),
        'a' => (8, G_LA),
        'b' => (8, G_LB),
        'c' => (7, G_LC),
        'd' => (8, G_LD),
        'e' => (8, G_LE),
        'f' => (6, G_LF),
        'g' => (8, G_LG),
        'h' => (8, G_LH),
        'i' => (2, G_LI),
        'j' => (5, G_LJ),
        'k' => (7, G_LK),
        'l' => (2, G_LL),
        'm' => (12, G_LM),
        'n' => (8, G_LN),
        'o' => (8, G_LO),
        'p' => (8, G_LP),
        'q' => (8, G_LQ),
        'r' => (5, G_LR),
        's' => (7, G_LS),
        't' => (6, G_LT_),
        'u' => (8, G_LU),
        'v' => (8, G_LV),
        'w' => (11, G_LW),
        'x' => (7, G_LX),
        'y' => (8, G_LY),
        'z' => (7, G_LZ),
        '{' => (5, G_LBRACE),
        '|' => (2, G_BAR),
        '}' => (5, G_RBRACE),
        '~' => (8, G_TILDE),
        '\u{a9}' => (14, G_COPYRIGHT),
        '\u{b0}' => (5, G_DEGREE),
        _ => return None,
    };
    Some(Glyph { advance, strokes })
}

// ─── punctuation ───────────────────────────────────────────────────────────

const G_BANG: &[&[(i8, i8)]] = &[&[(1, 14), (1, 4)], &[(1, 0)]];
const G_DQUOTE: &[&[(i8, i8)]] = &[&[(0, 14), (0, 11)], &[(3, 14), (3, 11)]];
const G_HASH: &[&[(i8, i8)]] = &[
    &[(2, 0), (3, 14)],
    &[(6, 0), (7, 14)],
    &[(0, 4), (9, 4)],
    &[(0, 10), (9, 10)],
];
const G_DOLLAR: &[&[(i8, i8)]] = &[
    &[
        (8, 12),
        (5, 14),
        (2, 14),
        (0, 12),
        (0, 10),
        (2, 8),
        (6, 6),
        (8, 4),
        (8, 2),
        (5, 0),
        (2, 0),
        (0, 2),
    ],
    &[(4, 15), (4, -1)],
];
const G_PERCENT: &[&[(i8, i8)]] = &[
    &[(0, 0), (10, 14)],
    &[(0, 10), (3, 10), (3, 14), (0, 14), (0, 10)],
    &[(7, 0), (10, 0), (10, 4), (7, 4), (7, 0)],
];
const G_AMP: &[&[(i8, i8)]] = &[&[
    (9, 0),
    (2, 10),
    (2, 12),
    (4, 14),
    (6, 12),
    (6, 10),
    (0, 5),
    (0, 2),
    (2, 0),
    (5, 0),
    (9, 4),
]];
const G_QUOTE: &[&[(i8, i8)]] = &[&[(0, 14), (0, 11)]];
const G_LPAREN: &[&[(i8, i8)]] = &[&[(4, -2), (1, 4), (1, 10), (4, 16)]];
const G_RPAREN: &[&[(i8, i8)]] = &[&[(0, -2), (3, 4), (3, 10), (0, 16)]];
const G_STAR: &[&[(i8, i8)]] = &[&[(3, 14), (3, 8)], &[(0, 9), (6, 13)], &[(0, 13), (6, 9)]];
const G_PLUS: &[&[(i8, i8)]] = &[&[(0, 7), (8, 7)], &[(4, 3), (4, 11)]];
const G_COMMA: &[&[(i8, i8)]] = &[&[(2, 1), (0, -3)]];
const G_HYPHEN: &[&[(i8, i8)]] = &[&[(0, 7), (6, 7)]];
const G_PERIOD: &[&[(i8, i8)]] = &[&[(1, 0)]];
const G_SLASH: &[&[(i8, i8)]] = &[&[(0, -1), (7, 15)]];
const G_COLON: &[&[(i8, i8)]] = &[&[(1, 0)], &[(1, 7)]];
const G_SEMICOLON: &[&[(i8, i8)]] = &[&[(2, 1), (0, -3)], &[(2, 7)]];
const G_LT: &[&[(i8, i8)]] = &[&[(7, 12), (0, 7), (7, 2)]];
const G_EQ: &[&[(i8, i8)]] = &[&[(0, 5), (8, 5)], &[(0, 9), (8, 9)]];
const G_GT: &[&[(i8, i8)]] = &[&[(0, 12), (7, 7), (0, 2)]];
const G_QUESTION: &[&[(i8, i8)]] = &[
    &[(0, 11), (2, 14), (6, 14), (8, 12), (8, 9), (4, 7), (4, 4)],
    &[(4, 0)],
];
const G_AT: &[&[(i8, i8)]] = &[
    &[
        (9, 1),
        (6, 0),
        (3, 0),
        (0, 3),
        (0, 11),
        (3, 14),
        (8, 14),
        (11, 11),
        (11, 5),
        (9, 4),
        (8, 5),
        (8, 8),
    ],
    &[(8, 8), (6, 9), (4, 8), (4, 6), (6, 5), (8, 6)],
];
const G_LBRACKET: &[&[(i8, i8)]] = &[&[(4, -2), (1, -2), (1, 16), (4, 16)]];
const G_BACKSLASH: &[&[(i8, i8)]] = &[&[(0, 15), (7, -1)]];
const G_RBRACKET: &[&[(i8, i8)]] = &[&[(0, -2), (3, -2), (3, 16), (0, 16)]];
const G_CARET: &[&[(i8, i8)]] = &[&[(0, 10), (4, 14), (8, 10)]];
const G_UNDERSCORE: &[&[(i8, i8)]] = &[&[(0, -3), (8, -3)]];
const G_BACKTICK: &[&[(i8, i8)]] = &[&[(0, 15), (3, 12)]];
const G_LBRACE: &[&[(i8, i8)]] = &[&[(5, -2), (3, 0), (3, 5), (0, 7), (3, 9), (3, 14), (5, 16)]];
const G_BAR: &[&[(i8, i8)]] = &[&[(1, -2), (1, 16)]];
const G_RBRACE: &[&[(i8, i8)]] = &[&[(0, -2), (2, 0), (2, 5), (5, 7), (2, 9), (2, 14), (0, 16)]];
const G_TILDE: &[&[(i8, i8)]] = &[&[(0, 7), (2, 9), (5, 7), (7, 9)]];
const G_COPYRIGHT: &[&[(i8, i8)]] = &[
    &[
        (4, 14),
        (9, 14),
        (12, 11),
        (12, 3),
        (9, 0),
        (4, 0),
        (1, 3),
        (1, 11),
        (4, 14),
    ],
    &[
        (9, 10),
        (7, 11),
        (5, 10),
        (4, 8),
        (4, 6),
        (5, 4),
        (7, 3),
        (9, 4),
    ],
];
const G_DEGREE: &[&[(i8, i8)]] = &[&[
    (1, 14),
    (3, 14),
    (4, 12),
    (3, 10),
    (1, 10),
    (0, 12),
    (1, 14),
]];

// ─── digits ────────────────────────────────────────────────────────────────

const G_D0: &[&[(i8, i8)]] = &[&[
    (2, 14),
    (6, 14),
    (8, 11),
    (8, 3),
    (6, 0),
    (2, 0),
    (0, 3),
    (0, 11),
    (2, 14),
]];
const G_D1: &[&[(i8, i8)]] = &[&[(1, 11), (4, 14), (4, 0)]];
const G_D2: &[&[(i8, i8)]] = &[&[(0, 11), (2, 14), (6, 14), (8, 12), (8, 9), (0, 0), (8, 0)]];
const G_D3: &[&[(i8, i8)]] = &[&[
    (0, 13),
    (3, 14),
    (6, 14),
    (8, 12),
    (8, 10),
    (5, 7),
    (8, 4),
    (8, 2),
    (6, 0),
    (3, 0),
    (0, 1),
]];
const G_D4: &[&[(i8, i8)]] = &[&[(6, 0), (6, 14), (0, 4), (8, 4)]];
const G_D5: &[&[(i8, i8)]] = &[&[
    (8, 14),
    (1, 14),
    (0, 7),
    (4, 8),
    (7, 7),
    (8, 4),
    (6, 0),
    (3, 0),
    (0, 1),
]];
const G_D6: &[&[(i8, i8)]] = &[&[
    (7, 13),
    (4, 14),
    (1, 11),
    (0, 5),
    (2, 0),
    (5, 0),
    (8, 2),
    (8, 5),
    (5, 7),
    (2, 6),
    (0, 4),
]];
const G_D7: &[&[(i8, i8)]] = &[&[(0, 14), (8, 14), (3, 0)]];
const G_D8: &[&[(i8, i8)]] = &[&[
    (3, 7),
    (1, 9),
    (1, 12),
    (3, 14),
    (5, 14),
    (7, 12),
    (7, 9),
    (5, 7),
    (3, 7),
    (0, 5),
    (0, 2),
    (3, 0),
    (5, 0),
    (8, 2),
    (8, 5),
    (5, 7),
]];
const G_D9: &[&[(i8, i8)]] = &[&[
    (1, 1),
    (4, 0),
    (7, 3),
    (8, 9),
    (6, 14),
    (3, 14),
    (0, 12),
    (0, 9),
    (3, 7),
    (6, 8),
    (8, 10),
]];

// ─── uppercase ─────────────────────────────────────────────────────────────

const G_UA: &[&[(i8, i8)]] = &[&[(0, 0), (5, 14), (10, 0)], &[(2, 5), (8, 5)]];
const G_UB: &[&[(i8, i8)]] = &[
    &[(0, 0), (0, 14), (6, 14), (9, 12), (9, 9), (6, 7), (0, 7)],
    &[(6, 7), (9, 5), (9, 2), (6, 0), (0, 0)],
];
const G_UC: &[&[(i8, i8)]] = &[&[
    (9, 12),
    (7, 14),
    (3, 14),
    (0, 11),
    (0, 3),
    (3, 0),
    (7, 0),
    (9, 2),
]];
const G_UD: &[&[(i8, i8)]] = &[&[(0, 0), (0, 14), (5, 14), (9, 11), (9, 3), (5, 0), (0, 0)]];
const G_UE: &[&[(i8, i8)]] = &[&[(9, 14), (0, 14), (0, 0), (9, 0)], &[(0, 7), (7, 7)]];
const G_UF: &[&[(i8, i8)]] = &[&[(9, 14), (0, 14), (0, 0)], &[(0, 7), (7, 7)]];
const G_UG: &[&[(i8, i8)]] = &[&[
    (9, 12),
    (7, 14),
    (3, 14),
    (0, 11),
    (0, 3),
    (3, 0),
    (7, 0),
    (10, 3),
    (10, 6),
    (6, 6),
]];
const G_UH: &[&[(i8, i8)]] = &[&[(0, 0), (0, 14)], &[(10, 0), (10, 14)], &[(0, 7), (10, 7)]];
const G_UI: &[&[(i8, i8)]] = &[&[(1, 0), (1, 14)]];
const G_UJ: &[&[(i8, i8)]] = &[&[(8, 14), (8, 4), (6, 0), (3, 0), (0, 3)]];
const G_UK: &[&[(i8, i8)]] = &[&[(0, 0), (0, 14)], &[(9, 14), (0, 7)], &[(3, 9), (9, 0)]];
const G_UL: &[&[(i8, i8)]] = &[&[(0, 14), (0, 0), (8, 0)]];
const G_UM: &[&[(i8, i8)]] = &[&[(0, 0), (0, 14), (6, 5), (12, 14), (12, 0)]];
const G_UN: &[&[(i8, i8)]] = &[&[(0, 0), (0, 14), (10, 0), (10, 14)]];
const G_UO: &[&[(i8, i8)]] = &[&[
    (3, 14),
    (7, 14),
    (10, 11),
    (10, 3),
    (7, 0),
    (3, 0),
    (0, 3),
    (0, 11),
    (3, 14),
]];
const G_UP: &[&[(i8, i8)]] = &[&[(0, 0), (0, 14), (6, 14), (9, 12), (9, 9), (6, 7), (0, 7)]];
const G_UQ: &[&[(i8, i8)]] = &[
    &[
        (3, 14),
        (7, 14),
        (10, 11),
        (10, 3),
        (7, 0),
        (3, 0),
        (0, 3),
        (0, 11),
        (3, 14),
    ],
    &[(6, 3), (10, -1)],
];
const G_UR: &[&[(i8, i8)]] = &[
    &[(0, 0), (0, 14), (6, 14), (9, 12), (9, 9), (6, 7), (0, 7)],
    &[(5, 7), (9, 0)],
];
const G_US: &[&[(i8, i8)]] = &[&[
    (9, 12),
    (6, 14),
    (3, 14),
    (0, 12),
    (0, 9),
    (3, 7),
    (6, 7),
    (9, 5),
    (9, 2),
    (6, 0),
    (3, 0),
    (0, 2),
]];
const G_UT: &[&[(i8, i8)]] = &[&[(0, 14), (10, 14)], &[(5, 14), (5, 0)]];
const G_UU: &[&[(i8, i8)]] = &[&[(0, 14), (0, 3), (3, 0), (7, 0), (10, 3), (10, 14)]];
const G_UV: &[&[(i8, i8)]] = &[&[(0, 14), (5, 0), (10, 14)]];
const G_UW: &[&[(i8, i8)]] = &[&[(0, 14), (3, 0), (6, 9), (10, 0), (13, 14)]];
const G_UX: &[&[(i8, i8)]] = &[&[(0, 0), (10, 14)], &[(0, 14), (10, 0)]];
const G_UY: &[&[(i8, i8)]] = &[&[(0, 14), (5, 7), (10, 14)], &[(5, 7), (5, 0)]];
const G_UZ: &[&[(i8, i8)]] = &[&[(0, 14), (9, 14), (0, 0), (9, 0)]];

// ─── lowercase ─────────────────────────────────────────────────────────────

const G_LA: &[&[(i8, i8)]] = &[
    &[(7, 10), (7, 0)],
    &[
        (7, 7),
        (5, 10),
        (2, 10),
        (0, 7),
        (0, 3),
        (2, 0),
        (5, 0),
        (7, 3),
    ],
];
const G_LB: &[&[(i8, i8)]] = &[
    &[(0, 14), (0, 0)],
    &[
        (0, 7),
        (2, 10),
        (5, 10),
        (7, 7),
        (7, 3),
        (5, 0),
        (2, 0),
        (0, 3),
    ],
];
const G_LC: &[&[(i8, i8)]] = &[&[
    (7, 8),
    (5, 10),
    (2, 10),
    (0, 7),
    (0, 3),
    (2, 0),
    (5, 0),
    (7, 2),
]];
const G_LD: &[&[(i8, i8)]] = &[
    &[(7, 14), (7, 0)],
    &[
        (7, 7),
        (5, 10),
        (2, 10),
        (0, 7),
        (0, 3),
        (2, 0),
        (5, 0),
        (7, 3),
    ],
];
const G_LE: &[&[(i8, i8)]] = &[&[
    (0, 5),
    (7, 5),
    (7, 8),
    (5, 10),
    (2, 10),
    (0, 7),
    (0, 3),
    (2, 0),
    (5, 0),
    (7, 2),
]];
const G_LF: &[&[(i8, i8)]] = &[&[(2, 0), (2, 12), (4, 14), (6, 13)], &[(0, 9), (5, 9)]];
const G_LG: &[&[(i8, i8)]] = &[
    &[(7, 10), (7, -1), (5, -4), (2, -4), (0, -3)],
    &[
        (7, 7),
        (5, 10),
        (2, 10),
        (0, 7),
        (0, 3),
        (2, 0),
        (5, 0),
        (7, 3),
    ],
];
const G_LH: &[&[(i8, i8)]] = &[
    &[(0, 14), (0, 0)],
    &[(0, 7), (2, 10), (5, 10), (7, 8), (7, 0)],
];
const G_LI: &[&[(i8, i8)]] = &[&[(1, 10), (1, 0)], &[(1, 13)]];
const G_LJ: &[&[(i8, i8)]] = &[&[(4, 10), (4, -1), (2, -4), (0, -3)], &[(4, 13)]];
const G_LK: &[&[(i8, i8)]] = &[&[(0, 14), (0, 0)], &[(6, 10), (0, 4)], &[(2, 6), (6, 0)]];
const G_LL: &[&[(i8, i8)]] = &[&[(1, 14), (1, 0)]];
const G_LM: &[&[(i8, i8)]] = &[
    &[(0, 10), (0, 0)],
    &[(0, 7), (2, 10), (4, 10), (6, 8), (6, 0)],
    &[(6, 7), (8, 10), (10, 10), (12, 8), (12, 0)],
];
const G_LN: &[&[(i8, i8)]] = &[
    &[(0, 10), (0, 0)],
    &[(0, 7), (2, 10), (5, 10), (7, 8), (7, 0)],
];
const G_LO: &[&[(i8, i8)]] = &[&[
    (2, 10),
    (5, 10),
    (7, 7),
    (7, 3),
    (5, 0),
    (2, 0),
    (0, 3),
    (0, 7),
    (2, 10),
]];
const G_LP: &[&[(i8, i8)]] = &[
    &[(0, 10), (0, -4)],
    &[
        (0, 7),
        (2, 10),
        (5, 10),
        (7, 7),
        (7, 3),
        (5, 0),
        (2, 0),
        (0, 3),
    ],
];
const G_LQ: &[&[(i8, i8)]] = &[
    &[(7, 10), (7, -4)],
    &[
        (7, 7),
        (5, 10),
        (2, 10),
        (0, 7),
        (0, 3),
        (2, 0),
        (5, 0),
        (7, 3),
    ],
];
const G_LR: &[&[(i8, i8)]] = &[&[(0, 10), (0, 0)], &[(0, 6), (2, 9), (5, 10)]];
const G_LS: &[&[(i8, i8)]] = &[&[
    (7, 9),
    (5, 10),
    (2, 10),
    (0, 8),
    (1, 6),
    (6, 4),
    (7, 2),
    (5, 0),
    (2, 0),
    (0, 1),
]];
const G_LT_: &[&[(i8, i8)]] = &[&[(2, 14), (2, 2), (4, 0), (6, 1)], &[(0, 10), (5, 10)]];
const G_LU: &[&[(i8, i8)]] = &[
    &[(0, 10), (0, 3), (2, 0), (5, 0), (7, 3)],
    &[(7, 10), (7, 0)],
];
const G_LV: &[&[(i8, i8)]] = &[&[(0, 10), (4, 0), (8, 10)]];
const G_LW: &[&[(i8, i8)]] = &[&[(0, 10), (3, 0), (5, 7), (8, 0), (11, 10)]];
const G_LX: &[&[(i8, i8)]] = &[&[(0, 0), (7, 10)], &[(0, 10), (7, 0)]];
const G_LY: &[&[(i8, i8)]] = &[&[(0, 10), (4, 1)], &[(8, 10), (2, -4)]];
const G_LZ: &[&[(i8, i8)]] = &[&[(0, 10), (7, 10), (0, 0), (7, 0)]];

/// The "missing glyph" box, deliberately visible.
const G_TOFU: &[&[(i8, i8)]] = &[&[(0, 0), (8, 0), (8, 13), (0, 13), (0, 0)]];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_printable_ascii_has_a_real_glyph() {
        for byte in 0x20u8..=0x7e {
            let c = char::from(byte);
            assert!(has_glyph(c), "no glyph for {c:?} (0x{byte:02x})");
        }
    }

    #[test]
    fn the_symbols_a_photographer_needs_are_covered() {
        for c in ['\u{a9}', '\u{b0}', '~', '@', '/', '.', '-'] {
            assert!(has_glyph(c), "no glyph for {c:?}");
        }
    }

    #[test]
    fn an_uncovered_character_falls_back_to_tofu_not_to_nothing() {
        assert!(!has_glyph('\u{4e2d}'));
        let g = glyph('\u{4e2d}');
        assert!(!g.strokes.is_empty(), "tofu must be visible");
        assert!(g.advance > 0);
    }

    #[test]
    fn every_glyph_fits_the_grid_it_claims() {
        for byte in 0x20u8..=0x7e {
            let c = char::from(byte);
            let g = glyph(c);
            assert!(g.advance >= 0, "{c:?} has a negative advance");
            for stroke in g.strokes {
                assert!(!stroke.is_empty(), "{c:?} has an empty polyline");
                for &(x, y) in *stroke {
                    // A generous box: descenders reach -4, brackets and the
                    // dollar's stem reach 16, and no glyph is wider than
                    // its advance plus a unit of overhang.
                    assert!(
                        (-4..=16).contains(&y),
                        "{c:?} point ({x},{y}) is outside the vertical grid"
                    );
                    assert!(
                        (0..=i16::from(g.advance) + 2).contains(&i16::from(x)),
                        "{c:?} point ({x},{y}) is outside its advance of {}",
                        g.advance
                    );
                }
            }
        }
    }

    #[test]
    fn line_width_sums_advances_with_spacing_between_but_not_after() {
        assert_eq!(line_width(""), 0.0);
        let i_advance = f32::from(glyph('I').advance);
        assert!((line_width("I") - i_advance).abs() < 1e-6);
        assert!((line_width("II") - (2.0 * i_advance + LETTER_SPACING)).abs() < 1e-6);
    }

    #[test]
    fn space_advances_without_drawing() {
        let g = glyph(' ');
        assert!(g.strokes.is_empty());
        assert!(g.advance > 0);
    }
}
