## v0.1.0 – 2026-10-30

First release: ship your desktop app to macOS, Windows and Linux from one machine, and let it update itself.

### Features

- Added installers for macOS (DMG), Windows (setup) and Linux (AppImage, .deb), all packed on one Linux machine or Mac.
- Added signing and notarization of macOS builds without a Mac.
- Added `shipo release`: one command makes the installers and a signed update feed, ready for a GitHub release.
- Added in-app updates: a small library downloads the update and installs it after the app quits.
- Added signed updates, with a spare key to replace a leaked one.
- Added an open, plain-text update feed ([spec.md](https://github.com/vladkens/shipo/blob/main/spec.md)).

**Full Changelog**: https://github.com/vladkens/shipo/commits/v0.1.0
