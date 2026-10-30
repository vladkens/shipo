## Unreleased

### Features

- Added our own Windows setup, with a classic install wizard: Welcome, the install folder, progress, and Finish with "Run" and a desktop shortcut; the uninstaller asks first, and `/S` installs silently.
- Added in-place updates of v0.1's Windows installs to the new setup.
- Added packing on Windows: Linux, a Mac or Windows packs every package, with nothing to install.
- Added reproducible packages: the same bytes whichever machine packs them.

### Fixes

- Fixed Windows' network errors in updates, which showed a code instead of their text.

## v0.1.0 – 2026-10-30

First release: ship your desktop app to macOS, Windows and Linux from one machine, and let it update itself.

### Features

- Added installers for macOS (DMG), Windows (setup) and Linux (AppImage, .deb), all packed on one Linux machine or Mac.
- Added signing and notarization of macOS builds without a Mac.
- Added `shipo release`: one command makes the installers and a signed update feed, ready for a GitHub release.
- Added in-app updates: a small library downloads the update and installs it after the app quits.
- Added signed updates, with a spare key to replace a leaked one.
- Added an open, plain-text update feed ([Feed](https://github.com/vladkens/shipo#-feed)).

**Full Changelog**: https://github.com/vladkens/shipo/commits/v0.1.0
