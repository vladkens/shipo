# shipo

Releases and updates for desktop apps. The packer (`src/cli/`, the `shipo` binary) turns an app's bare binaries for macOS, Windows and Linux into signed installers and a signed update feed, on a Linux, macOS or Windows host. The library (the rest of `src/`, the `shipo` library) runs inside the app: it checks the feed, downloads the update and installs it in place after the app quits. Both are one crate, `shipo`: the binary needs the `cli` feature, on by default.

Each fact has one source: the contract between the two is `spec.md`, the API the rustdoc, the CLI `shipo --help`, checks the `Makefile`, the work under way `docs/plans/`.

## Scope

- Change `spec.md`, the public API or the CLI only with the person's approval; a `spec.md` change lands with both sides' code. A public struct with public fields is `#[non_exhaustive]`.
- The library has no UI and no state of its own: it never shows a window, dialog or notification or takes the focus; the app draws everything and keeps its settings.
- Library dependencies are only `ed25519-compact`, `thiserror` (the library's `Error`), the `objc2` family on macOS and `windows-sys` on Windows (Linux loads libcurl at run time): every app builds them. The packer's are optional, under the `cli` feature, so an app with `default-features = false` builds none of them. The packer takes a crate only where our own code would be large. Change dependencies only with `cargo add`, `cargo remove` or `cargo upgrade`, never by hand.
- The packer builds every package on any of the three OSes. It may use only what each OS has or installs by one command on all three (`curl`, `git`); a tool of one platform (an apt package, `hdiutil`, `makensis`) is replaced by our own code, and any other tool is fetched pinned by version and SHA-256.
- An app's release job installs the packer pinned to a version, `cargo install shipo --version <x> --locked`; the prebuilt binaries are a convenience.

## Code

- Start every `.rs` file with `// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo`, then a `//!` doc whose first sentence says what the file does.
- A file holds one feature (the DMG, the Linux packages, signing), not one helper or format each; split it only before it passes ~800 lines of code (tests, docs, comments and blanks not counted). `make size` shows each part against its budget. Inside a file, `// MARK: <name>` separates its blocks.
- Put the library's OS code in `src/platform/<os>.rs` (code shared by macOS and Linux in `platform/mod.rs`'s Unix block); the rest stays testable on any host.
- Helper scripts go in `scripts/`, kebab-case, Python run with `uv run`.
- A comment adds what the code doesn't say, briefly; it never restates the code. Code taken from another project names it in the file's header (project, licence, link to the file), so it can be compared and updated; code moved from the person's own projects says nothing of where it came from.
- A test never shows a window.
- Check each format with the system's own reader too: `hdiutil` and `fsck_hfs` for the DMG, `unsquashfs` and the runtime for the AppImage, `dpkg-deb --info` for the .deb, the setup run with `/S` on Windows.

## Workflow

- Keep multi-step work as a checklist in `docs/plans/yyyymmdd-<name>.md`, only ticking it during the work; when done, add `Done: yyyy-mm-dd` on top and a short `## Result`, and move it to `docs/plans/completed/`; a release commit moves every finished plan there.
- A step is done when `make check` and `make test` pass.
- A commit is one short lowercase line, `<area>: <what changed>` (`cli: …`, `library: …`), no body, with its docs.
- CI runners are `ubuntu-latest`, `macos-latest`, `windows-latest`, never pinned.
