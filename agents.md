# shipo

Releases and updates for desktop apps. The packer (`cli/`, the `shipo` binary) turns an app's bare binaries for macOS, Windows and Linux into signed installers and a signed update feed, on one Ubuntu host. The library (`src/`, the `shipo` crate) runs inside the app: it checks the feed, downloads the update and installs it in place after the app quits.

Each fact has one source: the contract between the two is `spec.md`, the API the rustdoc, the CLI `shipo --help`, checks the `Makefile`, the work under way `docs/plans/`.

## Scope

- Change `spec.md`, the public API or the CLI only with the person's approval; a `spec.md` change lands with both sides' code. A public struct with public fields is `#[non_exhaustive]`.
- The library has no UI and no state of its own: it never shows a window, dialog or notification or takes the focus; the app draws everything and keeps its settings.
- Library dependencies are only `ed25519-compact`, the `objc2` family on macOS and `windows-sys` on Windows (Linux loads libcurl at run time): every app builds them. The packer takes a crate only where our own code would be large, each with a stated reason. Change dependencies only with `cargo add`, `cargo remove` or `cargo upgrade`, never by hand.
- The packer runs on Ubuntu (`shipo app` also on a Mac). It may require a tool apt installs; fetch any other one pinned by version and SHA-256.
- The packer ships as prebuilt binaries: a release job with secrets downloads one pinned by SHA-256, never builds crates from source.

## Code

- Start every `.rs` file with `// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo`, then a `//!` doc whose first sentence says what the file does.
- Split a file before it passes ~800 lines of code (tests, docs, comments and blanks not counted).
- Put the library's OS code in `src/platform/<os>.rs` (shared Unix code in `unix.rs`); the rest stays testable on any host.
- Helper scripts go in `scripts/`, kebab-case, Python run with `uv run`.
- A comment adds what the code doesn't say, briefly; it never restates the code or says where the code came from (no "ported from …").
- A test never shows a window.
- Check each format with the system's own reader too: `hdiutil` and `fsck_hfs` for the DMG, `unsquashfs` and the runtime for the AppImage, `dpkg-deb --info` for the .deb, the setup run with `/S` on Windows.

## Workflow

- Keep multi-step work as a checklist in `docs/plans/yyyymmdd-<name>.md`, only ticking it during the work; when done, add `Done: yyyy-mm-dd` on top and a short `## Result`, and move it to `docs/plans/completed/`.
- A step is done when `make check` and `make test` pass.
- A commit is one short lowercase line, `<area>: <what changed>` (`cli: …`, `library: …`), no body, with its docs.
- CI runners are `ubuntu-latest`, `macos-latest`, `windows-latest`, never pinned.
