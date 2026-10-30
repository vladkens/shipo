// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! A console app that updates itself in place, with no window: it prints its version, the release
//! a feed offers, or downloads and installs that release and ends, as the new version starts in
//! its place. Its first call is [shipo::finish], which is the installer when the update started it
//! and else prints the error of the installer that started this run.
//!
//! `UPDATE_VERSION=1.0.0 cargo build --release --example update` sets the version (the crate's
//! without it). The release's file is what people download, of `shipo release`: on macOS the
//! example bundled as `Update.app`, signed with the identity v1 has, in its DMG; on Windows the
//! per-user setup, with the config's `name` "app" so it installs
//! `%LOCALAPPDATA%\Programs\app\app.exe` (Windows asks to elevate an `update.exe`); on Linux the
//! AppImage. `shipo feed` makes a feed of them, `update.txt`, signed by a key of `shipo keygen`.
//! Then `update` prints the version, `update --check <feed url> <key>...` what the feed offers,
//! `update --update <feed url> <key>...` updates, printing the download's percentage.
//! `make e2e` does all of it on this host (scripts/e2e-update.py), as CI does on each OS.
//! With `SHIPO_FAKE_UPDATE=2.0.0` (the readme's Fake update) its feed's update is faked: any URL
//! and key do.

use std::env;
use std::error::Error;
use std::process::ExitCode;

use shipo::Feed;

pub const VERSION: &str = match option_env!("UPDATE_VERSION") {
  Some(v) => v,
  None => env!("CARGO_PKG_VERSION"),
};
const USAGE: &str = "usage: update [--check | --update <feed url> <key>...]";

/// What the arguments ask: the version; the release the feed offers; that release installed.
fn run(args: &[String]) -> Result<(), Box<dyn Error>> {
  let (update, url, keys) = match args {
    [] => {
      println!("{VERSION}");
      return Ok(());
    }
    [flag, url, keys @ ..] if !keys.is_empty() && flag == "--check" => (false, url, keys),
    [flag, url, keys @ ..] if !keys.is_empty() && flag == "--update" => (true, url, keys),
    _ => return Err(USAGE.into()),
  };

  let keys = keys.iter().map(String::as_str).collect::<Vec<_>>();
  let Some(release) = shipo::check(&Feed::new(url, &keys).fakeable(), VERSION)? else {
    println!("{VERSION} is the latest");
    return Ok(());
  };

  if !update {
    let notes = release.notes.as_deref().unwrap_or("no notes");
    println!("{} at {} ({notes})", release.version, release.url);
    return Ok(());
  }

  println!("{VERSION} -> {}: downloading {}", release.version, release.url);
  let staged = shipo::download(&release, |done, total| {
    println!("{}%", done * 100 / total.max(1));
  })?;
  shipo::install(staged)?;
  println!("installed {}, it starts as this one ends", release.version);
  Ok(())
}

fn main() -> ExitCode {
  // First, before the arguments: the installer runs here and exits.
  if let Some(e) = shipo::finish() {
    eprintln!("the update failed: {e}");
  }

  match run(&env::args().skip(1).collect::<Vec<_>>()) {
    Ok(()) => ExitCode::SUCCESS,
    Err(e) => {
      eprintln!("{e}");
      ExitCode::FAILURE
    }
  }
}
