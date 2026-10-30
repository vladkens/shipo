<div align="center">

# shipo

**Releases and updates for desktop apps**

[<img src="https://badges.ws/github/license/vladkens/shipo" alt="license" />](https://github.com/vladkens/shipo/blob/main/LICENSE)
[<img src="https://badges.ws/badge/follow-@uid127-black" alt="follow @uid127" />](https://x.com/uid127)

</div>

`shipo` ships a desktop app from the binaries you built and keeps it up to date. It has two halves, both the one crate `shipo`:

1. **The packer**, the `shipo` command: your app's binaries for macOS, Windows and Linux into a signed and notarized DMG, a per-user Windows setup, an AppImage and a .deb, plus a signed `update.txt` feed. It runs on one Linux host (or a Mac), with nothing installed but `curl` and NSIS: it writes the DMG (HFS+, no `hdiutil`), the squashfs and the .deb itself.
2. **The updater**, the `shipo` library with `default-features = false`: inside your app, it checks the signed feed, downloads the release, verifies it and installs it in place after the app quits. It has no UI of its own: your app draws the menu item and the progress. The feed and the install are a written contract, [spec.md](spec.md), so any updater can read the feed.

shipo is early (v0.0.x) and not on crates.io yet: install it from git, and expect the API and the CLI to change.

## 🌟 Features

- 📦 **One host, every installer**: a signed and notarized **DMG**, a **per-user setup** for Windows, an **AppImage** and a **.deb**, for x86_64 and aarch64, from one Ubuntu job or a Mac
- 💿 **A DMG without a Mac**: shipo writes the HFS+ volume and the UDIF image itself and signs and notarizes them through rcodesign; its tests check the image with `hdiutil` and `fsck_hfs`
- 🧰 **Nothing to install** but `curl` and NSIS: no `hdiutil`, `mksquashfs`, `dpkg-deb` or OpenSSL; rcodesign and the AppImage runtime are fetched pinned by SHA-256. The same binary packs the same AppImage and .deb bytes
- 🪶 **A tiny updater**: 7 dependency crates on Linux, 9 on Windows, 14 on macOS, and 6 of them are thiserror's derive, done at build time. **No OpenSSL, no HTTP crate**: it uses the OS's own HTTP, NSURLSession, WinHTTP and libcurl loaded at run time
- 🔐 **An Ed25519-signed feed**, with a **spare key** built into the app and [steps for a leaked key](#replacing-a-key); each download is checked by its size and SHA-512, and on macOS by the app's code signature
- 🔄 **In place, after the app quits**: the bundle swapped in one step on macOS, the setup run silently on Windows, the AppImage replaced on Linux; the app restarts with its arguments, and a failed install puts the old version back. **No window, no focus taken**
- 🪟 **No admin on Windows**: the setup installs into `%LOCALAPPDATA%\Programs`, with a Start menu entry and an entry in Settings › Apps
- 📄 **A plain-text feed**: one line per platform, readable by any client ([spec.md](spec.md)), hosted in the GitHub release or anywhere else

## 📚 The updater

```sh
cargo add shipo --no-default-features --git https://github.com/vladkens/shipo
```

```rust
/// The feed and the public keys `shipo keygen` printed: the working one and the spare.
const FEED: shipo::Feed = shipo::Feed::new(
  "https://github.com/me/hello/releases/latest/download/update.txt",
  &["<working public key>", "<spare public key>"],
);

fn main() {
  // First, before the arguments and any window: started by an update, this is the installer and exits.
  if let Some(e) = shipo::finish() {
    eprintln!("the update failed: {e}"); // the installer's error, once, to show in your UI
  }

  // The app.
}

/// On a thread of the app's own, when the user asks; the app quits after it, within 30 s.
fn update() -> shipo::Result<()> {
  let Some(release) = shipo::check(&FEED, env!("CARGO_PKG_VERSION"))? else { return Ok(()) };
  let staged = shipo::download(&release, |done, total| println!("{done} of {total}"))?;
  shipo::install(staged)
}
```

`check` fetches the feed, verifies its signature and gives the newer `Release` for this platform (its `version`, `notes` page, `url` and `size`), or `None`. `download` fetches the file beside the running app, checks it and stages it; `install` starts the installer and returns, and the app quits. The installer waits for it to end, puts the new version in and starts it with the same arguments; if that fails, it starts the old one, whose `finish` gives the error. The calls block, so run them off the UI thread. `shipo::supported()` tells up front, with no network, whether this app can update in place: not a build run from `target/`, an app installed by the .deb (apt updates it), a macOS app run where it was downloaded (macOS translocates it), or one in a folder the user can't write.

Every error is a `shipo::Error`: `Http`, `Signature`, `Mismatch` (the download isn't the feed's), `Unsupported` (a dev build such as `0.1.0-5-gabc`, or no installed app), and more; it is `#[non_exhaustive]`. On macOS, every release must be signed by the same Developer ID: the updater refuses a bundle that doesn't meet the running app's signature requirement, and an ad-hoc signed app takes no update. [examples/update.rs](examples/update.rs) is a console app that updates itself.

## 📦 The packer

```sh
cargo install --locked --git https://github.com/vladkens/shipo
```

Your app's `Cargo.toml` describes it:

```toml
[package]
name = "hello"
version = "1.2.0"
repository = "https://github.com/me/hello"  # where --tag links the feed's files

[package.metadata.shipo]
id = "com.example.hello"   # the bundle id, the .desktop's StartupWMClass
name = "Hello"             # the name people see and the files' prefix
icon = "assets/icon.png"   # a 1024 px PNG
comment = "Says hello"

[package.metadata.shipo.macos]
accessory = true                      # no Dock icon
entitlements = "assets/hello.entitlements"
plist.NSMicrophoneUsageDescription = "Hello hears you."

[package.metadata.shipo.windows]
app-id = "…"                          # the setup's GUID, else derived from `id`

[package.metadata.shipo.linux]
categories = ["Utility"]
deb-depends = ["libc6 (>= 2.35)"]
deb-recommends = ["libgtk-3-0"]
```

Put the release builds in `dist/<target>/<bin>[.exe]` (`dist/aarch64-apple-darwin/hello`, `dist/x86_64-pc-windows-msvc/hello.exe`, `dist/x86_64-unknown-linux-gnu/hello`, any of them) and run:

```sh
shipo release dist                   # the installers of a dev build, Hello-dev-*
shipo release dist --tag v1.2.0      # a release: signed, notarized, and update.txt
shipo feed --url https://example.com/hello/{version} dist/*.dmg dist/*-setup.exe dist/*.AppImage
shipo app --bin target/release/hello # the signed Hello.app alone, to try it on a Mac
```

```text
dist/Hello-dev-macos-aarch64.dmg
dist/Hello-dev-windows-x64-setup.exe
dist/Hello-dev-linux-x86_64.AppImage
dist/Hello-dev-linux-x86_64.deb
dist/update.txt
```

With `--tag` the tag must be `v` and the app's version, and the feed links the files of that GitHub release by the names they get there, `Hello-v1.2.0-…`: rename `-dev-` to the tag as you upload them. The .deb isn't in the feed: apt updates it. `shipo feed` signs a feed for files served elsewhere, into `target/distrib/update.txt`. `shipo --help` lists every command and option.

The host is Linux (x86_64 or aarch64) or a Mac with Apple silicon, with `curl` and, for the Windows setup, NSIS (`apt install nsis`, `brew install makensis`). A release takes these secrets as environment variables:

| Variable | What |
|---|---|
| `SHIPO_UPDATE_KEY` | the feed's private key, see [Keys](#-keys); or `--key <file>` |
| `APPLE_CERTIFICATE` | the Developer ID Application certificate, a base64 `.p12` (OpenSSL 3 exports one rcodesign reads with `-legacy`) |
| `APPLE_CERTIFICATE_PASSWORD` | its password |
| `APPLE_API_KEY` | the App Store Connect API key, the `.p8`'s text, for notarization |
| `APPLE_API_KEY_ID` | its key id |
| `APPLE_API_ISSUER` | its issuer id |

Off a Mac, rcodesign signs the app and the DMG with the certificate; on a Mac, `codesign` signs with the keychain's Developer ID and the `APPLE_CERTIFICATE*` secrets aren't used. rcodesign notarizes and staples the DMG either way. Without `--tag` they are optional: with none, the app is signed ad hoc (on a Mac, by a local identity shipo makes) and the DMG isn't notarized.

## 🔑 Keys

The feed is signed with Ed25519. The app holds the public keys, built in, and takes a feed signed by any of them; the private keys live only in the release job's secret and in your password manager. Make two, the working key and a spare:

```sh
shipo keygen update.key update-spare.key
```

It writes each private key, its 32-byte seed in hex, to its file (readable by you alone, never over a file that is there) and prints each public key. Then:

1. Both public keys go into the app: `shipo::Feed::new("<feed url>", &["<working>", "<spare>"])`.
2. `update.key` becomes the release job's secret, given to `shipo` as the environment variable `SHIPO_UPDATE_KEY`; on GitHub, `gh secret set SHIPO_UPDATE_KEY < update.key`.
3. `update-spare.key` goes to your password manager, never to CI.
4. Delete both files.

`shipo keygen --public <file>` prints a private key's public half, to tell which key is which. The app may hold any number of keys, but each is one more secret that can sign a feed: two is the rule, one that signs and one in reserve.

### Replacing a key

When the working key leaks (the secret was read, a log printed it), the spare signs the release that drops it:

1. Make a new spare, `shipo keygen update-spare-2.key`, and keep it in your password manager.
2. In the app, replace the leaked public key with the new spare's: the app now holds the old spare and the new one.
3. Set `SHIPO_UPDATE_KEY` to the old spare and release at once. The installed apps trust the old spare, so they take this release, which trusts the leaked key no more; the old spare is the working key from now on.

Until an install takes that release, a feed the leaked key signs still reaches it: release at once. On macOS the new bundle must also be signed by the app's Developer ID, which the leaked key alone doesn't pass. A leaked or lost spare is replaced the same way, the release signed by the working key; a key is retired with nothing leaked the same way too. With both keys lost, no feed reaches the installed apps: people download the next release by hand.

## 🚀 Releasing from GitHub Actions

Build the binaries on each OS, then pack them on Ubuntu, where the secrets are:

```yaml
on:
  push:
    tags: ["v*"]

jobs:
  build:
    strategy:
      matrix:
        include:
          - { os: macos-latest, target: aarch64-apple-darwin }
          - { os: windows-latest, target: x86_64-pc-windows-msvc }
          - { os: ubuntu-latest, target: x86_64-unknown-linux-gnu }
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - run: cargo build --release --target ${{ matrix.target }}
      - uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.target }}
          path: target/${{ matrix.target }}/release/hello${{ runner.os == 'Windows' && '.exe' || '' }}

  release:
    needs: build
    runs-on: ubuntu-latest
    permissions: { contents: write }
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
        with: { path: dist } # dist/<target>/hello[.exe]
      - run: sudo apt-get install -y nsis && cargo install --locked --git https://github.com/vladkens/shipo
      - run: shipo release dist --tag "$GITHUB_REF_NAME"
        env:
          SHIPO_UPDATE_KEY: ${{ secrets.SHIPO_UPDATE_KEY }}
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_API_KEY: ${{ secrets.APPLE_API_KEY }}
          APPLE_API_KEY_ID: ${{ secrets.APPLE_API_KEY_ID }}
          APPLE_API_ISSUER: ${{ secrets.APPLE_API_ISSUER }}
      - run: | # the names the feed lists
          cd dist && for f in Hello-dev-*; do mv "$f" "${f/-dev-/-$GITHUB_REF_NAME-}"; done
          gh release create "$GITHUB_REF_NAME" Hello-v* update.txt
        env: { GH_TOKEN: "${{ github.token }}" }
```

The apps then find the feed at `https://github.com/me/hello/releases/latest/download/update.txt`.

## 🤝 Contributing

Pull requests are welcome, including ones made with agents: point yours at [agents.md](agents.md).

## 📝 License

Distributed under the [MIT License](LICENSE).

## 🔍 See also

- [sparkle-project/Sparkle](https://github.com/sparkle-project/Sparkle) – The macOS updater whose code signature check shipo's follows; macOS only.
- [velopack/velopack](https://github.com/velopack/velopack) – Installers and updates for Windows, macOS and Linux, with delta updates.
- [crabnebula-dev/cargo-packager](https://github.com/crabnebula-dev/cargo-packager) – Installers for Rust apps and an updater, from the Tauri ecosystem.
