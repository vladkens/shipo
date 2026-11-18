Done: 2026-11-15

# shipo: localized Info.plist strings

Asked 2026-11-15 for Wavo's Russian UI: a macOS app lists its languages in `CFBundleLocalizations`, or the system's panels and menus stay English and the app is missing from System Settings' per-app language list; the microphone prompt shows `NSMicrophoneUsageDescription` translated only from `Contents/Resources/<lang>.lproj/InfoPlist.strings`.

## Tasks

- [x] 1. Config: `[package.metadata.shipo.macos.localizations.<lang>]`, Info.plist keys to translated strings; an empty table only lists the language; a non-string value, a language that is no `.lproj` name or a key not in the Info.plist is an error.
- [x] 2. Info.plist: `CFBundleLocalizations` of `en` and the languages, sorted, when there are any; `macos.plist.CFBundleLocalizations` wins.
- [x] 3. Bundle: `Contents/Resources/<lang>.lproj/InfoPlist.strings` for each language with keys, sealed by the signature and in the DMG.
- [x] 4. Docs: the readme's `macos` table.
- [x] 5. Checked on a Mac: `plutil`, `codesign --verify --deep --strict`, the DMG mounted by `hdiutil`, and `Bundle` reading the strings under `-AppleLanguages '(ru)'`.

## Result

`macos.localizations.<lang>` lists the app's languages in `CFBundleLocalizations` after `en` and writes each one's strings to `Contents/Resources/<lang>.lproj/InfoPlist.strings`, a binary plist as Xcode writes it. On this Mac a `shipo app` bundle and its `shipo release` DMG pass `plutil -lint`, `codesign --verify --deep --strict` (the strings sealed in CodeResources, a changed byte refused) and `hdiutil verify`; Foundation's `Bundle` lists the languages and under `-AppleLanguages '(ru)'` reads the translated `NSMicrophoneUsageDescription` and `CFBundleDisplayName`.
