# shipo

Releases and updates for desktop apps.

- `shipo` (the packer, `src/cli/`): an app's bare binaries for macOS, Windows and Linux into a signed and notarized DMG, a per-user setup, an AppImage and a .deb, and a signed `update.txt`, all on one Ubuntu host.
- `shipo` (the crate, `src/`): inside the app, checks the feed, downloads the update and installs it in place after the app quits, with no window.

`spec.md` describes the feed and the install contract between them.

Both are the one crate `shipo`. An app takes the library alone, without the packer's dependencies: `shipo = { version = "…", default-features = false }`. The packer installs with `cargo install shipo`, or from the release binaries.

Work in progress.

## Packing

The app's `Cargo.toml` describes it:

```toml
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

With the release builds in `dist/<triple>/<bin>[.exe]`, `shipo release dist --tag v1.2.0` writes the installers and the signed `update.txt` into `dist/`, linked to the GitHub release `v1.2.0`; without `--tag` it writes the installers of a dev build. `shipo feed` signs a feed for files served elsewhere. `shipo --help` lists every command and option. The packer writes the AppImage and the .deb itself; the host needs `curl` and, for the Windows setup, `nsis` from apt; off a Mac, the app is signed by rcodesign with the `APPLE_CERTIFICATE` (a base64 `.p12`, which OpenSSL 3 exports for it with `-legacy`) and `APPLE_CERTIFICATE_PASSWORD` secrets. The DMG, also the packer's own, is signed as the app is; a release's is notarized and stapled by rcodesign with the App Store Connect API key's secrets `APPLE_API_KEY` (the `.p8`), `APPLE_API_KEY_ID` and `APPLE_API_ISSUER`.

## Keys

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

## Contributing

Pull requests are welcome, including ones made with agents: point yours at [agents.md](agents.md).
