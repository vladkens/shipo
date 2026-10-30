# shipo

Releases and updates for desktop apps.

- `shipo` (the packer, `cli/`): an app's bare binaries for macOS, Windows and Linux into a signed and notarized DMG, a per-user setup, an AppImage and a .deb, and a signed `update.txt`, all on one Ubuntu host.
- `shipo` (the crate, `src/`): inside the app, checks the feed, downloads the update and installs it in place after the app quits, with no window.

`spec.md` describes the feed and the install contract between them.

Work in progress.

## Keys

The feed is signed with Ed25519. The app holds the public keys, built in, and takes a feed signed by any of them; the private keys live only in the release job's secret and in your password manager. Make two, the working key and a spare:

```sh
shipo keygen update.key update-spare.key
```

It writes each private key, its 32-byte seed in hex, to its file (readable by you alone, never over a file that is there) and prints each public key. Then:

1. Both public keys go into the app: `shipo::Feed::new("<feed url>", &["<working>", "<spare>"])`.
2. `update.key` becomes the release job's secret `UPDATE_KEY`; on GitHub, `gh secret set UPDATE_KEY < update.key`.
3. `update-spare.key` goes to your password manager, never to CI.
4. Delete both files.

`shipo keygen --public <file>` prints a private key's public half, to tell which key is which. The app may hold any number of keys, but each is one more secret that can sign a feed: two is the rule, one that signs and one in reserve.

### Replacing a key

When the working key leaks (the secret was read, a log printed it), the spare signs the release that drops it:

1. Make a new spare, `shipo keygen update-spare-2.key`, and keep it in your password manager.
2. In the app, replace the leaked public key with the new spare's: the app now holds the old spare and the new one.
3. Set `UPDATE_KEY` to the old spare and release at once. The installed apps trust the old spare, so they take this release, which trusts the leaked key no more; the old spare is the working key from now on.

Until an install takes that release, a feed the leaked key signs still reaches it: release at once. On macOS the new bundle must also be signed by the app's Developer ID, which the leaked key alone doesn't pass. A leaked or lost spare is replaced the same way, the release signed by the working key; a key is retired with nothing leaked the same way too. With both keys lost, no feed reaches the installed apps: people download the next release by hand.

## Contributing

A change to `spec.md` lands with both sides' code. A change is ready when `make check` and `make test` pass; `make prepare` formats the code. Commits are one short lowercase line. `agents.md` holds the project's conventions, for people and agents alike.
