Done: 2026-11-17

# The setup stubs from a pinned CRT and SDK

CI's `setup` job fails on every branch since Visual Studio 17.14.45 (2026-11-10): cargo-xwin takes the CRT and SDK from Microsoft's live manifest by version alone, and the SDK 10.0.26100 build moved from .15 to .18 under the same version, so kernel32.lib's import hints, and with them the stubs' bytes, changed. The person wants no stub rebuilds for Microsoft's updates or for releases.

## Tasks

- [x] 1. Confirm the cause: the CI-built and committed stubs differ only in the timestamp, the /Brepro hash and kernel32's import hints (+5 each).
- [x] 2. Find 17.14.44's package manifest (VisualStudio.vsman), the one live when the stubs were built: its URL and its payloads still download.
- [x] 3. `scripts/setup-stub.py` builds from that pinned manifest: the vsman by URL and SHA-256, xwin (pinned, installed by the script) unpacks the CRT and SDK from it and checks each payload's SHA-256, plain `cargo build` links with the pinned rustc's rust-lld and cargo-xwin's two link flags that change the bytes (`-nodefaultlib:ucrt -defaultlib:libucrt`); no cargo-xwin.
- [x] 4. The committed stubs come out byte for byte from a clean `setup/target`, so they stay as committed.
- [x] 5. CI's `setup` job without cargo-xwin; the Makefile's `setup` line.
- [x] 6. A PR to main, green (#19).

## Result

The stubs build from Visual Studio 17.14.44's vsman, pinned by URL and SHA-256, through xwin 0.10.0 and plain `cargo build`, to the committed bytes; cargo-xwin is gone. A newer CRT or SDK comes only by changing `VSMAN` and `make setup`. CI's `setup` takes 3m33s, about a minute more than before, for compiling xwin.
