#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 PixeLabs
# SPDX-License-Identifier: AGPL-3.0-or-later
# Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
"""Derive the website's looks demo values from the preset files that ship.

The demo table in `web/js/site.js` was hand written, and it had drifted from
`assets/presets/` in 72 places across 8 looks, 14 of them sign flips.
Glasshouse ships Highlights +8 and the page showed -25. Meanwhile the page
claimed the chips were the real slider positions, so the fix is not to edit
the numbers by hand again. It is to stop hand writing them.

Import `looks_table()` from the checker to compare, or run this file to print
the JavaScript block for pasting.
"""
from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
PRESETS = ROOT / "assets" / "presets"

# slug -> (family, preset name). These are the looks the page offers.
DEMO = [
    ("glasshouse", "Tonal", "Glasshouse"),
    ("super8", "Retro", "Super 8"),
    ("sunday-chrome", "Retro", "Sunday Chrome"),
    ("noir-grain", "Mono", "Noir Grain"),
    ("neon-rain", "Neon", "Neon Rain"),
    ("moss-slate", "Verdant", "Moss and Slate"),
    ("concrete-brutal", "Edge", "Concrete Brutal"),
    ("fog-bank", "Atmosphere", "Fog Bank"),
]

# crs field -> (js key, divisor, clamp). The browser shader works in -1..1
# where Adobe's schema works in -100..100.
FIELDS = {
    "Contrast2012": ("contrast", 100.0, 0.5),
    "Highlights2012": ("highlights", 100.0, 1.0),
    "Shadows2012": ("shadows", 100.0, 1.0),
    "Whites2012": ("whites", 100.0, 1.0),
    "Blacks2012": ("blacks", 100.0, 1.0),
    "Vibrance": ("vibrance", 100.0, 1.0),
    "Saturation": ("saturation", 100.0, 1.0),
    "Temperature": ("temp", 100.0, 0.3),
    "Tint": ("tint", 100.0, 0.15),
    "Exposure2012": ("exposure", 1.0, 1.2),
}


def read_preset(family: str, name: str) -> dict[str, float]:
    path = PRESETS / family / f"{name}.xmp"
    xml = path.read_text(encoding="utf-8")
    out: dict[str, float] = {}
    for field, (key, div, cap) in FIELDS.items():
        m = re.search(rf"<crs:{field}>(-?[\d.]+)</crs:{field}>", xml)
        if not m:
            continue
        v = float(m.group(1)) / div
        v = max(-cap, min(cap, v))
        if v:
            out[key] = round(v, 4)
    # A black and white conversion is saturation floored, however the
    # preset spells it.
    if re.search(r"<crs:ConvertToGrayscale>True</crs:ConvertToGrayscale>", xml):
        out["saturation"] = -1
    return out


def looks_table() -> dict[str, dict]:
    table = {"original": {"name": "As shot", "group": "No grade applied", "v": {}}}
    for slug, family, name in DEMO:
        table[slug] = {"name": name, "group": family, "v": read_preset(family, name)}
    return table


def render_js() -> str:
    """The `var LOOKS = {...}` block, ready to paste into web/js/site.js."""
    table = looks_table()
    order = ["temp", "tint", "exposure", "contrast", "highlights",
             "shadows", "whites", "blacks", "vibrance", "saturation"]
    slug_w = max(len(slug) for slug in table) + 3
    name_w = max(len(e["name"]) for e in table.values()) + 3
    lines = ["    var LOOKS = {"]
    items = list(table.items())
    for i, (slug, e) in enumerate(items):
        vals = ", ".join(f"{k}: {e['v'][k]:g}" for k in order if k in e["v"])
        key = f"'{slug}':".ljust(slug_w)
        nm = f"'{e['name']}',".ljust(name_w)
        comma = "" if i == len(items) - 1 else ","
        lines.append(
            f"      {key} {{ name: {nm} group: '{e['group']}', v: {{ {vals} }} }}{comma}"
        )
    lines.append("    };")
    return "\n".join(lines)


if __name__ == "__main__":
    if "--js" in sys.argv:
        print(render_js())
    else:
        for slug, e in looks_table().items():
            print(f"{slug:18} {e['v']}")
    sys.exit(0)
