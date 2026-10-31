## v0.2.0 – 2026-10-31

A classic Windows setup wizard, and every installer packed on any OS.

### Features

- Added a new Windows setup with a classic install wizard, an optional desktop shortcut and a silent `/S` mode.
- Windows apps installed with v0.1 move to the new setup on their next update, with no reinstall.
- Updates now work for Windows apps installed in a custom folder, picked in the wizard or set with `/D=`.
- Added packing on Windows: Linux, macOS or Windows builds every installer, with nothing else to install.
- Added a prebuilt packer for Windows.
- Added `SHIPO_FAKE_UPDATE=<version>` to preview your app's update UI without a release, for feeds marked `.fakeable()`.
- Added reproducible packages: with `SOURCE_DATE_EPOCH` set, every OS packs the same bytes, apart from a DMG signed with an identity and the feed listing it.

### Fixes

- Fixed update network errors on Windows showing a bare code instead of a message.

**Full Changelog**: https://github.com/vladkens/shipo/compare/v0.1.0...v0.2.0

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
