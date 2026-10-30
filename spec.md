# shipo's contract

What the packer (`shipo`, `src/cli/`) makes and the library (the `shipo` crate, `src/`) reads and does, so either side can be written by someone else. "Must" is the contract; the rest says how the two sides here do it.

## Platforms

A platform is `<os>-<arch>` as Rust's `std::env::consts` names them: `macos`, `windows` or `linux`, and `x86_64` or `aarch64`. File names spell Windows' arch as Windows does: `x64`, `arm64`.

## Release files

The packer makes, per platform it is given a binary for:

| platform | file | what it is |
|---|---|---|
| `macos-<arch>` | `<Name>-<label>-macos-<arch>.dmg` | the app, signed, notarized and stapled, in a disk image ([macOS](#macos)) |
| `windows-<arch>` | `<Name>-<label>-windows-<x64\|arm64>-setup.exe` | the per-user setup ([Windows](#windows)) |
| `linux-<arch>` | `<Name>-<label>-linux-<arch>.AppImage` | the app as one file ([Linux](#linux)) |
| `linux-<arch>` | `<Name>-<label>-linux-<arch>.deb` | the app for apt, which updates it; not in the feed |

`<Name>` is the app's name, `<label>` is `dev` for a build of a branch and the tag, `v<version>`, for a release. The packer writes the files by these names and the feed lists them so, to be uploaded as they are. A release's tag is `v` and the app's version; the packer refuses another.

## The feed

The feed is one text file, `update.txt`, signed, hosted anywhere; on GitHub a release's asset, at `<repository>/releases/latest/download/update.txt`, its files at `<repository>/releases/download/<tag>/<file>`.

```text
version 1.2.0
notes https://github.com/vladkens/wavo-app/releases/tag/v1.2.0
macos-aarch64 394788 <sha512 hex> https://github.com/vladkens/wavo-app/releases/download/v1.2.0/Wavo-v1.2.0-macos-aarch64.dmg
windows-x86_64 285930 <sha512 hex> https://github.com/…/Wavo-v1.2.0-windows-x64-setup.exe
linux-x86_64 412204 <sha512 hex> https://github.com/…/Wavo-v1.2.0-linux-x86_64.AppImage
sig <signature hex>
```

It is UTF-8, at most 1 MiB, one record per line; a line ends in LF or CRLF, the last one may have no end. A record is its first word, a single space and its fields split by single spaces:

- `version <version>`, exactly once: the release's version, one word, no `v`.
- `notes <url>`, at most once: the release's page, `<repository>/releases/tag/<tag>` on GitHub; https.
- `<platform> <size> <sha512> <url>`, at most once per platform: the platform's file, its size in bytes in decimal digits, its SHA-512 in 128 hex digits of either case, and its URL.
- `sig <signature>`, the last line and the only one of that word: the Ed25519 signature (RFC 8032, of the message itself, not a hash of it) of every byte before this line, line ends included, in 128 hex digits.

A record of another first word is skipped, repeated or not, so the format may grow: an older client skips a newer record, and a client skips other platforms' records. A later format adds multi-line text (release notes) one of two ways, both skipped by this one: a file beside the release's, `<word> [<lang>] <size> <sha512> <url>`, or a block of lines after its record, each starting with `|` (a bare `|` for an empty line), so none reads as a record. A `version`, `notes` or own platform record repeated, an empty line or one that starts with a space, a feed whose last line is not its one `sig` line, or a malformed `version`, `notes` or own platform record makes the feed invalid, whether it offers an update or not; a client takes nothing from an invalid feed. A URL is printable ASCII (no space or control character; anything else percent-encoded) and https to a host, or http to loopback (`127.0.0.1`, `localhost`, `[::1]`, any port) for tests.

The packer writes the records in the order above, each ending in LF, the platforms in the order macOS, Windows, Linux and x86_64 before aarch64, and only the files the library installs: the DMG, the setup and the AppImage. It writes the feed beside it and renames it into place, so a failed signature leaves the old one.

### Reading it

A client checks the signature before it reads a record: the feed is valid when any one of the public keys the app holds verifies it. Then it takes the release when its version is newer than the running one and the feed has a record for its platform; without that record there is no update. A running version that is a pre-release (it holds `-`, as `1.3.0-rc.1` and a dev build's `0.1.0-5-gabc` do) takes no update, not even its own release, and the client fetches nothing. A running release takes a pre-release the feed offers when it is newer.

A version is newer by its numeric dot parts first (`1.10` after `1.9`, `1.2` equal to `1.2.0`), then a release after its pre-releases (`1.2.0` after `1.2.0-rc.1`), the pre-releases by their dot parts, numbers below words, as semver orders them; a leading `v` and a `+build` are ignored. Two versions that don't both parse so make the feed invalid.

The release's file is taken only when its size and SHA-512 are the feed's: the download stops past the size, and nothing is opened before both match. HTTP redirects are followed by the OS's client; on Linux only to https (http too from loopback), on Windows never from https to http. The signature and the hash guard what comes.

### Keys

A public key is Ed25519's 32 bytes in 64 hex digits; the app holds a list of them, built in. The list must hold a key and only valid ones (a point on the curve, not of small order): one malformed key is the app's own mistake, and the client then reads no feed and fetches none. A private key is its 32-byte seed (RFC 8032) in 64 hex digits, given to the packer by `--key <file>` or the environment variable `SHIPO_UPDATE_KEY`, the release job's secret.

Each app has keys of its own: the feed doesn't name its app, and on Windows and Linux the library installs the file of any feed its keys verify.

The list may be of any length; two is the rule, made by `shipo keygen update.key update-spare.key` (a key per file): the working one, whose seed becomes the release job's secret, and the spare, whose seed is kept offline and never reaches CI. Both public keys go into the app, so every release trusts both. readme.md gives the steps.

When a private key leaks (the secret was read, a CI log printed it):

1. Make a new spare with `shipo keygen`, its seed offline.
2. In the app, replace the leaked public key with the new spare's: the app now holds the old spare and the new spare.
3. Set `SHIPO_UPDATE_KEY` to the old spare's seed and release. The installed apps trust the old spare, so they take this release, which no longer trusts the leaked key; the old spare is the working key from now on.

Until an install updates, it still accepts a feed the leaked key signs: release at once. On macOS a bundle must also meet the running app's code requirement ([macOS](#macos)), which the leaked key alone doesn't pass. A lost spare is replaced the same way, signed by the working key. With both lost, no feed reaches the installed apps: people install the next release by hand.

## Installing in place

The library replaces the installed app with the release's file, the one people download for the first install, after the app quits; it shows no window and takes no focus.

### Where the app is

| OS | installed | the target an update replaces |
|---|---|---|
| macOS | `<Name>.app`, where the user put it (`/Applications`) | the bundle of the running executable, `<X>.app/Contents/MacOS/<exe>` |
| Windows | `%LOCALAPPDATA%\Programs\<Name>\<Name>.exe`, by the setup | the running exe's folder when it is under `%LOCALAPPDATA%\Programs`, compared case-insensitively |
| Linux | the AppImage, where the user put it | `$APPIMAGE`, when the running executable is inside `$APPDIR` and `$APPIMAGE` is a file |

There is no target for a bare binary (a build), a Windows exe elsewhere, or a Linux binary not run as an AppImage (the .deb's in `/usr/bin`, which apt updates). A bundle macOS translocated (run from a read-only copy, as it does for an app downloaded and not moved), a target whose folder the user can't write (`/Applications` for a standard user, `/opt`), a target the user can't write itself (the swap moves the bundle to another folder, which rewrites its `..`), or a bundle on a volume without the swap (`VOL_CAP_INT_RENAME_SWAP`: APFS has it, HFS+ and exFAT don't) can't be updated either. The library tells both apart, so an app can hide its update up front.

### Staging

The target's home is its folder on macOS and Linux and the target itself on Windows (the app's folder, whose `.shipo-update-*` folders the uninstaller removes). The library downloads into a staging folder there, `.shipo-update-<pid>-<nanos>`, of the running app's pid and the time; only the user reaches it (0700 on Unix), and it is made, never reused. What it stages:

| OS | the file staged as | the new app | checked |
|---|---|---|---|
| macOS | `update.dmg` | the image attached read-only at `<staging>/volume`, unseen by Finder; its one `.app` at the top (none or more is invalid) copied with `ditto` to `<staging>/new/<X>.app`; the image detached, forced if it must be | the same bundle id as the running app, and signed so that it meets the running app's designated requirement for every architecture (Security.framework, as Sparkle's `SUCodeSigningVerifier`) |
| Windows | the last part of its URL's path, a plain `.exe` name that is not the running exe's | the setup as it came | the size and hash alone |
| Linux | `app.AppImage` | the file, made executable (0755) | the size and hash alone |

Before it stages, the library removes what an installer stopped midway left: each staging folder in the home whose app pid and installer pid (in its `ready` file, if any) both ended, and on macOS and Linux the target's old copy, `.<target name>.shipo-old`.

### The installer

The app starts the installer apart from itself (Unix: its own process group, on Linux with no descriptor past stderr inherited; Windows: no console, `DETACHED_PROCESS | CREATE_NO_WINDOW`, out of the app's job where the job lets it), its standard streams null:

```text
<installer> --shipo-install <pid> <new> <target> -- <the app's arguments…>
```

`<pid>` is the app's, `<new>` the staged app, `<target>` the absolute target. The installer is the new version on macOS (`<new>/Contents/MacOS/<the running exe's name>`, so a release keeps its executable's name) and on Linux (the new AppImage); on Windows, where the setup replaces the exe, it is a copy of the running exe at `<staging>\<the exe's name>` (named as it is: Windows asks to elevate an exe named `setup`, `update` or `install` that has no manifest).

The app waits up to 10 s for the installer's sign, the file `<staging>/ready` holding the installer's pid; with none it kills the installer, nothing changed. Then the app quits, within 30 s.

An app's `main` first checks for `--shipo-install` as its first argument, before it parses its own or opens a window; then it is the installer, and it never returns:

1. Its arguments must be the line above, `<target>` absolute, `<new>` inside a `.shipo-update-*` folder directly in the target's home, by plain path parts; else it exits with 2.
2. It writes `ready`, then waits up to 30 s for the app to end: on Unix until its parent changes (the app is its parent, so a reused pid can't fool it), on Windows on the process handle it opened before writing `ready`. If the app runs on, it changes nothing and exits with 1.
3. It puts the new app in:
   - macOS: the new and the old bundle exchanged in one step, `renamex_np` with `RENAME_SWAP` (APFS), as Sparkle's `SUFileManager` does.
   - Windows: the setup run silent (`/S`) into the target (`/D=<target>`, last and unquoted), so a moved or second copy updates itself, with no window, to its end; an exit code other than 0 is the error.
   - Linux: the target hard-linked as `.<target name>.shipo-old`, then the new AppImage renamed over the target in one step.
4. It starts the target with the app's arguments, in the app's working directory, and `SHIPO_UPDATE_ERROR` removed from its environment:
   - macOS: `open -n -g <target> --args …`, by LaunchServices: no focus taken, launchd's environment, `/` as the working directory.
   - Windows: the target folder's exe of the running exe's name, with no window of its own.
   - Linux: the AppImage, without the installer's `APPIMAGE`, `APPDIR`, `ARGV0` and `OWD`, which its runtime sets again.
5. If 3 or 4 fails, it undoes the swap (macOS exchanges again, Linux renames the old copy back) and starts the old app the same way with the error's text in `SHIPO_UPDATE_ERROR`.
6. It removes the old copy and the staging folder; on Windows, where it runs from the staging folder, all in it but its own exe, which the next download removes. It exits with 0, or 1 on a failure.

Run without `--shipo-install`, the app takes `SHIPO_UPDATE_ERROR` as the error of the installer that started it, to show once; the variable stays in its environment. On Windows it also removes the ended staging folders a few seconds in, as its installer couldn't.

Linux's rename of the new AppImage over the target, the undoing of a swap and the removals are tried again for up to 5 s while their error may pass: a folder macOS lists as not empty while it is emptied, a file on Windows the app that just quit still holds open.

## macOS

The DMG is a UDIF image compressed with zlib (UDZO) of an HFS+ volume named `<Name>`, holding `<Name>.app`, an `Applications` symlink to `/Applications` and the `.DS_Store` that lays out its window. The packer sets the bundle's modes: folders and files with an execute bit 0755, other files 0644. The app and the image are signed by a Developer ID, the image notarized and stapled, so the first launch of a downloaded copy passes Gatekeeper.

Every release must be signed by one stable identity: the library takes a bundle only when it meets the running app's designated requirement, and an ad-hoc signature's requirement is its own build, so an ad-hoc app takes no update. That is also what keeps the user's grants (Accessibility, the microphone) through an update. A release keeps the bundle id (the app's `id`) and the executable's name. The app runs outside the App Sandbox: a sandboxed app can't replace its own bundle, so the packer refuses entitlements with `com.apple.security.app-sandbox` on.

## Windows

The setup is our own per-user installer: a small Windows program, the stub (`setup/`, one per architecture, built with the static C runtime, no console), with the app appended as its payload. It asks for no administrator (its manifest's `asInvoker`). It installs into the folder `/D=<folder>` gives, else where the app is (its key's `InstallLocation`, when it is such a folder as `/D=` takes), else `%LOCALAPPDATA%\Programs\<Name>`: `<Name>.exe`, `Uninstall.exe` and, when the app has one, `LICENSE.txt`. An `Uninstall.exe` there already is replaced when it is this app's (ours, by its payload's app id, or any other where the key put this app, as an NSIS setup's); one of another app fails the setup, which then changed nothing. It installs over a previous NSIS install of the same app id: its `LICENSE.txt` goes when this setup has none, and the key is written anew. `/D=` is the last argument, the rest of the command line as it is, unquoted, its spaces kept, as NSIS setups take it, and an absolute folder below a drive's or a share's root (`C:\…` or `\\server\share\…`, never the root itself), else the setup refuses it. A newer setup installs over an older one, file by file, each written beside and renamed over the old; a file in use (the exe of an app that just quit) is tried again for 5 s, and a read-only one is made writable first, as NSIS does. Then it writes the key below, removed first so no value of an install before stays, then the Start menu entry, `<Name>.lnk`, whose failure (a folder past `MAX_PATH`) fails nothing; its description is the app's comment, cut to 1023 UTF-16 units.

- `/S` installs silently: no window, no message, the app not started. Without it the setup is a wizard as an NSIS MUI2 setup's, in the system's message font at the screen's DPI, its layout scaled by that font as dialog units are, the app's icon on its title bar, its banner and its header: Welcome; Choose Install Location, the folder above in a box with Browse (the shell's folder picker, the app's name added to the folder picked unless it ends with it), the space required and available, the folder taken only as `/D=` takes one, and fixed for an update (no `/D=`, the key's folder), whose old copy would stay; Installing, a bar and a log of its steps, Next once done; Finish, with "Run <Name>" checked and "Create desktop shortcut" not, the shortcut `<Name>.lnk` on the user's desktop as the Start menu's, never over one of that name to anything else. Cancel, Esc or the close box before Installing ends the setup with nothing changed, its exit code 1; during it they do nothing; on Finish they close it, nothing run or made. An error shows in a message box; an app that fails to start shows its error too, the install done. Its exit code is 0 when installed, 2 when it failed: another architecture than Windows' (an x64 setup runs on x64 and arm64 Windows, which emulates it; an arm64 one on arm64), a `/D=` not such a folder, a file still in use after 5 s, a damaged payload ("download it again"), any other error. It loads DLLs from System32 only, never from its own folder (Downloads), and never shows Windows' own "No disk" box.
- It is never named `setup.exe`, into which Windows loads compatibility shims.
- It writes the key `HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{<AppId>}` for Settings › Apps: `DisplayName`, `DisplayVersion`, `Publisher`, `DisplayIcon`, `InstallLocation`, `UninstallString`, `QuietUninstallString` (`/S`), and the DWORDs `NoModify` 1, `NoRepair` 1 and `EstimatedSize` in KiB. `<AppId>` is the config's `windows.app-id`, else the UUID v5 of the app's `id` in the DNS namespace; it never changes, so a newer setup replaces the install.
- The uninstaller is the stub with the app's name and ids alone as its payload, and whether its setup wrote `LICENSE.txt`; its PE security directory is cleared, as a signed setup's signature doesn't cover it. Without `/S` it is a wizard as the setup's: a confirm page with the install folder, Uninstall and Cancel (Cancel, Esc or the close box end it with nothing changed, its exit code 1), Uninstalling with a bar and its log, then Finish; an error shows in a message box, which `/S` keeps quiet, and it exits as the setup does. It runs only as `Uninstall.exe` in the folder the key's `InstallLocation` names, when the key has one, else it fails changing nothing: the copy it leaves in the temp folder, started again, would take that folder for the app's. It runs in the temp folder, not its own, which then can go. It removes `<Name>.exe` first, tried for 5 s, so a running app stops it before anything changed; then it moves itself out to the temp folder (or, on another volume, the install folder's parent, never over a file there), where it can run on; then, each tried however the others went, so no key is left of an app gone: `LICENSE.txt` when its setup wrote one (a named file only as a file, never a folder of its name), every `.shipo-update-*` entry in the install folder, the folder when it is empty then (a `/D=` may share it), the Start menu entry and `<Name>.lnk` on the desktop when it points to `<Name>.exe`, the app's launch at login (the values named by the app's `id` in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` and `…\Explorer\StartupApproved\Run`) and its key. It keeps the app's data; its own exe stays where it moved.

## Linux

The AppImage is the type2 runtime with a squashfs appended, of an AppDir holding `AppRun` (a link to `usr/bin/<bin>`), `usr/bin/<bin>`, `<id>.desktop`, `<id>.png` and `.DirIcon`. The runtime needs no libfuse2 and sets `APPIMAGE` and `APPDIR` for the app. The `.desktop` entry's `StartupWMClass` is `<id>`, and the app sets its window's Wayland app id and X11 WM_CLASS to `<id>`, so a dock matches the window to the entry.

The .deb installs `/usr/bin/<bin>`, the `.desktop` entry and the icon. Its `Depends` is the config's list, with `libc6 (>= <x.y>)` first when the list names no `libc6`; `<x.y>` is the newest `GLIBC_<x.y>` in the binary's version needs (`.gnu.version_r`), which the packer prints: the binary runs on that glibc or newer. Its version is the app's without the build metadata (`+…`), with the first `-` as `~` and any later one as `.`, and `~dev` after a dev build's, so apt orders both before the release: `1.2.0-rc-3+b5` is `1.2.0~rc.3`.
