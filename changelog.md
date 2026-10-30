## v0.1.0 – 2026-10-30

First release: ship your desktop app to macOS, Windows and Linux from one machine, and let it update itself.

### Features

- Added installers for every platform from one Linux machine or Mac: a DMG for macOS, a setup for Windows that needs no admin rights, an AppImage and a .deb for Linux
- Added signing and notarization of macOS builds on Linux, so you don't need a Mac to release for it
- Added `shipo release`, one command that leaves the installers and the update feed ready to upload to a GitHub release, with a GitHub Actions workflow to copy from the readme
- Added in-app updates with a small library: your app checks for a new version and downloads it, and the update installs quietly after the app quits and starts it again
- Added reporting of failed updates: if an update can't install, your app hears why on its next start and can tell the user
- Added signed updates, so your app installs only what you released, with a spare key to take over if the main one leaks
- Added a plain-text update feed described in [spec.md](https://github.com/vladkens/shipo/blob/main/spec.md), so any app or tool can read it

**Full Changelog**: https://github.com/vladkens/shipo/commits/v0.1.0
