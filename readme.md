# shipo

Releases and updates for desktop apps.

- `shipo` (the packer, `cli/`): an app's bare binaries for macOS, Windows and Linux into a signed and notarized DMG, a per-user setup, an AppImage and a .deb, and a signed `update.txt`, all on one Ubuntu host.
- `shipo` (the crate, `src/`): inside the app, checks the feed, downloads the update and installs it in place after the app quits, with no window.

`spec.md` describes the feed and the install contract between them.

Work in progress.
