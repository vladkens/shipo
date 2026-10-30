// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The shipo packer: an app's binaries into signed installers and a signed update feed. This file
//! is its command line; [USAGE] is its documentation.

mod config;
mod image;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{fmt, io};

use config::Config;

/// `shipo --help`.
const USAGE: &str = "\
shipo: an app's binaries into signed installers and a signed update feed.

Usage:
  shipo app --bin <file>                              the macOS .app of the binary, signed
  shipo release <dir> [--tag v<version>]              the installers of <dir>/<triple>/<bin>[.exe]
  shipo feed --url <base> [--notes <url>] <files>...  update.txt of the files, signed
  shipo keygen [<file>...]                            a key per file, its public half printed
  shipo keygen --public <file>                        print a key's public half

Options:
  --manifest <file>   app, release, feed: the app's Cargo.toml, by default ./Cargo.toml
  --bin <file>        app: the binary
  --tag v<version>    release: the GitHub release's tag, to write update.txt too
  --key <file>        release, feed: the private key's file, else $UPDATE_KEY
  --url <base>        feed: where the files will be served, {version} the version
  --notes <url>       feed: the release notes' page
  --public            keygen: print the public half of a key, made before
  <file>...           keygen: the keys' files, by default update.key update-spare.key
  -h, --help          print this help
";

/// A command as the arguments give it.
#[derive(Debug, PartialEq)]
enum Command {
  Help,
  App {
    manifest: PathBuf,
    bin: PathBuf,
  },
  Release {
    manifest: PathBuf,
    dir: PathBuf,
    tag: Option<String>,
    key: Option<PathBuf>,
  },
  Feed {
    manifest: PathBuf,
    url: String,
    notes: Option<String>,
    files: Vec<PathBuf>,
    key: Option<PathBuf>,
  },
  Keygen {
    files: Vec<PathBuf>,
  },
  Public {
    file: PathBuf,
  },
}

fn main() {
  if let Err(e) = run(lexopt::Parser::from_env()) {
    eprintln!("shipo: {e}");
    std::process::exit(1);
  }
}

/// Runs the command `args` give.
fn run(args: lexopt::Parser) -> io::Result<()> {
  let cmd = parse(args).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
  match cmd {
    Command::Help => {
      print!("{USAGE}");
      Ok(())
    }
    Command::App { manifest, bin } => app(&Config::load(&manifest)?, &bin),
    Command::Release { manifest, dir, tag, key } => {
      release(&Config::load(&manifest)?, &dir, tag.as_deref(), key.as_deref())
    }
    Command::Feed { manifest, url, notes, files, key } => {
      feed(&Config::load(&manifest)?, &url, notes.as_deref(), &files, key.as_deref())
    }
    Command::Keygen { files } => keygen(&files),
    Command::Public { file } => public(&file),
  }
}

/// The command in `args`, the program's name not among them, checked as [USAGE] gives them.
fn parse(mut args: lexopt::Parser) -> Result<Command, lexopt::Error> {
  use lexopt::prelude::*;

  let (mut cmd, mut free) = (None::<String>, vec![]);
  let (mut manifest, mut bin, mut tag, mut key) = (None, None, None, None);
  let (mut url, mut notes, mut public) = (None, None, false);
  while let Some(arg) = args.next()? {
    let is = |names: &[&str]| cmd.as_deref().is_some_and(|c| names.contains(&c));
    match arg {
      Short('h') | Long("help") => return Ok(Command::Help),
      Value(v) if cmd.is_none() => cmd = Some(v.string()?),
      Value(v) => free.push(PathBuf::from(v)),
      Long("manifest") if is(&["app", "release", "feed"]) => {
        manifest = Some(value(&mut args, "manifest", manifest.is_some())?.into())
      }
      Long("bin") if is(&["app"]) => bin = Some(value(&mut args, "bin", bin.is_some())?.into()),
      Long("tag") if is(&["release"]) => {
        tag = Some(value(&mut args, "tag", tag.is_some())?.string()?)
      }
      Long("key") if is(&["release", "feed"]) => {
        key = Some(value(&mut args, "key", key.is_some())?.into())
      }
      Long("url") if is(&["feed"]) => url = Some(value(&mut args, "url", url.is_some())?.string()?),
      Long("notes") if is(&["feed"]) => {
        notes = Some(value(&mut args, "notes", notes.is_some())?.string()?)
      }
      Long("public") if is(&["keygen"]) && public => return Err(twice("public")),
      Long("public") if is(&["keygen"]) => public = true,
      _ => return Err(arg.unexpected()),
    }
  }

  let manifest = manifest.unwrap_or_else(|| PathBuf::from("Cargo.toml"));
  let one = |free: Vec<PathBuf>| <[PathBuf; 1]>::try_from(free).ok().map(|[x]| x);
  match cmd.as_deref() {
    None => Err("a command: app, release, feed or keygen; see --help".into()),
    Some("app") if !free.is_empty() => Err("app takes no files, the binary is --bin".into()),
    Some("app") => match bin {
      Some(bin) => Ok(Command::App { manifest, bin }),
      None => Err("app takes --bin, the binary".into()),
    },
    Some("release") => match one(free) {
      Some(dir) => Ok(Command::Release { manifest, dir, tag, key }),
      None => Err("release takes the binaries' folder".into()),
    },
    Some("feed") => match url {
      Some(url) if !free.is_empty() => Ok(Command::Feed { manifest, url, notes, files: free, key }),
      _ => Err("feed needs --url and the files".into()),
    },
    Some("keygen") if public => match one(free) {
      Some(file) => Ok(Command::Public { file }),
      None => Err("keygen --public takes one key file".into()),
    },
    Some("keygen") if free.is_empty() => {
      Ok(Command::Keygen { files: vec!["update.key".into(), "update-spare.key".into()] })
    }
    Some("keygen") => Ok(Command::Keygen { files: free }),
    Some(cmd) => Err(format!("no command {cmd}: app, release, feed or keygen").into()),
  }
}

/// The value of the option `--<opt>` just read, given before if `given`: an error then, or when
/// the value is a word of its own starting with `-`, as in `--url --notes x`.
fn value(args: &mut lexopt::Parser, opt: &str, given: bool) -> Result<OsString, lexopt::Error> {
  if given {
    return Err(twice(opt));
  }

  if let Some(value) = args.optional_value() {
    return Ok(value);
  }

  let value = args.value()?;
  match value.to_string_lossy().starts_with('-') {
    true => Err(format!("--{opt} needs a value, not {}", value.to_string_lossy()).into()),
    false => Ok(value),
  }
}

fn twice(opt: &str) -> lexopt::Error {
  format!("--{opt} is given twice").into()
}

// MARK: Commands
// Each comes in a step of its own (docs/plans).

/// Signs the macOS app of the binary `bin`.
fn app(_cfg: &Config, _bin: &Path) -> io::Result<()> {
  Err(not_yet("app"))
}

/// The installers of the binaries in `dir`, and with `tag` the feed signed by `key`.
fn release(_cfg: &Config, _dir: &Path, _tag: Option<&str>, _key: Option<&Path>) -> io::Result<()> {
  Err(not_yet("release"))
}

/// The feed of `files` served at `url`, signed by `key`.
fn feed(
  _cfg: &Config,
  _url: &str,
  _notes: Option<&str>,
  _files: &[PathBuf],
  _key: Option<&Path>,
) -> io::Result<()> {
  Err(not_yet("feed"))
}

/// A new key in each of `files`, its public half printed.
fn keygen(_files: &[PathBuf]) -> io::Result<()> {
  Err(not_yet("keygen"))
}

/// Prints the public half of the key in `file`.
fn public(_file: &Path) -> io::Result<()> {
  Err(not_yet("keygen --public"))
}

/// The error of a command still to come.
fn not_yet(cmd: &str) -> io::Error {
  io::Error::new(io::ErrorKind::Unsupported, format!("{cmd}: not yet"))
}

/// An error in what the user gave, `what` its message.
fn bad(what: impl fmt::Display) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(line: &str) -> Result<Command, String> {
    let line = line.split_whitespace().map(OsString::from);
    parse(lexopt::Parser::from_args(line)).map_err(|e| e.to_string())
  }

  #[test]
  fn commands() {
    let (manifest, cargo) = (PathBuf::from("app/Cargo.toml"), PathBuf::from("Cargo.toml"));
    let cases = [
      ("--help", Command::Help),
      ("feed -h --bogus", Command::Help),
      ("app --bin x", Command::App { manifest: cargo.clone(), bin: "x".into() }),
      (
        "app --manifest app/Cargo.toml --bin=x",
        Command::App { manifest: manifest.clone(), bin: "x".into() },
      ),
      (
        "release out --manifest app/Cargo.toml",
        Command::Release { manifest: manifest.clone(), dir: "out".into(), tag: None, key: None },
      ),
      (
        "release --tag v1.2.0 out --key k",
        Command::Release {
          manifest: cargo.clone(),
          dir: "out".into(),
          tag: Some("v1.2.0".into()),
          key: Some("k".into()),
        },
      ),
      (
        "feed a.zip --url https://x/{version} b.zip --notes https://x/notes",
        Command::Feed {
          manifest: cargo.clone(),
          url: "https://x/{version}".into(),
          notes: Some("https://x/notes".into()),
          files: vec!["a.zip".into(), "b.zip".into()],
          key: None,
        },
      ),
      ("keygen", Command::Keygen { files: vec!["update.key".into(), "update-spare.key".into()] }),
      ("keygen a.key", Command::Keygen { files: vec!["a.key".into()] }),
      ("keygen --public a.key", Command::Public { file: "a.key".into() }),
      (
        "release out --tag=-v1",
        Command::Release {
          manifest: cargo.clone(),
          dir: "out".into(),
          tag: Some("-v1".into()),
          key: None,
        },
      ),
    ];

    for (line, want) in cases {
      assert_eq!(args(line), Ok(want), "{line}");
    }
  }

  #[test]
  fn errors() {
    let cases = [
      ("", "a command: app, release, feed or keygen; see --help"),
      ("pack", "no command pack: app, release, feed or keygen"),
      ("--manifest Cargo.toml app", "invalid option '--manifest'"),
      ("app", "app takes --bin, the binary"),
      ("app x --bin y", "app takes no files, the binary is --bin"),
      ("app --tag v1", "invalid option '--tag'"),
      ("release", "release takes the binaries' folder"),
      ("release a b", "release takes the binaries' folder"),
      ("feed a.zip", "feed needs --url and the files"),
      ("feed --url https://x", "feed needs --url and the files"),
      ("feed --url", "missing argument for option '--url'"),
      ("feed --url --notes x a.zip", "--url needs a value, not --notes"),
      ("feed --url https://x a.zip --url https://y", "--url is given twice"),
      ("release out --tag v1 --tag=v2", "--tag is given twice"),
      ("release out --key -", "--key needs a value, not -"),
      ("app --bin x --manifest a --manifest b", "--manifest is given twice"),
      ("keygen --public --public a.key", "--public is given twice"),
      ("keygen --manifest Cargo.toml", "invalid option '--manifest'"),
      ("keygen --key k", "invalid option '--key'"),
      ("keygen --public", "keygen --public takes one key file"),
      ("keygen --public a b", "keygen --public takes one key file"),
    ];

    for (line, want) in cases {
      assert_eq!(args(line), Err(want.into()), "{line}");
    }
  }

  #[test]
  fn stubs() {
    let e = run(lexopt::Parser::from_args(["keygen"])).unwrap_err();
    assert_eq!(e.to_string(), "keygen: not yet");
  }
}
