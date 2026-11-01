// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The shipo packer: an app's binaries into signed installers and a signed update feed. This file
//! is its command line, which clap parses from [Command].

mod config;
mod dmg;
mod feed;
mod hfs;
mod image;
mod linux;
mod macos;
mod payload;
mod sign;
mod tool;
mod unicode;
mod windows;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};
use config::{Config, Overrides};

#[derive(Debug, Parser, PartialEq)]
#[command(
  name = "shipo",
  about = "shipo: an app's binaries into signed installers and a signed update feed.",
  arg_required_else_help = true,
  disable_help_subcommand = true,
  subcommand_value_name = "command"
)]
struct Cli {
  #[command(subcommand)]
  cmd: Command,
}

/// A command as the arguments give it; its docs are its `--help`.
#[derive(Debug, PartialEq, Subcommand)]
enum Command {
  /// the macOS .app of the binary, signed
  App {
    /// the app's Cargo.toml
    #[arg(long, value_name = "file", default_value = "Cargo.toml")]
    manifest: PathBuf,
    /// the binary
    #[arg(long, value_name = "file")]
    bin: PathBuf,
    /// the bundle id, in place of the config's
    #[arg(long, value_name = "id")]
    id: Option<String>,
    /// the name shown and the bundle's, in place of the config's
    #[arg(long, value_name = "name")]
    name: Option<String>,
    /// the icon, in place of the config's
    #[arg(long, value_name = "png")]
    icon: Option<PathBuf>,
  },
  /// the installers of <dir>/<triple>/<bin>[.exe]
  Release {
    /// the app's Cargo.toml
    #[arg(long, value_name = "file", default_value = "Cargo.toml")]
    manifest: PathBuf,
    /// the binaries' folder
    #[arg(value_name = "dir")]
    dir: PathBuf,
    /// the GitHub release's tag, v<version>, to write update.txt too
    #[arg(long, value_name = "tag")]
    tag: Option<String>,
    /// the private key's file, else $SHIPO_UPDATE_KEY
    #[arg(long, value_name = "file")]
    key: Option<PathBuf>,
  },
  /// update.txt of the files, signed
  Feed {
    /// the app's Cargo.toml
    #[arg(long, value_name = "file", default_value = "Cargo.toml")]
    manifest: PathBuf,
    /// where the files will be served, {version} the version
    #[arg(long, value_name = "base")]
    url: String,
    /// the release notes' page
    #[arg(long, value_name = "url")]
    notes: Option<String>,
    /// the files the feed lists
    #[arg(value_name = "files", required = true)]
    files: Vec<PathBuf>,
    /// the private key's file, else $SHIPO_UPDATE_KEY
    #[arg(long, value_name = "file")]
    key: Option<PathBuf>,
  },
  /// a key per file, its public half printed
  Keygen {
    /// the keys' files
    #[arg(value_name = "file", default_values = ["update.key", "update-spare.key"])]
    files: Vec<PathBuf>,
    /// print the public half of one key, made before
    #[arg(long)]
    public: bool,
  },
}

fn main() {
  let cmd = parse(std::env::args_os()).unwrap_or_else(|e| e.exit());
  if let Err(e) = run(cmd) {
    eprintln!("shipo: {e}");
    std::process::exit(1);
  }
}

/// The command in `args`, the program's name first; clap's error, which prints itself, else.
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, clap::Error> {
  let cmd = Cli::try_parse_from(args)?.cmd;
  if let Command::Keygen { files, public: true } = &cmd
    && files.len() != 1
  {
    let mut cli = Cli::command();
    cli.build();
    let keygen = cli.find_subcommand_mut("keygen").expect("keygen is a command");
    return Err(keygen.error(ErrorKind::WrongNumberOfValues, "--public takes one key file"));
  }

  Ok(cmd)
}

/// Runs `cmd`.
fn run(cmd: Command) -> io::Result<()> {
  match cmd {
    Command::App { manifest, bin, id, name, icon } => {
      app(&Config::load(&manifest)?.with(Overrides { id, name, icon })?, &bin)
    }
    Command::Release { manifest, dir, tag, key } => {
      release(&Config::load(&manifest)?, &dir, tag.as_deref(), key.as_deref())
    }
    Command::Feed { manifest, url, notes, files, key } => {
      feed(&Config::load(&manifest)?, &url, notes.as_deref(), &files, key.as_deref())
    }
    Command::Keygen { files, public: true } => public(&files[0]),
    Command::Keygen { files, public: false } => keygen(&files),
  }
}

// MARK: Commands

/// Makes and signs the macOS app of the binary `bin`, and prints its path.
fn app(cfg: &Config, bin: &Path) -> io::Result<()> {
  let (bundle, signer) = macos::app(cfg, bin, None)?;
  eprintln!("shipo: {} {}, signed by {signer}", cfg.name, cfg.version);
  println!("{}", bundle.display());
  Ok(())
}

/// An OS `release` takes binaries for, in the feed's order.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Os {
  Macos,
  Windows,
  Linux,
}

impl Os {
  const ALL: [Self; 3] = [Self::Macos, Self::Windows, Self::Linux];

  /// The end of its target triples.
  fn triple(self) -> &'static str {
    match self {
      Self::Macos => "apple-darwin",
      Self::Windows => "pc-windows-msvc",
      Self::Linux => "unknown-linux-gnu",
    }
  }
}

/// An arch `release` takes binaries for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Arch {
  X86_64,
  Aarch64,
}

impl Arch {
  const ALL: [Self; 2] = [Self::X86_64, Self::Aarch64];

  /// Rust's name, which starts its target triples and ends the macOS and Linux files' names.
  pub fn name(self) -> &'static str {
    match self {
      Self::X86_64 => "x86_64",
      Self::Aarch64 => "aarch64",
    }
  }

  /// Windows' name, in the setup's file name.
  pub fn windows(self) -> &'static str {
    match self {
      Self::X86_64 => "x64",
      Self::Aarch64 => "arm64",
    }
  }

  /// Debian's name.
  pub fn deb(self) -> &'static str {
    match self {
      Self::X86_64 => "amd64",
      Self::Aarch64 => "arm64",
    }
  }

  /// The machine of an exe's COFF header.
  pub fn machine(self) -> u16 {
    match self {
      Self::X86_64 => windows::AMD64,
      Self::Aarch64 => windows::ARM64,
    }
  }
}

impl fmt::Display for Arch {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    f.write_str(self.name())
  }
}

/// The installers of the binaries in `dir`, [named](out) by `tag` at its top level, each path
/// printed; with `tag` also update.txt there, signed by `key`, of the files at the GitHub release
/// by those names. The tag and the key are checked before anything is built.
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
      Os::Macos => {
        let notarize = sign::secrets(&sign::NOTARY, tag)?;
        let (app, _) = macos::app(cfg, &bin, tag)?;
        vec![dmg::dmg(cfg, &app, arch, tag, notarize, dir)?]
      }
      Os::Windows => vec![windows::setup(cfg, &bin, arch, tag, dir)?],
      Os::Linux => {
        vec![linux::appimage(cfg, &bin, arch, tag, dir)?, linux::deb(cfg, &bin, arch, tag, dir)?]
      }
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
  let body = feed::body(&cfg.version, Some(&notes), &url, &files)?;
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

/// `<name>-<label>-<end>` in `dir`, a file `release` writes: the label the `tag`, else `dev`.
pub fn out(dir: &Path, name: &str, tag: Option<&str>, end: &str) -> PathBuf {
  dir.join(format!("{name}-{}-{end}", tag.unwrap_or("dev")))
}

/// Removes `dir`'s update.txt and the files a run of any [label] writes, `<name>-<label>-<end>` of
/// each [Arch]'s ends, so no old feed, package of a triple gone from `dir` or other version's file
/// is left beside the new ones, which a glob would upload; and their parts a stopped run left.
fn clean(dir: &Path, name: &str) -> io::Result<()> {
  let ends: Vec<_> = Arch::ALL
    .into_iter()
    .flat_map(|a| {
      [
        format!("-macos-{a}.dmg"),
        format!("-windows-{}-setup.exe", a.windows()),
        format!("-linux-{a}.AppImage"),
        format!("-linux-{a}.deb"),
      ]
    })
    .collect();

  let ours = |f: &str| {
    let rest = f.strip_prefix(name).and_then(|f| f.strip_prefix('-')).unwrap_or_default();
    ends.iter().any(|end| rest.strip_suffix(end.as_str()).is_some_and(label))
  };

  for entry in fs::read_dir(dir)? {
    let entry = entry?;
    let file = entry.file_name();
    let file = file.to_str().map(|f| tool::part_of(f).unwrap_or(f));
    let stale = file.is_some_and(|f| f == "update.txt" || ours(f));
    if stale && !entry.file_type()?.is_dir() {
      fs::remove_file(entry.path())?;
    }
  }

  Ok(())
}

/// Whether `l` is a file name's label: `dev`, or a tag, `v<number>.<number>…`.
fn label(l: &str) -> bool {
  let Some(version) = l.strip_prefix('v') else { return l == "dev" };
  let parts: Vec<_> = version.split('.').take(2).collect();
  parts.len() == 2 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The binaries in `dir`, `<triple>/<bin>[.exe]` of each [Os] and [Arch], each with its OS and
/// arch, in the feed's order; an error when there is none.
fn binaries(dir: &Path, bin: &str) -> io::Result<Vec<(Os, Arch, PathBuf)>> {
  let all = Os::ALL.into_iter().flat_map(|os| Arch::ALL.map(|arch| (os, arch)));
  let triple = |os: Os, arch: Arch| format!("{arch}-{}", os.triple());
  let found: Vec<_> = all
    .clone()
    .map(|(os, arch)| {
      let exe = if os == Os::Windows { ".exe" } else { "" };
      (os, arch, dir.join(triple(os, arch)).join(format!("{bin}{exe}")))
    })
    .filter(|(_, _, path)| path.is_file())
    .collect();

  if found.is_empty() {
    let triples: Vec<_> = all.map(|(os, arch)| triple(os, arch)).collect();
    return Err(bad(format!(
      "{} holds no <triple>/{bin}[.exe], of {}",
      dir.display(),
      triples.join(", ")
    )));
  }

  Ok(found)
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
  let body = feed::body(&cfg.version, notes, url, files)?;
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

/// The error `e` of `path`, which it names: `<path>: <e>`.
fn at(path: &Path) -> impl Fn(io::Error) -> io::Error + '_ {
  move |e| wrap(&e, format!("{}: {e}", path.display()))
}

/// The error `e` said as `msg`, its kind kept.
fn wrap(e: &io::Error, msg: impl fmt::Display) -> io::Error {
  io::Error::new(e.kind(), msg.to_string())
}

#[cfg(test)]
mod tests {
  use std::fs;

  use super::*;

  fn args(line: &str) -> Result<Command, clap::Error> {
    parse(["shipo"].into_iter().chain(line.split_whitespace()).map(OsString::from))
  }

  #[test]
  fn commands() {
    let (manifest, cargo) = (PathBuf::from("app/Cargo.toml"), PathBuf::from("Cargo.toml"));
    let app = |manifest: &PathBuf| Command::App {
      manifest: manifest.clone(),
      bin: "x".into(),
      id: None,
      name: None,
      icon: None,
    };

    let cases = [
      ("app --bin x", app(&cargo)),
      ("app --manifest app/Cargo.toml --bin=x", app(&manifest)),
      (
        "app --id=me.dev --bin x --icon dev.png --name Dev",
        Command::App {
          manifest: cargo.clone(),
          bin: "x".into(),
          id: Some("me.dev".into()),
          name: Some("Dev".into()),
          icon: Some("dev.png".into()),
        },
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
      (
        "keygen",
        Command::Keygen {
          files: vec!["update.key".into(), "update-spare.key".into()],
          public: false,
        },
      ),
      ("keygen a.key", Command::Keygen { files: vec!["a.key".into()], public: false }),
      ("keygen --public a.key", Command::Keygen { files: vec!["a.key".into()], public: true }),
      ("keygen a.key --public", Command::Keygen { files: vec!["a.key".into()], public: true }),
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
      assert_eq!(args(line).map_err(|e| e.to_string()), Ok(want), "{line}");
    }

    let line = ["shipo", "app", "--bin", "x", "--name", "Wavo Dev"].map(OsString::from);
    let Ok(Command::App { name, .. }) = parse(line) else { panic!() };
    assert_eq!(name.as_deref(), Some("Wavo Dev"));
  }

  #[test]
  fn errors() {
    use ErrorKind::*;

    let cases = [
      ("", DisplayHelpOnMissingArgumentOrSubcommand, ""),
      ("--help", DisplayHelp, ""),
      ("feed -h --bogus", DisplayHelp, "Usage: shipo feed"),
      ("pack", InvalidSubcommand, "'pack'"),
      ("--manifest Cargo.toml app", UnknownArgument, "'--manifest'"),
      ("app", MissingRequiredArgument, "--bin <file>"),
      ("app x --bin y", UnknownArgument, "'x'"),
      ("app --tag v1", UnknownArgument, "'--tag'"),
      ("app --bin x --id a.b --id c.d", ArgumentConflict, "'--id <id>'"),
      ("app --bin x --name", InvalidValue, "'--name <name>'"),
      ("app --bin x --icon --name y", InvalidValue, "'--icon <png>'"),
      ("release out --id me.dev", UnknownArgument, "'--id'"),
      ("release out --name Dev", UnknownArgument, "'--name'"),
      ("feed --url https://x a.zip --icon dev.png", UnknownArgument, "'--icon'"),
      ("keygen --id me.dev", UnknownArgument, "'--id'"),
      ("release", MissingRequiredArgument, "<dir>"),
      ("release a b", UnknownArgument, "'b'"),
      ("feed a.zip", MissingRequiredArgument, "--url <base>"),
      ("feed --url https://x", MissingRequiredArgument, "<files>..."),
      ("feed --url", InvalidValue, "'--url <base>'"),
      ("feed --url --notes x a.zip", InvalidValue, "'--url <base>'"),
      ("feed --url https://x a.zip --url https://y", ArgumentConflict, "'--url <base>'"),
      ("release out --tag v1 --tag=v2", ArgumentConflict, "'--tag <tag>'"),
      ("release out --key -x", UnknownArgument, "'-x'"),
      ("app --bin x --manifest a --manifest b", ArgumentConflict, "'--manifest <file>'"),
      ("keygen --public --public a.key", ArgumentConflict, "'--public'"),
      ("keygen --manifest Cargo.toml", UnknownArgument, "'--manifest'"),
      ("keygen --key k", UnknownArgument, "'--key'"),
      ("keygen --public", WrongNumberOfValues, "--public takes one key file"),
      ("keygen --public a b", WrongNumberOfValues, "Usage: shipo keygen"),
    ];

    for (line, kind, has) in cases {
      let e = args(line).unwrap_err();
      let text = e.to_string();
      assert_eq!(e.kind(), kind, "{line}: {text}");
      assert!(text.contains(has), "{line}: {text}");
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
      (Os::Macos, Arch::X86_64, "x86_64-apple-darwin/hello"),
      (Os::Windows, Arch::X86_64, "x86_64-pc-windows-msvc/hello.exe"),
      (Os::Linux, Arch::Aarch64, "aarch64-unknown-linux-gnu/hello"),
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
  fn names() {
    let dir = Path::new("dist");
    assert_eq!(out(dir, "Wavo", None, "linux-x86_64.deb"), dir.join("Wavo-dev-linux-x86_64.deb"));
    assert_eq!(
      out(dir, "Wavo Pro", Some("v1.2.0"), "macos-aarch64.dmg"),
      dir.join("Wavo Pro-v1.2.0-macos-aarch64.dmg")
    );
  }

  #[test]
  fn labels() {
    for l in ["dev", "v1.2", "v1.2.0", "v1.2.0-rc.1", "v10.0.0+b5"] {
      assert!(label(l), "{l}");
    }

    for l in ["", "devx", "Pro-dev", "v", "v1", "vx.1", "v2-v1.0.0", "v1..0", "1.2.0"] {
      assert!(!label(l), "{l}");
    }
  }

  #[test]
  fn cleans() {
    let tmp = tool::Temp::new().unwrap();
    let names = [
      "Wavo-dev-linux-x86_64.deb",
      "Wavo-dev-windows-x64-setup.exe",
      "update.txt",
      "Wavo-v1.2.0-linux-x86_64.deb",
      "Wavo-v1.1.0-rc.1-macos-aarch64.dmg",
      "Wavo-v1.2.0-windows-arm64-setup.exe",
      "Wavo-v1.2.0-linux-aarch64.AppImage",
      "Wavo-devx.txt",
      "Wavo-dev-linux-x86_64.zip",
      "Wavo-v1.2.0-notes.md",
      "Wavo-v2-v1.0.0-linux-x86_64.deb",
      "Wavo Pro-dev-linux-x86_64.deb",
      "Other-dev-linux-x86_64.deb",
      "notes.md",
      "Wavo-dev-linux-x86_64.deb.123-456-7.part",
      "update.txt.1-2-3.part",
      "notes.md.1-2-3.part",
      "Wavo-dev-linux-x86_64.deb.x.part",
    ];

    for name in names {
      fs::write(tmp.join(name), b"").unwrap();
    }

    fs::create_dir(tmp.join("Wavo-dev-folder")).unwrap();
    clean(&tmp, "Wavo").unwrap();
    let want = [
      "Other-dev-linux-x86_64.deb",
      "Wavo Pro-dev-linux-x86_64.deb",
      "Wavo-dev-folder",
      "Wavo-dev-linux-x86_64.deb.x.part",
      "Wavo-dev-linux-x86_64.zip",
      "Wavo-devx.txt",
      "Wavo-v1.2.0-notes.md",
      "Wavo-v2-v1.0.0-linux-x86_64.deb",
      "notes.md",
      "notes.md.1-2-3.part",
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
