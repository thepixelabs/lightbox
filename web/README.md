<!-- SPDX-FileCopyrightText: 2026 PixeLabs -->
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Additional terms under AGPL section 7 apply: see COPYING.additional-terms. -->

# The Lightbox website

The marketing site for Lightbox. Plain HTML, CSS and JavaScript with no build
step, no framework and no dependencies, so you can open it, edit it and see the
result immediately.

## Running it

```sh
cd web
python3 -m http.server 8777
```

Then open <http://127.0.0.1:8777>. A plain file open (`file://`) mostly works
too, but WebGL texture loading needs an HTTP origin, so the hero will fall back
to a static image.

## What is where

| Path | Contents |
|---|---|
| `index.html` | The whole page. One file, sectioned with comments. |
| `css/site.css` | Design tokens at the top, then sections in page order, responsive rules and reduced-motion overrides at the bottom. |
| `js/develop.js` | A small raw develop pipeline in WebGL: white balance, exposure, tone, saturation, split tone, vignette and grain, plus histogram read-back from a 128 by 80 offscreen target. |
| `js/scenes.js` | The canvas set pieces: the interactive tone curve, and the HSL, white balance, crop, histogram and export demonstrations. All share one animation loop that only runs while a scene is on screen. |
| `js/site.js` | Navigation, reveal on scroll, one entry animation and the copy buttons. Four and a half kilobytes, no canvas and no WebGL: the live develop demos were removed because they demonstrated a browser shader rather than Lightbox. |
| `assets/img/` | Photographs and screenshots, WebP only apart from the social card. See the provenance note below. |
| `assets/fonts/` | Self-hosted webfonts. Inter and JetBrains Mono are Latin subsets of the very TTFs the application embeds; Inter Tight is the Latin subset of the variable face. All SIL OFL 1.1. |

## About the images

Every photograph on the page was generated for this project and then rendered
through the real Lightbox engine, so the graded frames are genuine engine
output rather than a filter applied in an image editor. Nothing here is borrowed and there is no credit line to carry. The full
resolution sources are kept outside the repository: they are large, and they
carry the generator's own provenance metadata, which belongs with the file.

```sh
lightbox-cli open <source>.jpg --store /tmp/site.lbdata
lightbox-cli preset apply --catalog /tmp/site.lbdata --image 1 --id <preset-id>
lightbox-cli render --catalog /tmp/site.lbdata --image 1 --out out.png
```

The nine preset frames are regenerated the same way, one per family, and the
numbers printed beside them are read out of the shipped `.xmp` files by
`tools/site-checks/looks_from_presets.py` rather than typed by hand. CI fails
if the two disagree.

The application screenshots are captures of the real app, taken with
`lightbox <paths> --screenshot out.png`, opened on those same generated
photographs.

The application screenshots come from the shell's own capture mode, which runs
the real app against a throwaway catalog so the result is identical on any
machine:

```sh
lightbox fixtures/*.raf --screenshot app-editor.png --screenshot-size 1600x1000
```

If you change the imagery, keep both facts true. The claim that these are real
engine renders is one of the more persuasive things on the page, and it is only
worth making while it is accurate.

## Third-party requests

There are none. The page self-hosts its fonts and loads no tag manager, no
embedded video, nothing from a font CDN and no analytics. Open the network tab
and every request is to this origin.

An analytics beacon used to sit here, gated to the production host and never
actually switched on. It went because the argument for it was weak: a page whose
central claim is that Lightbox never phones home should not open a connection to
count its own readers, and the download figures GitHub publishes answer the same
question without putting a script on anyone's machine.

Keep it to that one. A page arguing that Lightbox never phones home has no
business opening a connection to Google to draw its own headline, and a
sceptical reader can check the whole claim with the network tab open.

## Conventions

The page is dark only, by choice, because it is about photographs. Colour comes
from one steel blue accent, `#5583a8`, which is the same accent token the
application ships with, plus a warm to cool spectral gradient used sparingly and
only where colour grading is the actual subject.

`prefers-reduced-motion` is honoured properly rather than nominally: the hero
stops being a scroll-driven scene and becomes a static developed frame, the
canvas animations hold still, and the typing terminal prints at once.

Keep claims on this page in step with `../README.md`. If a feature moves from
the roadmap into the product, it moves in both places on the same day, and not
before.

## Deploying

A GitHub Actions workflow publishes `web/` to GitHub Pages on every push to
`main` that touches it. See `.github/workflows/pages.yml`.

There is nothing dynamic here, so any static host works just as well. For
Cloudflare Pages, point the project at this directory with no build command.
