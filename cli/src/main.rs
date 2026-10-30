// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The shipo packer: an app's binaries into signed installers and a signed update feed. This file
//! is its command line; [USAGE] is its documentation.

mod btree;
mod config;
mod deb;
mod dmg;
mod ds_store;
mod feed;
mod hfs;
mod image;
mod linux;
mod macos;
mod pe;
mod plist;
mod squashfs;
mod tool;
mod tree;
mod udif;
mod unicode;
mod windows;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

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
  --key <file>        release, feed: the private key's file, else $SHIPO_UPDATE_KEY
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

/// Makes and signs the macOS app of the binary `bin`, and prints its path.
fn app(cfg: &Config, bin: &Path) -> io::Result<()> {
  let (bundle, signer) = macos::app(cfg, bin, None)?;
  eprintln!("shipo: {} {}, signed by {signer}", cfg.name, cfg.version);
  println!("{}", bundle.display());
  Ok(())
}

/// The OSes `release` takes binaries for, as their target triples end, in the feed's order.
const OSES: [&str; 3] = ["apple-darwin", "pc-windows-msvc", "unknown-linux-gnu"];
/// The archs `release` takes binaries for, as their target triples start.
const ARCHS: [&str; 2] = ["x86_64", "aarch64"];

/// The installers of the binaries in `dir`, `<Name>-dev-…` at its top level, each path printed;
/// with `tag` also update.txt there, signed by `key`, of the files as the GitHub release serves
/// them. The tag and the key are checked before anything is built.
fn release(cfg: &Config, dir: &Path, tag: Option<&str>, key: Option<&Path>) -> io::Result<()> {
  if tag.is_none() && key.is_some() {
    return Err(bad("--key signs the feed, which only --tag writes"));
  }

  let key = match tag {
    Some(tag) => {
      tagged(tag, &cfg.version, &cfg.repository, &cfg.name)?;
      Some(feed::Key::load(key, std::env::var_os("SHIPO_UPDATE_KEY"))?)
    }
    None => None,
  };

  let binaries = binaries(dir, &cfg.bin)?;
  clean(dir, &cfg.name)?;
  let mut files = vec![];
  for (os, arch, bin) in binaries {
    let made = match os {
      "apple-darwin" => {
        let notarize = macos::secrets(&dmg::NOTARY, tag)?;
        let (app, _) = macos::app(cfg, &bin, tag)?;
        vec![dmg::dmg(cfg, &app, arch, tag, notarize, dir)?]
      }
      "pc-windows-msvc" => vec![windows::setup(cfg, &bin, arch, dir)?],
      _ => vec![
        linux::appimage(cfg, &bin, arch, dir)?,
        linux::deb(cfg, &bin, arch, tag.is_none(), dir)?,
      ],
    };

    for file in made {
      println!("{}", file.display());
      files.push(file);
    }
  }

  let (Some(tag), Some(key)) = (tag, key) else { return Ok(()) };

  // The feed lists what the library installs: a .deb is apt's.
  files.retain(|f| f.extension().is_none_or(|e| e != "deb"));
  let url = format!("{}/releases/download/{tag}", cfg.repository);
  let notes = format!("{}/releases/tag/{tag}", cfg.repository);
  let name = |file: &str| renamed(&cfg.name, tag, file);
  let body = feed::body(&cfg.version, Some(&notes), &url, &files, name)?;
  println!("{}", feed::write(&key, &body, dir)?.display());
  Ok(())
}

/// Checks a release's `tag` against the app's `version`, `repository` and `name`: `v` and the
/// version, a GitHub repository, whose release serves the files, and a name GitHub keeps in the
/// files' names: it renames an upload of other characters (a space to `.`), so the feed would miss.
fn tagged(tag: &str, version: &str, repository: &str, name: &str) -> io::Result<()> {
  if tag != format!("v{version}") {
    return Err(bad(format!("the tag {tag} is not the app's version, v{version}")));
  }

  let repo = repository.strip_prefix("https://github.com/").unwrap_or_default();
  let parts: Vec<_> = repo.split('/').collect();
  if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
    return Err(bad(format!(
      "--tag serves the files from the GitHub release: package.repository is {repository:?}, not \
       https://github.com/<owner>/<repo>"
    )));
  }

  let kept = |c: char| c.is_ascii_alphanumeric() || "._-".contains(c);
  if name.is_empty() || !name.chars().all(kept) {
    return Err(bad(format!(
      "--tag: the name {name:?} is not of [A-Za-z0-9._-] alone, which GitHub keeps in a release's \
       file names"
    )));
  }

  Ok(())
}

/// Removes `dir`'s update.txt and `<name>-dev-*` files, a previous run's, so no old feed or
/// package of a triple gone from `dir` is left beside the new ones.
fn clean(dir: &Path, name: &str) -> io::Result<()> {
  let prefix = format!("{name}-dev-");
  for entry in fs::read_dir(dir)? {
    let entry = entry?;
    let file = entry.file_name();
    let stale = file == "update.txt" || file.to_str().is_some_and(|f| f.starts_with(&prefix));
    if stale && !entry.file_type()?.is_dir() {
      fs::remove_file(entry.path())?;
    }
  }

  Ok(())
}

/// The binaries in `dir`, `<triple>/<bin>[.exe]` of [OSES] and [ARCHS], each with its OS and
/// arch, in the feed's order; an error when there is none.
fn binaries(dir: &Path, bin: &str) -> io::Result<Vec<(&'static str, &'static str, PathBuf)>> {
  let all = OSES.iter().flat_map(|os| ARCHS.iter().map(move |arch| (*os, *arch)));
  let found: Vec<_> = all
    .map(|(os, arch)| {
      let exe = if os == "pc-windows-msvc" { ".exe" } else { "" };
      (os, arch, dir.join(format!("{arch}-{os}")).join(format!("{bin}{exe}")))
    })
    .filter(|(_, _, path)| path.is_file())
    .collect();

  if found.is_empty() {
    let triples: Vec<_> =
      OSES.iter().flat_map(|os| ARCHS.map(|arch| format!("{arch}-{os}"))).collect();
    return Err(bad(format!(
      "{} holds no <triple>/{bin}[.exe], of {}",
      dir.display(),
      triples.join(", ")
    )));
  }

  Ok(found)
}

/// The name rust-publish gives the release's `file`: `<name>-dev-…` as `<name>-<tag>-…`, the same
/// bytes; another name as it is.
fn renamed(name: &str, tag: &str, file: &str) -> String {
  match file.strip_prefix(&format!("{name}-dev-")) {
    Some(rest) => format!("{name}-{tag}-{rest}"),
    None => file.to_string(),
  }
}

/// The feed of `files` served at `url`, signed by `key`, in `<target>/distrib`; prints its path.
fn feed(
  cfg: &Config,
  url: &str,
  notes: Option<&str>,
  files: &[PathBuf],
  key: Option<&Path>,
) -> io::Result<()> {
  // The key first: a missing one fails before the files are hashed.
  let key = feed::Key::load(key, std::env::var_os("SHIPO_UPDATE_KEY"))?;
  let body = feed::body(&cfg.version, notes, url, files, str::to_owned)?;
  let path = feed::write(&key, &body, &cfg.target.join("distrib"))?;
  println!("{}", path.display());
  Ok(())
}

/// A new key in each of `files`, its public half printed.
fn keygen(files: &[PathBuf]) -> io::Result<()> {
  for (file, public) in files.iter().zip(feed::keygen(files)?) {
    println!("{}: {public}", file.display());
  }

  Ok(())
}

/// Prints the public half of the key in `file`.
fn public(file: &Path) -> io::Result<()> {
  println!("{}", feed::Key::read(file)?.public());
  Ok(())
}

/// An error in what the user gave, `what` its message.
fn bad(what: impl fmt::Display) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

#[cfg(test)]
mod tests {
  use std::fs;

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
  fn binaries_found() {
    let tmp = tool::Temp::new().unwrap();
    assert_eq!(
      binaries(&tmp, "hello").unwrap_err().to_string(),
      format!(
        "{} holds no <triple>/hello[.exe], of x86_64-apple-darwin, aarch64-apple-darwin, \
         x86_64-pc-windows-msvc, aarch64-pc-windows-msvc, x86_64-unknown-linux-gnu, \
         aarch64-unknown-linux-gnu",
        tmp.display()
      )
    );

    // A Windows binary without `.exe`, another binary, a folder and a stray triple are not taken.
    let files = [
      "aarch64-unknown-linux-gnu/hello",
      "x86_64-pc-windows-msvc/hello.exe",
      "aarch64-pc-windows-msvc/hello",
      "x86_64-unknown-linux-gnu/other",
      "x86_64-unknown-linux-musl/hello",
      "x86_64-apple-darwin/hello",
    ];

    for file in files {
      fs::create_dir_all(tmp.join(file).parent().unwrap()).unwrap();
      fs::write(tmp.join(file), b"").unwrap();
    }

    fs::create_dir_all(tmp.join("aarch64-apple-darwin/hello")).unwrap();
    let found = binaries(&tmp, "hello").unwrap();
    let want = [
      ("apple-darwin", "x86_64", "x86_64-apple-darwin/hello"),
      ("pc-windows-msvc", "x86_64", "x86_64-pc-windows-msvc/hello.exe"),
      ("unknown-linux-gnu", "aarch64", "aarch64-unknown-linux-gnu/hello"),
    ];

    let want: Vec<_> = want.map(|(os, arch, file)| (os, arch, tmp.join(file))).into();
    assert_eq!(found, want);
  }

  #[test]
  fn tags() {
    let gh = "https://github.com/vladkens/wavo-app";
    assert!(tagged("v1.2.0", "1.2.0", gh, "Wavo").is_ok());
    assert!(tagged("v1.2.0-rc.1", "1.2.0-rc.1", gh, "Wavo_App-2.x").is_ok());

    let not_version = "the tag 1.2.0 is not the app's version, v1.2.0";
    let not_github = |repo: &str| {
      format!(
        "--tag serves the files from the GitHub release: package.repository is {repo:?}, not \
         https://github.com/<owner>/<repo>"
      )
    };

    let cases = [
      ("1.2.0", "1.2.0", gh, not_version.to_string()),
      ("v1.2.1", "1.2.0", gh, "the tag v1.2.1 is not the app's version, v1.2.0".into()),
      ("v1.2", "1.2.0", gh, "the tag v1.2 is not the app's version, v1.2.0".into()),
      ("v1.2.0", "1.2.0", "", not_github("")),
      ("v1.2.0", "1.2.0", "https://gitlab.com/a/b", not_github("https://gitlab.com/a/b")),
      ("v1.2.0", "1.2.0", "https://github.com/a", not_github("https://github.com/a")),
      ("v1.2.0", "1.2.0", "https://github.com/a/b/c", not_github("https://github.com/a/b/c")),
      ("v1.2.0", "1.2.0", "http://github.com/a/b", not_github("http://github.com/a/b")),
    ];

    for (tag, version, repo, want) in cases {
      let e = tagged(tag, version, repo, "Wavo").unwrap_err().to_string();
      assert_eq!(e, want, "{tag} {repo}");
    }

    for name in ["Wavo Pro", "Wavo/Pro", "Wavö", "Wavo+", ""] {
      assert_eq!(
        tagged("v1.2.0", "1.2.0", gh, name).unwrap_err().to_string(),
        format!(
          "--tag: the name {name:?} is not of [A-Za-z0-9._-] alone, which GitHub keeps in a \
           release's file names"
        )
      );
    }
  }

  #[test]
  fn renames() {
    let cases = [
      ("Wavo-dev-linux-x86_64.AppImage", "Wavo-v1.2.0-linux-x86_64.AppImage"),
      ("Wavo-dev-windows-arm64-setup.exe", "Wavo-v1.2.0-windows-arm64-setup.exe"),
      ("Wavo Pro-dev-macos-aarch64.dmg", "Wavo Pro-dev-macos-aarch64.dmg"),
      ("Wavo-devx-linux-x86_64.AppImage", "Wavo-devx-linux-x86_64.AppImage"),
      ("update.txt", "update.txt"),
    ];

    for (file, want) in cases {
      assert_eq!(renamed("Wavo", "v1.2.0", file), want, "{file}");
    }

    assert_eq!(
      renamed("My-dev-App", "v1.2.0", "My-dev-App-dev-linux-x86_64.AppImage"),
      "My-dev-App-v1.2.0-linux-x86_64.AppImage"
    );

    assert_eq!(
      renamed("Wavo Pro", "v1.2.0", "Wavo Pro-dev-macos-aarch64.dmg"),
      "Wavo Pro-v1.2.0-macos-aarch64.dmg"
    );
  }

  #[test]
  fn cleans() {
    let tmp = tool::Temp::new().unwrap();
    let names = [
      "Wavo-dev-linux-x86_64.deb",
      "Wavo-dev-windows-x64-setup.exe",
      "update.txt",
      "Wavo-v1.2.0-linux-x86_64.deb",
      "Wavo-devx.txt",
      "Other-dev-linux-x86_64.deb",
      "notes.md",
    ];

    for name in names {
      fs::write(tmp.join(name), b"").unwrap();
    }

    fs::create_dir(tmp.join("Wavo-dev-folder")).unwrap();
    clean(&tmp, "Wavo").unwrap();
    let want = [
      "Other-dev-linux-x86_64.deb",
      "Wavo-dev-folder",
      "Wavo-devx.txt",
      "Wavo-v1.2.0-linux-x86_64.deb",
      "notes.md",
    ];
    assert_eq!(listed(&tmp), want);
  }

  /// The names in `dir`, sorted, its folders' contents not among them.
  fn listed(dir: &Path) -> Vec<String> {
    let names = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap());
    let mut names: Vec<_> = names.collect();
    names.sort();
    names
  }

  /// An app `Wavo` of version 1.2.0 on GitHub in `dir`, its binary `wavo`.
  fn app(dir: &Path) -> Config {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    let manifest = dir.join("Cargo.toml");
    let toml = "[package]\nname = \"wavo\"\nversion = \"1.2.0\"\nedition = \"2024\"\n\
      repository = \"https://github.com/vladkens/wavo-app\"\n\n[package.metadata.shipo]\n\
      id = \"com.example.wavo\"\nname = \"Wavo\"\nicon = \"icon.png\"\n";
    fs::write(&manifest, toml).unwrap();
    Config::load(&manifest).unwrap()
  }

  #[test]
  fn release_checks_first() {
    let tmp = tool::Temp::new().unwrap();
    let cfg = app(&tmp.join("app"));
    let dist = tmp.join("dist");
    fs::create_dir_all(dist.join("x86_64-unknown-linux-gnu")).unwrap();
    fs::write(dist.join("x86_64-unknown-linux-gnu/wavo"), b"").unwrap();
    // A previous run's files stay when the checks fail.
    fs::write(dist.join("update.txt"), b"old").unwrap();
    let before = listed(&dist);

    let e = release(&cfg, &dist, Some("v9.9.9"), None).unwrap_err();
    assert_eq!(e.to_string(), "the tag v9.9.9 is not the app's version, v1.2.0");
    assert_eq!(listed(&dist), before);

    let key = tmp.join("missing.key");
    let e = release(&cfg, &dist, Some("v1.2.0"), Some(&key)).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}");
    assert_eq!(listed(&dist), before);

    let e = release(&cfg, &dist, None, Some(&key)).unwrap_err();
    assert_eq!(e.to_string(), "--key signs the feed, which only --tag writes");
    assert_eq!(listed(&dist), before);
  }
}
