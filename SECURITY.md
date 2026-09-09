# Security Policy

## Reporting a vulnerability

Email `security@pixelabs.net` with what you found and, if you have one, a way to reproduce it. There is no bug bounty program. You can expect a best-effort response, not a guaranteed turnaround: this is a small pre-1.0 project, not a funded security team. Please don't open a public issue for anything you believe is exploitable until we've had a chance to look at it.

## Scope

Lightbox's real attack surface is decoding untrusted binary image files. Someone using this editor will point it at raw, JPEG, TIFF, PNG, or HEIC files from cameras and sources they don't control, and every one of those formats is a binary parser's worth of attack surface. That's in scope, and it's taken seriously enough to shape the architecture: raw mosaic decoding through LibRaw, which is memory-unsafe C++, runs in a separate sandboxed subprocess (`lightbox-rawproxy`) specifically so a decoder crash or memory-safety bug fails one file instead of the whole running application. If you find a way to turn a decode bug into something worse than a crash, or a way to escape that sandbox, that's exactly what this policy is for.

Also in scope: the local SQLite edit store (corruption or data loss from a crafted input, not just from a crash), XMP and ICC profile parsing (both parse untrusted, user-supplied data), and anything that would let opening a file execute code or touch the filesystem outside the store it's meant to write to.

## Out of scope

Lightbox runs entirely on your machine with no server component and no accounts, so there is no login flow, no multi-tenant data boundary, and no network service to probe. Denial-of-service reports that require an attacker who can already run arbitrary code on the same machine aren't useful; at that point the machine, not Lightbox, is compromised. Issues in a dependency that's already public and has an upstream fix pending are better reported upstream, though we'd still like to know if it's exploitable through Lightbox specifically.

## Supported versions

There isn't a supported-versions table, because there isn't a 1.0 yet. Fixes land on `main`, and that's the only branch that gets them. If you're running something older, update before assuming a report still applies.
