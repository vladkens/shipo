## Unreleased

### Features

- Added a new Windows setup with a classic install wizard, an optional desktop shortcut and a silent `/S` mode.
- Added updates of v0.1's Windows installs to the new setup, with no reinstall.
- Added packing on Windows: Linux, macOS or Windows builds every installer, with nothing else to install.
- Added reproducible packages: with `SOURCE_DATE_EPOCH` set, shipo packs the same bytes from any host, except a DMG signed with an identity and the feed that lists it.

### Fixes

- Fixed update network errors on Windows showing a bare code instead of a message.

## v0.1.0 – 2026-10-30

First release: ship your desktop app to macOS, Windows and Linux from one machine, and let it update itself.

### Features

- Added installers for macOS (DMG), Windows (setup) and Linux (AppImage, .deb), all packed on one Linux machine or Mac.
- Added signing and notarization of macOS builds without a Mac.
- Added `shipo release`: one command makes the installers and a signed update feed, ready for a GitHub release.
- Added in-app updates: a small library downloads the update and installs it after the app quits.
- Added signed updates, with a spare key to replace a leaked one.
- Added an open, plain-text update feed ([spec.md](https://github.com/vladkens/shipo/blob/v0.1.0/spec.md)).

**Full Changelog**: https://github.com/vladkens/shipo/commits/v0.1.0
