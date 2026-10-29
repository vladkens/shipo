# shipo

Releases and updates for desktop apps. The packer (`cli/`, the `shipo` binary) turns an app's bare binaries for macOS, Windows and Linux into signed installers and a signed update feed, on one Ubuntu host. The library (`src/`, the `shipo` crate) runs inside the app: it checks the feed, downloads the update and installs it in place after the app quits, silently. `SPEC.md` is the contract between the two, so either side can be written by someone else.

## Scope

- The library has no UI and no state of its own: it gives the data (`check`) and the mechanism (`download`, `install`, `finish`); the app draws the menu item, the popup, the progress and the setting, and keeps the check time and a skipped version.
- The library never shows a window, a dialog or a notification and never takes the focus.
- Library dependencies: `ed25519-compact`, the `objc2` family on macOS, `windows-sys` on Windows, none on Linux (libcurl loaded at run time). Packer dependencies: as few as possible, each with a reason; report the crate count (`cargo tree -p <crate> --prefix none | sort -u | wc -l`) when it changes. Change dependencies only with `cargo add` / `cargo remove` / `cargo upgrade`.
- The packer runs on Ubuntu (`ubuntu-latest`); `app` also runs on a Mac for a local build. It may require a tool that apt installs (`makensis`, `dpkg-deb`, `mksquashfs`, `openssl`); a tool not in apt is fetched pinned by version and sha256. Its own code replaces only what is not simple to require: the DMG.
- The packer is published as prebuilt binaries; a release job with the secrets downloads the binary pinned by sha256 and never builds crates from source.

## Code

- Start every `.rs` file with `// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo`, then a `//!` doc whose first sentence says what the file does.
- A file stays within ~800 lines of code, not counting tests, doc comments, comments and blanks; split it before it grows past that.
- A statement after an `if`, `for`, `while`, `loop` or `match` block starts after a blank line, as do the other logical steps; rustfmt never adds them, `scripts/blank-lines.py` checks it.
- OS code lives in `src/platform/<os>.rs` (or `<os>/` when it grows); the portable logic stays testable on any host.
- Keep third-party checkouts in `3rd/` (gitignored), never modify them, and name the source file in a comment when porting from one, keeping its license.
- Helper scripts go in `scripts/`, in kebab-case, in Python run with `uv run`.
- No comments in workflow files.

## Testing

- `make check` (nightly fmt, the blank-line check, clippy with `-D warnings`) and `make test` pass before a commit; `make prepare` formats, adds the blank lines and applies clippy fixes.
- Tests are ordinary `#[test]` functions: pure logic inline, whole flows in `tests/`. They never show a window.
- Formats are checked by the system's own reader, not only by our tests: `hdiutil` and `fsck_hfs` for the DMG, `unsquashfs` and the runtime for the AppImage, `dpkg-deb --info` for the .deb, the installer run with `/S` on Windows.
- VMs (`~/Code/vm`, never edited): at most two at once; never quit or relaunch UTM.

## Workflow

- For multi-step work keep a checklist in `docs/plans/yyyymmdd-<name>.md`, tick it during the work, and when done add `Done: yyyy-mm-dd`, a short `## Result`, and move it to `docs/plans/completed/`.
- Commit only when the person asks: one short lowercase line, no body, docs in the same commit.
- Report a blocker the moment you hit it (a missing permission, tool, secret or source); don't build a degraded version around it.
- Ask the person only when a choice changes the direction or the request is ambiguous; otherwise decide and say so.
- Verify claims against the current source (docs, repository, the code itself), not memory; link the source and date what can go stale.
- Ubuntu runners are always `ubuntu-latest`.
