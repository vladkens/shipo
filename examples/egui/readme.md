# ShipoEgui

An [egui](https://github.com/emilk/egui) app that updates itself with shipo: its version, a button that checks the feed, the release found with its notes, the download's progress bar, then install and restart. The update's calls run on a thread and post back to the window; `src/update.rs` holds the steps, tested with no window.

```sh
cargo run    # the window; a build run by cargo can't update in place, so it says why
cargo test   # the update's steps
```

To see an update, pack two versions and serve their feed locally (here on an Apple silicon Mac, where shipo signs both with one local identity, as macOS requires):

1. `shipo keygen update.key update-spare.key`, then put both public keys and `http://127.0.0.1:8000/update.txt` in `FEED` in `src/update.rs`.
2. `cargo build --release`, copy `target/release/shipo-egui` to `dist/aarch64-apple-darwin/`, run `shipo release dist` and install `dist/ShipoEgui-dev-macos-aarch64.dmg`.
3. Set `version = "0.2.0"` in `Cargo.toml`, build and pack again, then `shipo feed --key update.key --url http://127.0.0.1:8000 dist/*.dmg`, `cp target/distrib/update.txt dist/` and serve it: `python3 -m http.server 8000 -d dist`.
4. Open the installed ShipoEgui 0.1.0 and check for updates: it downloads 0.2.0, installs it and restarts.
