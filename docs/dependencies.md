# Dependencies

Why each crate is in, and why the packer writes some formats itself. A crate comes in only where our own code would be large; every app builds the library's, so it takes as few as it can. Counts are crates in `cargo tree -e normal`, the crate itself included, measured on 2026-10-30.

## Library (`src/`)

| Crate | For | Why |
|---|---|---|
| `ed25519-compact` | the feed's signature, SHA-512 | no dependencies, `default-features = false` |
| `objc2` family (macOS) | NSURLSession, Security.framework | the system's own HTTP and code-signing check |
| `windows-sys` (Windows) | WinHTTP, processes | the system's own HTTP |
| none (Linux) | libcurl, loaded at run time | no link-time dependency |

## Packer (`cli/`): taken

| Crate | For | Why |
|---|---|---|
| `toml` (`parse`, `serde`) | the app's `Cargo.toml` | 5 crates; reads the file itself, so cargo isn't needed on the host and a `shipo.toml` can come later |
| `png` | any PNG read and written | ~9 crates; every PNG colour type and depth |
| `lexopt` | the arguments | no dependencies; clap is far heavier |
| `ed25519-compact` | signing the feed, `keygen` | the library's, no dependencies |
| `flate2` | zlib and gzip (DMG, .deb, squashfs, tarballs) | already in the tree through `png`; adds nothing |
| `hmac-sha256` | SHA-256 of the pinned tools | no dependencies; `sha2` is 10 crates |
| `tar` (no default features) | tarballs read and written | 3 crates (`tar`, `filetime`, `libc`); replaced ~340 lines of our own ustar code |

## Packer: written ourselves instead

| Format or task | Crates looked at | Why our own |
|---|---|---|
| the config | `cargo metadata` + a JSON crate | needs cargo on the host; `toml` serves `shipo.toml` too |
| SHA-256 | `sha2` | 10 crates for what `hmac-sha256` does with none |
| .deb | `cargo-deb` (65 crates), `ar` (last release 2021) | an `ar` of two tarballs: ~100 lines (`deb.rs`); deb(5) |
| squashfs (AppImage) | `backhand` (36–54 crates, zstd and xz in C) | ~470 lines of gzip blocks (`squashfs.rs`), which the type2 runtime reads; the kernel's format docs. zstd later through a pure-Rust encoder if the size matters |
| HFS+ (DMG) | `fstool` (one author, first release 2026-10, 48 crates), `hfsplus` and `dmgwiz` (read only) | no mature crate writes HFS+; written from Apple's TN1150 |
| UDIF (DMG) | `apple-dmg` (46 crates, FAT32 inside, not HFS+) | ~140 lines (`udif.rs`) |
| the Mach-O minimum macOS | — | a few load commands |
| PE resources (icon, version, manifest) | — | one `.rsrc` section (`pe.rs`) |
| HTTP fetch | `ureq` (27 crates), `minreq` (12), both with `ring` in C and assembly | the system's `curl`, on macOS, Windows 10+ and Ubuntu |
| .p12 to PEM | `p12-keystore` (79 crates), `pkcs12` (parses only) | rcodesign reads the .p12 itself (`--p12-file`); a modern AES .p12 needs a `-legacy` export |
| Apple code signing | `apple-codesign` (395 crates, MPL-2.0) | rcodesign's release binary, pinned by SHA-256 |

## Host tools still used

| Tool | Where | Why |
|---|---|---|
| `curl` | fetching the pinned tools | on every host already |
| rcodesign 0.29.0 | signing and notarizing off a Mac | fetched, pinned for Linux x86_64 and aarch64 and macOS arm64, the packer's hosts |
| `codesign` | signing on a Mac with the keychain's identity | the system's |
| `makensis` | the Windows setup | apt's `nsis` or brew's `makensis`; a setup of our own (a Rust stub, as Velopack's, ~800 lines) would drop it, when the packer runs on Windows hosts, with the `spec.md` change approved |

Sources never ported, being GPL or APSL: squashfs-tools, dpkg, libdmg-hfsplus, hfsprogs. Code taken from a permissive project names it in the file's header (`agents.md`).
