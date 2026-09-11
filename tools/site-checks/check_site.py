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
    # Either the full name or the SPDX identifier. The owner asked for less
    # licence prose on the landing page, not more, so naming it as
    # "AGPL-3.0-or-later" and pointing at LICENSE is the preferred form.
    if "Affero" in page or "AGPL-3.0" in page:
        ok("the page names the AGPL")
    else:
        bad("web/index.html never names the licence")


def check_presets_actually_ship() -> None:
    """The page's preset count is only true if the app carries them.

    For one build it did not. `bundled_preset_files` knew only the compile
    time source path, so every downloaded copy started with an empty library
    while the page advertised thirty-six presets, and nothing failed, because
    the loader returns an empty list rather than an error. `check_preset_count`
    below passed throughout, because it counts files in the repository, which
    is the wrong artefact.

    CI cannot build a macOS bundle here, so this checks the two pieces of code
    that have to agree: the packager must copy the presets in, and the app must
    look for them beside itself.
    """
    packer = (ROOT / "tools" / "xtask" / "src" / "bundle_mac.rs").read_text(
        encoding="utf-8"
    )
    copies = (
        'root.join("assets/presets")' in packer
        and 'resources.join("presets")' in packer
        and "bundle_presets(root, &resources)" in packer
    )
    if copies:
        ok("bundle-mac copies the preset library into the app")
    else:
        bad(
            "tools/xtask/src/bundle_mac.rs no longer copies assets/presets into "
            "the bundle, so a downloaded Lightbox ships with no presets while "
            "the page advertises them"
        )

    shell = (ROOT / "crates" / "lightbox-shell" / "src" / "lib.rs").read_text(
        encoding="utf-8"
    )
    if "Resources/presets" in shell:
        ok("the app looks for its presets inside the bundle")
    else:
        bad(
            "lightbox-shell no longer looks for Resources/presets, so a "
            "packaged build will find no preset library"
        )


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


def check_develop_source(page: str) -> None:
    """The page says the editor grades the camera's embedded preview and that
    only `lightbox-cli` reaches the sensor. Fail the day either half changes.

    Anchored in two facts, not in prose: who calls `decode_for_develop`, and
    whether the shell can even see the decode crate. Measured on the same raw
    file, `lightbox-cli render` (the shell's pipeline) returns the preview's
    dimensions and `render-ref` (the sensor path) returns the sensor's. When
    someone wires the shell to the proxy, this check is how the website finds
    out instead of continuing to undersell the product.
    """
    # A CALL, not a mention. `decode_for_develop` appears in doc comments and
    # in prose explaining precisely this situation, and an earlier version of
    # this check counted those, so it reported the shell as a caller because
    # `canvas/view.rs` has a comment saying the shell is not one. Strip line
    # comments first, then require the open paren, which also excludes `use`
    # imports.
    callers = set()
    for rs in (ROOT / "crates").rglob("*.rs"):
        code = "\n".join(
            line.split("//")[0] for line in rs.read_text(encoding="utf-8").splitlines()
        )
        if "decode_for_develop(" not in code:
            continue
        callers.add(rs.relative_to(ROOT / "crates").parts[0])
    callers -= {"lightbox-decode"}

    shell_manifest = (ROOT / "crates" / "lightbox-shell" / "Cargo.toml").read_text(
        encoding="utf-8"
    )
    shell_sees_decode = "lightbox-decode" in shell_manifest.split("[dependencies]")[-1]

    page_says_preview = re.search(
        r"embedded[^.]{0,80}not yet on the sensor|not yet on the sensor", page, re.I
    )

    if callers <= {"lightbox-cli"} and not shell_sees_decode:
        ok("the sensor path is still CLI-only, so the page's wording holds")
        if not page_says_preview:
            bad(
                "the editor still grades the embedded preview, but the page no "
                "longer says so. Put the caveat back in web/index.html."
            )
    elif page_says_preview:
        bad(
            f"decode_for_develop now has callers {sorted(callers)} "
            f"(shell sees lightbox-decode: {shell_sees_decode}), so the editor "
            f"can reach sensor data, but the page still says it cannot. "
            f"Update web/index.html, this is good news."
        )
    else:
        ok("the sensor path reaches beyond the CLI and the page no longer claims otherwise")


def check_download_link(page: str) -> None:
    """The download button must point at GitHub's fixed 'latest' filename.

    `releases/latest/download/<file>` only resolves while the filename never
    changes, which is why release.yml uploads every artifact a second time
    under a version-free name. A button pointing at a versioned file works on
    the day it is written and 404s on the next release.
    """
    wanted = "releases/latest/download/Lightbox.dmg"
    if wanted in page:
        ok("the download button uses the stable latest-release link")
    elif "releases/latest/download/" in page:
        bad(
            "the download button uses a latest-release link with a filename "
            "release.yml does not publish under a fixed name"
        )
    else:
        bad("web/index.html has no download button pointing at a release artifact")


def check_cli_examples(page: str) -> None:
    """Every `lightbox-cli` line on the page must be a command that runs.

    The page shipped `lightbox-cli edit set --image 1 exposure=+0.35 temp=5600`
    for months. It fails twice: `--catalog` is required, and `temp` is not an
    accepted parameter at all. A command that errors on first paste is worse
    than no command, because the reader concludes the tool is broken rather
    than the example.

    Anchored in the code: the accepted parameter list is read out of
    `crates/lightbox-cli/src/edit.rs`, so adding `temp` to the CLI is what
    makes `temp=5600` legal on the page, not editing this file.
    """
    edit_src = (ROOT / "crates" / "lightbox-cli" / "src" / "edit.rs").read_text(
        encoding="utf-8"
    )
    block = re.search(
        r"const SCALAR_PARAMS[^=]*=\s*&\[(.*?)\];", edit_src, re.S
    )
    if not block:
        bad("could not find SCALAR_PARAMS in lightbox-cli/src/edit.rs")
        return
    accepted = set(re.findall(r'\("([a-z_]+)",', block.group(1)))

    # Commands that read or write an edit store need to be told which one.
    needs_catalog = {
        "edit", "export", "render", "history", "undo", "redo", "snapshot",
        "preset", "xmp", "step-to", "clear-history", "list", "backup", "check",
    }

    problems = []
    for raw in re.findall(r"lightbox-cli [^\n<]+", page):
        line = raw.replace("\\", " ").strip()
        parts = line.split()
        if len(parts) < 2:
            continue
        sub = parts[1]
        if sub in needs_catalog and "--catalog" not in line and "--store" not in line:
            problems.append(f"`{line}` needs --catalog but does not pass one")
        if sub == "edit" and len(parts) > 2 and parts[2] == "set":
            for kv in parts[3:]:
                if "=" not in kv or kv.startswith("--"):
                    continue
                key = kv.split("=")[0]
                if key not in accepted:
                    problems.append(
                        f"`{line}` sets {key!r}, which lightbox-cli does not "
                        f"accept. It takes: {', '.join(sorted(accepted))}"
                    )

    if problems:
        for p in problems:
            bad(p)
    else:
        ok("every lightbox-cli example on the page is a runnable command")


def check_vibrance_gap(page: str) -> None:
    """Vibrance and saturation exist in the engine but have no slider.

    The node, the shader and the preset field are all real, so a preset moves
    those values and the user then cannot adjust them. The page advertised
    both as things you can set, which is the one kind of error that actively
    wastes a person's time: they download it looking for a control that is not
    there.

    Anchored in the shell, not in the page. The day someone adds the slider,
    this fails and tells them to delete the caveat.
    """
    shell = ROOT / "crates" / "lightbox-shell" / "src"
    has_slider = any(
        "ParamId::Vibrance" in rs.read_text(encoding="utf-8")
        for rs in shell.rglob("*.rs")
    )
    # Match the CLAIM, not one author's phrasing, so a rewrite of the section
    # does not break the guard and a deletion still does.
    page_admits = bool(
        re.search(
            r"(vibrance and saturation|vibrance)[^.]{0,80}"
            r"(no slider|sliders are not|not in the rail)",
            page,
            re.I,
        )
    )
    if has_slider and page_admits:
        bad(
            "the shell now has a Vibrance control, but the page still says "
            "the slider is missing. Delete the caveat in web/index.html."
        )
    elif not has_slider and not page_admits:
        bad(
            "vibrance and saturation still have no slider in the shell, and "
            "the page no longer says so. Put the caveat back."
        )
    elif has_slider:
        ok("vibrance has a slider now and the page no longer claims otherwise")
    else:
        ok("the vibrance/saturation gap is still real and the page still says so")


def check_looks_match_presets(page: str) -> None:
    """The preset values printed on the page must match the files that ship.

    These were hand written in JavaScript and had drifted from
    `assets/presets/` in 72 places across 8 looks, 14 of them sign flips,
    while the page claimed they were the real slider positions. The live
    demo is gone now and the numbers are static markup, so they can drift
    exactly the same way. Regenerate with
    `python3 tools/site-checks/looks_from_presets.py --js`, or read the
    values out of the preset files as that module does.
    """
    sys.path.insert(0, str(ROOT / "tools" / "site-checks"))
    from looks_from_presets import looks_table  # noqa: PLC0415

    label = {
        "contrast": "Contrast", "highlights": "Highlights", "shadows": "Shadows",
        "whites": "Whites", "blacks": "Blacks", "vibrance": "Vibrance",
        "saturation": "Saturation", "temp": "Temp", "tint": "Tint",
        "exposure": "Exposure",
    }
    wrong = []
    for slug, entry in looks_table().items():
        if not entry["v"]:
            continue
        # The card for this preset, from its image to the end of its list.
        m = re.search(
            rf'presets/{re.escape(slug)}\.webp(.*?)</ul>', page, re.S
        )
        if not m:
            wrong.append(f"{slug} has no card on the page")
            continue
        card = m.group(1)
        for key, val in entry["v"].items():
            want = f"{val * 100:+.0f}"
            pair = f"<span>{label[key]}</span><span class=\"mono\">{want}</span>"
            if pair not in card:
                wrong.append(f"{slug}: {label[key]} on the page is not {want}")

    if wrong:
        for w in wrong[:6]:
            bad(w)
        if len(wrong) > 6:
            bad(f"and {len(wrong) - 6} more preset values disagree with the files")
    else:
        ok("the preset values on the page match the files that ship")


def check_preset_family_counts(page: str) -> None:
    """Per-family counts, not just the total.

    The page listed Mono as 6 when it ships 5, so the seven family numbers
    summed to 37 against a headline of 36 that was itself correctly checked.
    Checking only the total let the breakdown drift.
    """
    root = ROOT / "assets" / "presets"
    on_disk = {
        d.name: len(list(d.glob("*.xmp"))) for d in sorted(root.iterdir()) if d.is_dir()
    }
    wrong = []
    for family, n in on_disk.items():
        m = re.search(
            rf"<li>{re.escape(family)}\s*<span class=\"fs-count\">(\d+)</span>", page
        )
        if not m:
            continue  # the names check already covers absence
        if int(m.group(1)) != n:
            wrong.append(f"page says {family} has {m.group(1)}, it ships {n}")
    if wrong:
        for w in wrong:
            bad(w)
    else:
        ok("every preset family's count matches the files that ship")


def check_formats_are_really_supported(page: str) -> None:
    """Only advertise raw formats the probe can actually open.

    The page advertised RW2. `crates/lightbox-decode/src/probe/mod.rs` returns
    `ProbeParts::unsupported` for it: the file is catalogued and badged, never
    decoded. A Panasonic owner would download on that promise and find they
    cannot open their own photographs, which is the most expensive kind of
    false claim a landing page can carry.
    """
    probe = (ROOT / "crates" / "lightbox-decode" / "src" / "probe" / "mod.rs").read_text(
        encoding="utf-8"
    )
    # Extensions the page advertises, from the status list.
    m = re.search(r'class="status-ext mono">([A-Z0-9 ]+)</span>', page)
    if not m:
        ok("the page advertises no explicit format list")
        return
    advertised = set(m.group(1).split())
    # A format named in an "unsupported" comment is a promise the code breaks.
    unsupported = set()
    for line in probe.splitlines():
        if "unsupported" in line.lower() or "Unsupported" in line:
            for token in re.findall(r"\b([A-Z]{2,4}[0-9]?)\b", line):
                unsupported.add(token)
    clash = sorted(advertised & unsupported)
    if clash:
        bad(
            f"the page advertises {', '.join(clash)}, which the probe reports "
            f"as unsupported. Remove it or implement it."
        )
    else:
        ok(f"all {len(advertised)} advertised formats are ones the probe accepts")


def check_shader_count(page: str) -> None:
    """The "N compute shaders back the controls" claim must match the tree."""
    words = {
        "Twelve": 12, "Thirteen": 13, "Fourteen": 14, "Fifteen": 15,
        "Sixteen": 16, "Seventeen": 17, "Eighteen": 18, "Nineteen": 19,
        "Twenty": 20, "Twenty-one": 21, "Twenty-two": 22,
    }
    m = re.search(r"\b([A-Z][a-z]+(?:-[a-z]+)?) compute shaders back the controls", page)
    if not m:
        ok("the page makes no shader-count claim")
        return
    claimed = words.get(m.group(1))
    shaders = ROOT / "crates" / "lightbox-render" / "shaders"
    actual = len(list(shaders.glob("global_*.wgsl"))) + len(list(shaders.glob("geom_*.wgsl")))
    if claimed == actual:
        ok(f"the page's {actual} control-backing shaders matches the tree")
    else:
        bad(
            f"the page claims {m.group(1).lower()} ({claimed}) compute shaders "
            f"back the controls; the tree has {actual} global_* and geom_* shaders"
        )


def check_ai_still_unbuilt(page: str) -> None:
    """`lightbox-ml` is an empty stub, exactly like `lightbox-mask`.

    The masking stub had a tripwire and the identical AI stub had none, so
    the page could start advertising on-device AI with nothing behind it and
    no build would complain. Same guard, same shape.
    """
    src = ROOT / "crates" / "lightbox-ml" / "src"
    lines = 0
    for rs in src.rglob("*.rs"):
        for line in rs.read_text(encoding="utf-8").splitlines():
            t = line.strip()
            if t and not t.startswith("//"):
                lines += 1
    claims_ai = re.search(
        r"(on-device|local) AI[^.]{0,60}(not built|is not)|AI[^.]{0,40}not built",
        page,
        re.I,
    )
    if lines < 20:
        ok(f"lightbox-ml is still a stub ({lines} lines)")
        if re.search(r"\bAI\b[^.]{0,80}(denoise|super.resolution|masking)\s", page) and not claims_ai:
            bad(
                "the page describes AI features while lightbox-ml is an empty "
                "stub, and says nothing about them being unbuilt"
            )
    else:
        ok(f"lightbox-ml has {lines} lines of real code now")


def main() -> int:
    page = (WEB / "index.html").read_text(encoding="utf-8")
    print("site checks")
    check_css_braces()
    check_licence_wording(page)
    check_presets_actually_ship()
    check_preset_count(page)
    check_preset_families(page)
    check_preset_family_counts(page)
    check_formats_are_really_supported(page)
    check_shader_count(page)
    check_masking_still_unbuilt(page)
    check_ai_still_unbuilt(page)
    check_develop_source(page)
    check_download_link(page)
    check_cli_examples(page)
    check_vibrance_gap(page)
    check_looks_match_presets(page)
    print()
    if failures:
        print(f"site checks FAILED ({len(failures)})")
        return 1
    print("site checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
