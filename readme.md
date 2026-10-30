<div align="center">

# shipo

**Releases and updates for desktop apps**

[<img src="https://badges.ws/crates/v/shipo" alt="version" />](https://crates.io/crates/shipo)
[<img src="https://badges.ws/crates/docs/shipo" alt="docs" />](https://docs.rs/shipo)
[<img src="https://badges.ws/github/license/vladkens/shipo" alt="license" />](https://github.com/vladkens/shipo/blob/main/LICENSE)
[<img src="https://badges.ws/badge/follow-@uid127-black" alt="follow @uid127" />](https://x.com/uid127)

</div>

The `shipo` command turns your app's built binaries into installers for macOS, Windows and Linux, plus a signed update feed. The `shipo` crate runs inside your app and updates it in place from that feed. One Linux machine or Mac packs every platform.

## 🌟 Features

- 📦 **Every installer from one machine**: DMG, Windows setup, AppImage and .deb
- 🍏 **Builds and signs the DMG on Linux**: a Mac is optional
- 🧰 **Runs with just `curl`** on Linux or a Mac (and NSIS for the Windows setup)
- 🪟 **Installs for the current user** on Windows, with a Start menu entry
- 🪶 **A tiny updater**: 1 dependency on Linux, 3 on Windows, 8 on macOS
- 🔐 **Signed updates**, with a spare key to replace a leaked one
- 🔄 **Updates silently after the app quits**, restarts it and reports any failure
- 📄 **A plain-text feed** any client can read, described in [spec.md](spec.md)

## ⚡ Quick start

```sh
cargo install --locked shipo
```

1. Make the feed's keys: `shipo keygen update.key update-spare.key` ([Keys](#-keys)).
2. Add the `shipo` crate to your app with both public keys ([Updater](#-updater)).
3. Put the built binaries in `dist/` and pack a release: `shipo release dist --tag v1.0.0` ([Packer](#-packer)).

## 🔑 Keys

The feed is signed with Ed25519. Your app holds two public keys and takes a feed signed by either: the working key signs every release, the spare stands by to replace it.

```sh
shipo keygen update.key update-spare.key
```

It writes each private key to its file, readable by you alone, and prints both public keys for your app. Then:

1. Set `update.key` as the release job's secret: `gh secret set SHIPO_UPDATE_KEY < update.key`.
2. Keep `update-spare.key` in your password manager, out of CI.
3. Delete both files.

`shipo keygen --public <file>` prints a key file's public key, to tell which is which.

### Replacing a key

When the working key leaks, the spare signs the release that drops it:

1. Make a new spare, `shipo keygen update-spare-2.key`, and keep it in your password manager.
2. In the app, replace the leaked public key with the new spare's.
3. Set `SHIPO_UPDATE_KEY` to the old spare and release at once: the installed apps trust it, so they take the release that no longer trusts the leaked key. The old spare is the working key from now on.

A leaked or lost spare is replaced the same way, with the working key signing. With both keys lost, people download the next release by hand.

## 📚 Updater

```sh
cargo add shipo --no-default-features
```

```rust
/// The feed and the public keys `shipo keygen` printed: the working one and the spare.
const FEED: shipo::Feed = shipo::Feed::new(
  "https://github.com/me/hello/releases/latest/download/update.txt",
  &["<working public key>", "<spare public key>"],
);

fn main() {
  // First, before the arguments and any window. Your app's binary is also its installer:
  // started by an update, `finish` installs it and exits; otherwise it returns.
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

The installer waits for the app to quit, puts the new version in and restarts it with the same arguments. If that fails, macOS and Linux put the old version back and Windows reruns the app in its folder; the next `finish` returns the error. `shipo::supported()` tells up front whether this install can update in place, and [examples/update.rs](examples/update.rs) is a console app that updates itself.

The updater uses the system's HTTP; the dependency counts above leave out 6 crates that derive the error type at build time.

## 📦 Packer

shipo packs on Linux (x86_64 or aarch64) or an Apple silicon Mac. It needs `curl`, and NSIS for the Windows setup (`apt install nsis`, `brew install makensis`); on a Mac it also uses the tools macOS ships.

Your app's `Cargo.toml` describes the app:

```toml
[package]
name = "hello"
version = "1.2.0"
authors = ["Jane Doe <jane@example.com>"]
repository = "https://github.com/me/hello"

[package.metadata.shipo]
id = "com.example.hello"
name = "Hello"
icon = "assets/icon.png"
comment = "Says hello"
publisher = "Example Labs"

[package.metadata.shipo.macos]
accessory = true
entitlements = "assets/hello.entitlements"
plist.NSMicrophoneUsageDescription = "Hello hears you."

[package.metadata.shipo.windows]
app-id = "7d3f1c2a-5b8e-4f61-9a0d-2c4e6b8f1a35"
copyright = "© 2026 Example Labs"

[package.metadata.shipo.linux]
categories = ["Utility"]
deb-package = "hello"
deb-depends = ["libasound2t64 | libasound2"]
deb-recommends = ["libgtk-3-0"]
```

shipo reads these from `[package]`, or from `[workspace.package]` for `version`, `authors` and `repository`:

- `version`: the release's version, so its tag is `v1.2.0`; macOS and Windows keep its numbers (1.2.0 of 1.2.0-beta.1)
- `authors`: the first is the publisher by default; the .deb needs one with a mail, `Name <mail>`
- `repository`: the GitHub repository whose release the feed links, for `--tag`
- `name`, `default-run`, the bin targets: the binary to pack, the only one, else `default-run`, else the one named as the package

`[package.metadata.shipo]` takes these keys, any other is an error; paths are from the `Cargo.toml`'s folder:

| Key | Type | Default | What it does |
|---|---|---|---|
| `id` | string | required | the reverse-DNS bundle id: never change it |
| `name` | string | required | the name people see, and the files' prefix |
| `icon` | path | required | a square PNG, 1024 px or more |
| `comment` | string | none | a line about the app, for the menus and the .deb |
| `publisher` | string | the first author, else from `id` | who makes the app, on Windows and in the .deb |

`[package.metadata.shipo.macos]`:

| Key | Type | Default | What it does |
|---|---|---|---|
| `accessory` | bool | `false` | a menu bar app, without a Dock icon |
| `entitlements` | path | none | the entitlements plist to sign with |
| `plist` | table | none | Info.plist keys to add or override, like usage strings |

`[package.metadata.shipo.windows]`:

| Key | Type | Default | What it does |
|---|---|---|---|
| `app-id` | string, a GUID | made from `id` | the setup's AppId: never change it |
| `copyright` | string | `© <publisher>` | the exe's and setup's copyright |

`[package.metadata.shipo.linux]`:

| Key | Type | Default | What it does |
|---|---|---|---|
| `categories` | list | `["Utility"]` | the app menu's categories |
| `deb-package` | string | the binary's name, lowercase, `_` as `-` | the .deb's package name |
| `deb-depends` | list | `libc6 (>= <the binary's glibc>)` | the packages the .deb needs; shipo adds that `libc6` unless you list one |
| `deb-recommends` | list | none | the packages the .deb recommends |

Put the release builds, x86_64 or aarch64, in `dist/<target>/<bin>[.exe]`, such as `dist/aarch64-apple-darwin/hello`, and run:

```sh
shipo release dist                   # the installers of a dev build, Hello-dev-*
shipo release dist --tag v1.2.0      # a release, Hello-v1.2.0-*: signed, notarized, and update.txt
shipo feed --url https://example.com/hello/{version} dist/*.dmg dist/*-setup.exe dist/*.AppImage
shipo app --bin target/release/hello # the signed Hello.app alone, to try it on a Mac
```

It writes the installers into `dist/`, and with `--tag` the signed feed, `update.txt`:

| File | Updated by |
|---|---|
| `Hello-dev-macos-aarch64.dmg` | shipo |
| `Hello-dev-windows-x64-setup.exe` | shipo |
| `Hello-dev-linux-x86_64.AppImage` | shipo |
| `Hello-dev-linux-x86_64.deb` | apt or the user |

With `--tag v1.2.0`, `v` and the app's version, the files are `Hello-v1.2.0-…` and the feed links them in that GitHub release: upload them as they are. GitHub keeps only `A-Z a-z 0-9 . _ -` in file names, so a release's `name` takes only these.

`shipo feed` signs a feed for files served elsewhere and writes it to `target/distrib/update.txt`. `shipo --help` lists every option.

macOS releases need a paid Apple Developer ID, and an App Store Connect API key for notarization. A release takes these secrets:

| Variable | What |
|---|---|
| `SHIPO_UPDATE_KEY` | the feed's private key, see [Keys](#-keys); or `--key <file>` |
| `APPLE_CERTIFICATE` | the Developer ID Application certificate, a base64 `.p12` (OpenSSL 3 exports one with `-legacy`) |
| `APPLE_CERTIFICATE_PASSWORD` | its password |
| `APPLE_API_KEY` | the App Store Connect API key, the `.p8`'s text, for notarization |
| `APPLE_API_KEY_ID` | its key id |
| `APPLE_API_ISSUER` | its issuer id |

On Linux shipo signs the app and the DMG with the certificate and notarizes the DMG. On a Mac the keychain's Developer ID signs, and the `APPLE_CERTIFICATE*` secrets go unused. On macOS the updater installs only a release signed with the same Developer ID as the running app.

A dev build, without `--tag`, needs no secrets. The app is signed ad hoc on Linux, or on a Mac by a local identity shipo creates in the login keychain. The DMG is notarized only when the `APPLE_API_*` secrets are set.

## 🚀 Releasing from GitHub Actions

Build the binaries on each OS, then pack them on Ubuntu, where the secrets are; the apps find the feed at `https://github.com/me/hello/releases/latest/download/update.txt`:

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
      - uses: actions/checkout@v7
      - run: cargo build --release --target ${{ matrix.target }}
      - uses: actions/upload-artifact@v7
        with:
          name: ${{ matrix.target }}
          path: target/${{ matrix.target }}/release/hello${{ runner.os == 'Windows' && '.exe' || '' }}

  release:
    needs: build
    runs-on: ubuntu-latest
    permissions: { contents: write }
    steps:
      - uses: actions/checkout@v7
      - uses: actions/download-artifact@v8
        with: { path: dist } # dist/<target>/hello[.exe]
      - run: sudo apt-get update && sudo apt-get install -y nsis && cargo install --locked shipo
      - run: shipo release dist --tag "$GITHUB_REF_NAME"
        env:
          SHIPO_UPDATE_KEY: ${{ secrets.SHIPO_UPDATE_KEY }}
          APPLE_CERTIFICATE: ${{ secrets.APPLE_CERTIFICATE }}
          APPLE_CERTIFICATE_PASSWORD: ${{ secrets.APPLE_CERTIFICATE_PASSWORD }}
          APPLE_API_KEY: ${{ secrets.APPLE_API_KEY }}
          APPLE_API_KEY_ID: ${{ secrets.APPLE_API_KEY_ID }}
          APPLE_API_ISSUER: ${{ secrets.APPLE_API_ISSUER }}
      - run: gh release create "$GITHUB_REF_NAME" dist/Hello-* dist/update.txt
        env: { GH_TOKEN: "${{ github.token }}" }
```


## 🤝 Contributing

Pull requests are welcome, including ones made with agents: point yours at [agents.md](agents.md).

## 📝 License

Distributed under the [MIT License](LICENSE).
