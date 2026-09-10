#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 PixeLabs
# SPDX-License-Identifier: AGPL-3.0-or-later
# Additional terms under AGPL section 7 apply: see COPYING.additional-terms.
"""Keep the website structurally sound and factually honest.

Two classes of check.

**Structural.** A stylesheet whose braces do not balance silently nests
everything after the fault inside whatever block was left open. That is a
correctness bug, not a style one: it has already shipped a state where the
`prefers-reduced-motion` rules applied only below 680px, so people who asked
their computer to stop animating things got animation anyway on every desktop.

**Truth.** The page hand-writes facts that actually live in the code. These
assertions are anchored in the CODE, not in the page, which is the whole point:
a guard that only reads the page can catch it contradicting itself but never
catches it going stale. Anchored this way, the build fails on the commit that
makes the page untrue, and tells whoever wrote it.

Run from anywhere: python3 tools/site-checks/check_site.py
"""
from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
WEB = ROOT / "web"

failures: list[str] = []


def ok(msg: str) -> None:
    print(f"  ok    {msg}")


def bad(msg: str) -> None:
    print(f"  FAIL  {msg}")
    failures.append(msg)


def brace_balance(css: str) -> int:
    """Net unclosed braces, ignoring any inside strings or comments."""
    i, n, depth = 0, len(css), 0
    in_str: str | None = None
    in_comment = False
    while i < n:
        c = css[i]
        if in_comment:
            if css.startswith("*/", i):
                in_comment = False
                i += 2
                continue
            i += 1
            continue
        if in_str:
            if c == "\\":
                i += 2
                continue
            if c == in_str:
                in_str = None
            i += 1
            continue
        if css.startswith("/*", i):
            in_comment = True
            i += 2
            continue
        if c in "\"'":
            in_str = c
            i += 1
            continue
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
        i += 1
    return depth


def check_css_braces() -> None:
    for css_path in sorted(WEB.glob("css/*.css")):
        depth = brace_balance(css_path.read_text(encoding="utf-8"))
        rel = css_path.relative_to(ROOT)
        if depth == 0:
            ok(f"{rel} braces balance")
        else:
            bad(
                f"{rel} has {depth} unclosed block(s). Every rule after the "
                f"fault is nested inside it by accident."
            )


def check_licence_wording(page: str) -> None:
    stale = re.search(r"source[ -]available|polyform|noncommercial", page, re.I)
    if stale:
        bad(f"web/index.html still says {stale.group(0)!r}, a licence this project dropped")
    else:
        ok("licence wording is current")
    if "Affero" in page:
        ok("the page names the AGPL")
    else:
        bad("web/index.html never mentions the AGPL")


def check_preset_count(page: str) -> None:
    shipped = len(list((ROOT / "assets" / "presets").rglob("*.xmp")))
    claimed = {int(m) for m in re.findall(r"(\d+)\s+(?:bundled\s+)?(?:presets|looks)", page)}
    if "hirty-six" in page:
        claimed.add(36)
    if not claimed:
        bad("the page states no preset count at all, so it cannot be checked")
    elif claimed == {shipped}:
        ok(f"page's preset count matches the {shipped} files that ship")
    else:
        bad(f"page claims {sorted(claimed)} presets, {shipped} .xmp files actually ship")


def check_preset_families(page: str) -> None:
    families = sorted(p.name for p in (ROOT / "assets" / "presets").iterdir() if p.is_dir())
    missing = [f for f in families if f not in page]
    if missing:
        bad(f"preset families that ship but are not named on the page: {', '.join(missing)}")
    else:
        ok(f"all {len(families)} preset families are named on the page")


def check_masking_still_unbuilt(page: str) -> None:
    """The page says masking does not exist. Fail the day it does."""
    src = ROOT / "crates" / "lightbox-mask" / "src"
    lines = 0
    for rs in src.rglob("*.rs"):
        for line in rs.read_text(encoding="utf-8").splitlines():
            s = line.strip()
            if s and not s.startswith("//"):
                lines += 1
    page_says_unbuilt = re.search(r"masking[^.]{0,60}not built|not built[^.]{0,60}masking", page, re.I)
    if lines < 20:
        ok(f"lightbox-mask is still a stub ({lines} lines), so the page's claim holds")
    elif page_says_unbuilt:
        bad(
            f"lightbox-mask now has {lines} lines of real code, but the website "
            f"still tells people masking is not built. Update web/index.html."
        )
    else:
        ok(f"lightbox-mask has {lines} lines and the page no longer claims otherwise")


def main() -> int:
    page = (WEB / "index.html").read_text(encoding="utf-8")
    print("site checks")
    check_css_braces()
    check_licence_wording(page)
    check_preset_count(page)
    check_preset_families(page)
    check_masking_still_unbuilt(page)
    print()
    if failures:
        print(f"site checks FAILED ({len(failures)})")
        return 1
    print("site checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
