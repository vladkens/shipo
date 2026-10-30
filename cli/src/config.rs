// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The app's config, read from its `Cargo.toml`: `[package.metadata.shipo]`, the package's fields
//! with the workspace's inheritance, and the binary and the target folder cargo would use. Read as
//! TOML, so cargo is not needed on the host.

use std::ffi::OsString;
use std::path::{self, Component, Path, PathBuf};
use std::{fs, io};

use toml::{Table, Value};

use crate::bad;

/// The app as its `[package.metadata.shipo]` and its package give it.
#[derive(Debug)]
#[cfg_attr(not(test), expect(dead_code, reason = "the commands read it in the next steps"))]
pub struct Config {
  /// The reverse-DNS id: the bundle id, the `.desktop`'s `StartupWMClass`.
  pub id: String,
  /// The name people see, also the release files' prefix.
  pub name: String,
  /// The icon's PNG, from the manifest's folder.
  pub icon: PathBuf,
  /// A line about the app; empty if none.
  pub comment: String,
  pub version: String,
  /// The binary's name, without `.exe`.
  pub bin: String,
  /// `[package.metadata.shipo.macos]` as it is; empty if none.
  pub macos: Table,
  /// `[package.metadata.shipo.windows]` as it is; empty if none.
  pub windows: Table,
  /// `[package.metadata.shipo.linux]` as it is; empty if none.
  pub linux: Table,
  /// The config's, else the first author without ` <mail>`; empty if neither.
  pub publisher: String,
  pub authors: Vec<String>,
  /// The package's repository without a trailing `/` and `.git`; empty if none.
  pub repository: String,
  /// The manifest's folder.
  pub root: PathBuf,
  /// The workspace's root folder, else the manifest's.
  pub workspace: PathBuf,
  /// Cargo's target folder, where the builds are.
  pub target: PathBuf,
}

/// A workspace root: its folder and its `[workspace.package]`, the values a member inherits.
struct Workspace {
  dir: PathBuf,
  package: Table,
}

/// The keys of `[package.metadata.shipo]` and of its tables, so a typo is an error; `plist`'s keys
/// are the app's own.
const KEYS: &[&str] = &["id", "name", "icon", "comment", "publisher", "macos", "windows", "linux"];
const MACOS: &[&str] = &["accessory", "entitlements", "plist"];
const WINDOWS: &[&str] = &["app-id", "copyright"];
const LINUX: &[&str] = &["categories", "deb-package", "deb-depends", "deb-recommends"];

/// What reads an environment variable: [std::env::var_os], or a test's.
type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

impl Config {
  /// The config of the package whose manifest is `manifest`, a `Cargo.toml`.
  pub fn load(manifest: &Path) -> io::Result<Self> {
    Self::read(manifest, &|name| std::env::var_os(name))
  }

  /// [Config::load] with `env` for the environment.
  fn read(manifest: &Path, env: Env) -> io::Result<Self> {
    let manifest = normalize(&path::absolute(manifest)?);
    let root = manifest.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    let no = |what: &str| bad(format!("{} has no {what}", manifest.display()));
    let toml = table(&manifest)?;
    let Some(pkg) = toml.get("package").and_then(Value::as_table) else {
      return Err(bad(format!("{} is no package", manifest.display())));
    };

    let shipo = pkg.get("metadata").and_then(|m| m.get("shipo")).and_then(Value::as_table);
    let shipo = shipo.ok_or_else(|| no("[package.metadata.shipo]"))?;
    known(shipo, "[package.metadata.shipo]", KEYS)?;
    let required = |key: &str| shipo.get(key).and_then(Value::as_str);
    let (Some(id), Some(name), Some(icon)) = (required("id"), required("name"), required("icon"))
    else {
      let missing = ["id", "name", "icon"].into_iter().filter(|k| required(k).is_none());
      let missing: Vec<_> = missing.collect();
      return Err(bad(format!("[package.metadata.shipo] needs {}", missing.join(", "))));
    };

    let meta = |key: &str| text(shipo.get(key), &format!("[package.metadata.shipo] {key}"));
    let ws = workspace(&root, &toml, pkg)?;
    let field = |key: &str| inherit(pkg, ws.as_ref(), key);

    let package = text(pkg.get("name"), "package.name")?.ok_or_else(|| no("name"))?;
    let version = text(field("version")?, "package.version")?.ok_or_else(|| no("version"))?;
    let edition = text(field("edition")?, "package.edition")?;
    let authors = match field("authors")? {
      None => Some(vec![]),
      Some(v) => {
        v.as_array().and_then(|a| a.iter().map(|x| x.as_str().map(String::from)).collect())
      }
    };

    let authors = authors.ok_or_else(|| bad("package.authors is not a list of strings"))?;

    let author = authors.first().map_or("", |a| a.split(" <").next().unwrap_or(a));
    let publisher = meta("publisher")?.filter(|p| !p.is_empty()).unwrap_or_else(|| author.into());
    let repository = text(field("repository")?, "package.repository")?.unwrap_or_default();
    let repository = repository.trim_end_matches('/');
    let repository = repository.strip_suffix(".git").unwrap_or(repository).to_string();
    let bins = bins(&toml, &package, edition.as_deref().unwrap_or("2015"), &root)?;
    let bin = match bins.as_slice() {
      [one] => one.clone(),
      _ => text(pkg.get("default-run"), "package.default-run")?.unwrap_or(package),
    };

    let workspace = ws.as_ref().map_or_else(|| root.clone(), |w| w.dir.clone());
    let target = target(env, &workspace)?;
    Ok(Self {
      id: id.into(),
      name: name.into(),
      icon: root.join(icon),
      comment: meta("comment")?.unwrap_or_default(),
      version,
      bin,
      macos: subtable(shipo, "macos", MACOS)?,
      windows: subtable(shipo, "windows", WINDOWS)?,
      linux: subtable(shipo, "linux", LINUX)?,
      publisher,
      authors,
      repository,
      root,
      workspace,
      target,
    })
  }
}

/// The TOML file at `path`, its name in the errors.
fn table(path: &Path) -> io::Result<Table> {
  let text = fs::read_to_string(path);
  let text = text.map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
  text.parse().map_err(|e: toml::de::Error| bad(format!("{}: {e}", path.display())))
}

/// `value` as a string; `None` if there is none, an error naming `what` if it is no string.
fn text(value: Option<&Value>, what: &str) -> io::Result<Option<String>> {
  match value {
    None => Ok(None),
    Some(Value::String(s)) => Ok(Some(s.clone())),
    Some(_) => Err(bad(format!("{what} is not a string"))),
  }
}

/// Checks that `table`, `what` in the error, has no key but `keys`.
fn known(table: &Table, what: &str, keys: &[&str]) -> io::Result<()> {
  let unknown: Vec<_> = table.keys().map(String::as_str).filter(|k| !keys.contains(k)).collect();
  match unknown.is_empty() {
    true => Ok(()),
    false => Err(bad(format!("{what} takes {}, not {}", keys.join(", "), unknown.join(", ")))),
  }
}

/// `[package.metadata.shipo.<key>]` of the keys `keys`, empty if there is none.
fn subtable(shipo: &Table, key: &str, keys: &[&str]) -> io::Result<Table> {
  let what = format!("[package.metadata.shipo.{key}]");
  match shipo.get(key) {
    None => Ok(Table::new()),
    Some(Value::Table(t)) => known(t, &what, keys).map(|_| t.clone()),
    Some(_) => Err(bad(format!("{what} is not a table"))),
  }
}

/// The workspace the package in `dir` (its manifest parsed, `toml`) is in: at `package.workspace`'s
/// folder when it is set, else its own manifest's `[workspace]`, else the nearest folder up whose
/// `Cargo.toml` has a `[workspace]` that doesn't [exclude](excluded) it; `None` if there is none.
/// Cargo's `members` are not checked.
fn workspace(dir: &Path, toml: &Table, pkg: &Table) -> io::Result<Option<Workspace>> {
  let of = |dir: &Path, toml: &Table| {
    let package = toml["workspace"].get("package").and_then(Value::as_table);
    Workspace { dir: dir.to_path_buf(), package: package.cloned().unwrap_or_default() }
  };

  if let Some(path) = text(pkg.get("workspace"), "package.workspace")? {
    let dir = normalize(&dir.join(path));
    let file = dir.join("Cargo.toml");
    let toml = table(&file)?;
    return match toml.contains_key("workspace") {
      true => Ok(Some(of(&dir, &toml))),
      false => Err(bad(format!("{} has no [workspace]", file.display()))),
    };
  }

  if toml.contains_key("workspace") {
    return Ok(Some(of(dir, toml)));
  }

  for root in dir.ancestors().skip(1) {
    let file = root.join("Cargo.toml");
    if !file.is_file() {
      continue;
    }

    let toml = table(&file)?;
    if toml.get("workspace").is_some_and(|ws| !excluded(ws, root, dir)) {
      return Ok(Some(of(root, &toml)));
    }
  }

  Ok(None)
}

/// Whether the `[workspace]` `ws` in `root` leaves out the package in `dir`, as cargo's
/// `is_excluded`: `dir` is in a folder its `exclude` lists and in none its `members` lists, each
/// taken as a path, not a glob.
fn excluded(ws: &Value, root: &Path, dir: &Path) -> bool {
  let listed = |key: &str| {
    let paths = ws.get(key).and_then(Value::as_array).into_iter().flatten();
    paths.filter_map(Value::as_str).any(|p| dir.starts_with(normalize(&root.join(p))))
  };

  listed("exclude") && !listed("members")
}

/// The package's `key`: its own value, or for `{ workspace = true }` its workspace's, an error if
/// that has none; `None` if the package has no `key`.
fn inherit<'a>(
  pkg: &'a Table,
  ws: Option<&'a Workspace>,
  key: &str,
) -> io::Result<Option<&'a Value>> {
  let Some(value) = pkg.get(key) else { return Ok(None) };
  if value.get("workspace").and_then(Value::as_bool) != Some(true) {
    return Ok(Some(value));
  }

  let Some(ws) = ws else {
    return Err(bad(format!("package.{key} is the workspace's, but the package is in none")));
  };

  match ws.package.get(key) {
    Some(value) => Ok(Some(value)),
    None => Err(bad(format!(
      "package.{key} is the workspace's, but {}'s [workspace.package] has no {key}",
      ws.dir.join("Cargo.toml").display()
    ))),
  }
}

/// The package's bin targets as cargo finds them in `toml`, the manifest in `dir`, of `edition`:
/// the `[[bin]]`s, then unless `autobins = false`, `src/main.rs` named `package`,
/// `src/bin/<name>.rs` and `src/bin/<name>/main.rs`. One per name; none for a file a `[[bin]]`
/// builds already. Edition 2015 infers none beside a `[[bin]]` unless `autobins = true`.
fn bins(toml: &Table, package: &str, edition: &str, dir: &Path) -> io::Result<Vec<String>> {
  let (mut names, mut paths) = (vec![], vec![]);
  for bin in toml.get("bin").and_then(Value::as_array).into_iter().flatten() {
    let Some(name) = bin.get("name").and_then(Value::as_str) else {
      return Err(bad("a [[bin]] has no name"));
    };

    if let Some(path) = bin.get("path").and_then(Value::as_str) {
      paths.push(normalize(Path::new(path)));
    }

    if !names.iter().any(|n| n == name) {
      names.push(name.to_string());
    }
  }

  let autobins = toml.get("package").and_then(|p| p.get("autobins")).and_then(Value::as_bool);
  if !autobins.unwrap_or(edition != "2015" || names.is_empty()) {
    return Ok(names);
  }

  let mut found = vec![];
  if dir.join("src/main.rs").is_file() {
    found.push((package.to_string(), PathBuf::from("src/main.rs")));
  }

  for entry in fs::read_dir(dir.join("src/bin")).into_iter().flatten() {
    let entry = entry?;
    let (path, file) = (entry.path(), entry.file_name().to_string_lossy().into_owned());
    match file.strip_suffix(".rs") {
      Some(stem) if path.is_file() => found.push((stem.into(), Path::new("src/bin").join(&file))),
      None if path.join("main.rs").is_file() => {
        found.push((file.clone(), Path::new("src/bin").join(&file).join("main.rs")))
      }
      _ => {}
    }
  }

  for (name, path) in found {
    if !names.contains(&name) && !paths.contains(&path) {
      names.push(name);
    }
  }

  Ok(names)
}

/// `path` without its `.` parts, each `..` dropping the part before it, as cargo's
/// `normalize_path`, without looking at the disk.
fn normalize(path: &Path) -> PathBuf {
  let mut out = PathBuf::new();
  for part in path.components() {
    match part {
      Component::CurDir => {}
      Component::ParentDir
        if matches!(out.components().next_back(), Some(Component::Normal(_))) =>
      {
        out.pop();
      }
      part => out.push(part),
    }
  }

  out
}

/// Cargo's target folder: `CARGO_TARGET_DIR`, else `CARGO_BUILD_TARGET_DIR`, from the current
/// folder when relative; else `target` in the workspace's folder `ws`. `build.target-dir` of
/// `.cargo/config.toml` is not read.
fn target(env: Env, ws: &Path) -> io::Result<PathBuf> {
  let vars = ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"].into_iter();
  match vars.filter_map(env).find(|v| !v.is_empty()) {
    Some(dir) => path::absolute(dir),
    None => Ok(ws.join("target")),
  }
}

#[cfg(test)]
mod tests {
  use std::ops::Deref;

  use super::*;

  /// A new folder in the temp dir with `(path, text)` files, removed on drop.
  struct Dir(PathBuf);

  impl Dir {
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
      let dir = std::env::temp_dir().join(format!("shipo-cli-{}-{name}", std::process::id()));
      let _ = fs::remove_dir_all(&dir);
      for (path, text) in files {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
      }

      fs::create_dir_all(&dir).unwrap();
      Self(dir)
    }
  }

  impl Deref for Dir {
    type Target = Path;
    fn deref(&self) -> &Path {
      &self.0
    }
  }

  impl Drop for Dir {
    fn drop(&mut self) {
      let _ = fs::remove_dir_all(&self.0);
    }
  }

  /// The smallest config, its `[package]` ending at the top so a test can add to it.
  const APP: &str = concat!(
    "[package]\nname = \"app\"\nversion = \"1.2.0\"\nedition = \"2021\"\n",
    "[package.metadata.shipo]\nid = \"me.app\"\nname = \"App\"\nicon = \"icon.png\"\n",
  );

  /// [APP] with `package` added to its `[package]`, in place of its edition if it has one, and
  /// `rest` after.
  fn app(package: &str, rest: &str) -> String {
    let app = match package.contains("edition") {
      true => APP.replace("edition = \"2021\"\n", ""),
      false => APP.into(),
    };

    app.replacen("[package]\n", &format!("[package]\n{package}\n"), 1) + rest
  }

  /// An environment without variables.
  fn none(_: &str) -> Option<OsString> {
    None
  }

  fn load(name: &str, files: &[(&str, &str)], manifest: &str) -> io::Result<Config> {
    let dir = Dir::new(name, files);
    Config::read(&dir.join(manifest), &none)
  }

  fn error(name: &str, files: &[(&str, &str)], manifest: &str) -> String {
    load(name, files, manifest).unwrap_err().to_string()
  }

  #[test]
  fn minimal() {
    let dir = Dir::new("minimal", &[("Cargo.toml", APP)]);
    let c = Config::read(&dir.join("Cargo.toml"), &none).unwrap();
    assert_eq!((c.id.as_str(), c.name.as_str(), c.version.as_str()), ("me.app", "App", "1.2.0"));
    assert_eq!((c.icon, c.root), (dir.join("icon.png"), dir.to_path_buf()));
    assert_eq!((c.bin.as_str(), c.comment.as_str(), c.publisher.as_str()), ("app", "", ""));
    assert!(c.macos.is_empty() && c.windows.is_empty() && c.linux.is_empty());
    assert_eq!((c.authors, c.repository), (vec![], String::new()));
    assert_eq!((c.workspace, c.target), (dir.to_path_buf(), dir.join("target")));
  }

  #[test]
  fn required() {
    let none = error("none", &[], "Cargo.toml");
    assert!(none.contains("Cargo.toml: "), "{none}");

    let ws = error("no-package", &[("Cargo.toml", "[workspace]\n")], "Cargo.toml");
    assert!(ws.ends_with("Cargo.toml is no package"), "{ws}");

    let plain = "[package]\nname = \"app\"\nversion = \"1.2.0\"\n";
    let plain = error("no-shipo", &[("Cargo.toml", plain)], "Cargo.toml");
    assert!(plain.ends_with("Cargo.toml has no [package.metadata.shipo]"), "{plain}");

    let some = APP.replace("id = \"me.app\"\n", "").replace("icon = \"icon.png\"", "icon = 1");
    let some = error("some", &[("Cargo.toml", &some)], "Cargo.toml");
    assert_eq!(some, "[package.metadata.shipo] needs id, icon");

    let version = APP.replace("version = \"1.2.0\"", "");
    let version = error("no-version", &[("Cargo.toml", &version)], "Cargo.toml");
    assert!(version.ends_with("Cargo.toml has no version"), "{version}");

    let comment = app("", "comment = 1\n");
    let comment = error("comment", &[("Cargo.toml", &comment)], "Cargo.toml");
    assert_eq!(comment, "[package.metadata.shipo] comment is not a string");

    let macos = app("", "macos = \"yes\"\n");
    let macos = error("macos", &[("Cargo.toml", &macos)], "Cargo.toml");
    assert_eq!(macos, "[package.metadata.shipo.macos] is not a table");

    let typo = app("", "nmae = \"App\"\npublisher = \"Me\"\nlinx = {}\n");
    let typo = error("typo", &[("Cargo.toml", &typo)], "Cargo.toml");
    let keys = "id, name, icon, comment, publisher, macos, windows, linux";
    assert_eq!(typo, format!("[package.metadata.shipo] takes {keys}, not linx, nmae"));

    let typo = app("", "[package.metadata.shipo.linux]\ndeb-recomends = []\n");
    let typo = error("typo-linux", &[("Cargo.toml", &typo)], "Cargo.toml");
    let keys = "categories, deb-package, deb-depends, deb-recommends";
    assert_eq!(typo, format!("[package.metadata.shipo.linux] takes {keys}, not deb-recomends"));

    let typo = app("", "[package.metadata.shipo.macos]\nplist.Anything = 1\nicon = \"x\"\n");
    let typo = error("typo-macos", &[("Cargo.toml", &typo)], "Cargo.toml");
    assert_eq!(
      typo,
      "[package.metadata.shipo.macos] takes accessory, entitlements, plist, not icon"
    );

    let typo = app("", "[package.metadata.shipo.windows]\ncopyright = \"x\"\nappid = \"x\"\n");
    let typo = error("typo-windows", &[("Cargo.toml", &typo)], "Cargo.toml");
    assert_eq!(typo, "[package.metadata.shipo.windows] takes app-id, copyright, not appid");

    let bad = error("bad-toml", &[("Cargo.toml", "[package\n")], "Cargo.toml");
    assert!(bad.contains("Cargo.toml: TOML parse error"), "{bad}");
  }

  #[test]
  fn inherited() {
    let package = "version.workspace = true\nauthors.workspace = true\nrepository.workspace = true";
    let root = concat!(
      "[workspace]\nmembers = [\"app\"]\n[workspace.package]\nversion = \"2.0.0\"\n",
      "authors = [\"Jane Doe <jane@example.com>\"]\nrepository = \"https://github.com/me/app/\"\n",
    );
    let manifest = app(package, "").replace("version = \"1.2.0\"\n", "");
    let dir = Dir::new("inherited", &[("Cargo.toml", root), ("app/Cargo.toml", &manifest)]);
    let c = Config::read(&dir.join("app/Cargo.toml"), &none).unwrap();
    assert_eq!((c.version.as_str(), c.repository.as_str()), ("2.0.0", "https://github.com/me/app"));
    assert_eq!(
      (c.authors, c.publisher.as_str()),
      (vec!["Jane Doe <jane@example.com>".into()], "Jane Doe")
    );
    assert_eq!((c.root, c.target), (dir.join("app"), dir.join("target")));
    assert_eq!(c.workspace, dir.to_path_buf());

    // The package's own manifest is the workspace's root.
    let own = app(package, "[workspace]\n[workspace.package]\nversion = \"3.0.0\"\nauthors = []\n");
    let own = own.replace("version = \"1.2.0\"\n", "").replace("repository.workspace = true", "");
    let dir = Dir::new("own", &[("Cargo.toml", "[workspace]\n"), ("app/Cargo.toml", &own)]);
    let c = Config::read(&dir.join("app/Cargo.toml"), &none).unwrap();
    assert_eq!((c.version.as_str(), c.target), ("3.0.0", dir.join("app/target")));

    // `package.workspace` passes over the nearer root.
    let far = app("workspace = \"../real\"\nversion.workspace = true", "");
    let far = far.replace("version = \"1.2.0\"\n", "");
    let real = "[workspace]\n[workspace.package]\nversion = \"4.0.0\"\n";
    let files = [("Cargo.toml", root), ("app/Cargo.toml", far.as_str()), ("real/Cargo.toml", real)];
    let dir = Dir::new("far", &files);
    let c = Config::read(&dir.join("app/Cargo.toml"), &none).unwrap();
    assert_eq!((c.version.as_str(), c.target), ("4.0.0", dir.join("real/target")));
    assert_eq!(c.workspace, dir.join("real"));

    // From a path with `..` the walk up starts at the package's real folder.
    let c = Config::read(&dir.join("real/../app/Cargo.toml"), &none).unwrap();
    assert_eq!((c.root, c.target), (dir.join("app"), dir.join("real/target")));

    let files = [("app/Cargo.toml", far.as_str()), ("real/Cargo.toml", "[package]\n")];
    let e = error("far-none", &files, "app/Cargo.toml");
    assert!(e.ends_with("real/Cargo.toml has no [workspace]"), "{e}");
  }

  #[test]
  fn excluded() {
    let root = "[workspace]\nexclude = [\"./app\"]\n[workspace.package]\nversion = \"9.9.9\"\n";
    let dir = Dir::new("excluded", &[("Cargo.toml", root), ("app/Cargo.toml", APP)]);
    let c = Config::read(&dir.join("app/Cargo.toml"), &none).unwrap();
    assert_eq!((c.version.as_str(), c.target), ("1.2.0", dir.join("app/target")));
    assert_eq!(c.workspace, dir.join("app"));

    let manifest = app("version.workspace = true", "").replace("version = \"1.2.0\"\n", "");
    let e = error(
      "excluded-inherit",
      &[("Cargo.toml", root), ("app/Cargo.toml", &manifest)],
      "app/Cargo.toml",
    );
    assert_eq!(e, "package.version is the workspace's, but the package is in none");

    // A member listed by its path is in, under an excluded folder too; an outer root is passed by.
    let root = "[workspace]\nmembers = [\"app/inner\"]\nexclude = [\"app\"]\n";
    let files =
      [("Cargo.toml", root), ("app/inner/Cargo.toml", APP), ("app/other/Cargo.toml", APP)];
    let dir = Dir::new("member", &files);
    let c = Config::read(&dir.join("app/inner/Cargo.toml"), &none).unwrap();
    assert_eq!(c.target, dir.join("target"));
    let c = Config::read(&dir.join("app/other/Cargo.toml"), &none).unwrap();
    assert_eq!(c.target, dir.join("app/other/target"));
  }

  #[test]
  fn inherited_missing() {
    let manifest = app("repository.workspace = true", "");
    let root = "[workspace]\n[workspace.package]\nversion = \"2.0.0\"\n";
    let e =
      error("missing", &[("Cargo.toml", root), ("app/Cargo.toml", &manifest)], "app/Cargo.toml");
    assert!(e.starts_with("package.repository is the workspace's, but "), "{e}");
    assert!(e.ends_with("Cargo.toml's [workspace.package] has no repository"), "{e}");

    let manifest = app("authors = { workspace = true }", "");
    let e = error("no-workspace", &[("app/Cargo.toml", &manifest)], "app/Cargo.toml");
    assert_eq!(e, "package.authors is the workspace's, but the package is in none");
  }

  #[test]
  fn binary() {
    // Each: the lines added to `[package]`, the lines after the config, the empty sources, the bin.
    let (main, a, b) = ("src/main.rs", "src/bin/a.rs", "src/bin/b/main.rs");
    let cases = [
      ("", "", vec![], "app"),
      ("", "", vec![main], "app"),
      ("", "", vec![a], "a"),
      ("", "", vec![b], "b"),
      ("", "", vec!["src/bin/c/lib.rs"], "app"),
      ("", "", vec![main, a], "app"),
      ("default-run = \"a\"", "", vec![main, a, b], "a"),
      // The shipo-cli case: `[[bin]]` builds src/main.rs under another name.
      ("", "[[bin]]\nname = \"x\"\npath = \"./src/main.rs\"\n", vec![main], "x"),
      // `[[bin]]` of the package's name builds src/main.rs: one target.
      ("default-run = \"other\"", "[[bin]]\nname = \"app\"\n", vec![main], "app"),
      ("", "[[bin]]\nname = \"x\"\npath = \"src/x.rs\"\n", vec![main], "app"),
      ("autobins = false", "[[bin]]\nname = \"x\"\npath = \"src/x.rs\"\n", vec![main, a, b], "x"),
      ("autobins = false\ndefault-run = \"a\"", "", vec![main, a], "a"),
      ("default-run = \"y\"", "[[bin]]\nname = \"x\"\n[[bin]]\nname = \"y\"\n", vec![], "y"),
      // Edition 2015 infers none beside a `[[bin]]`, unless `autobins = true`.
      ("edition = \"2015\"", "[[bin]]\nname = \"x\"\npath = \"src/x.rs\"\n", vec![main], "x"),
      (
        "edition = \"2015\"\nautobins = true",
        "[[bin]]\nname = \"x\"\npath = \"src/x.rs\"\n",
        vec![main],
        "app",
      ),
      ("edition = \"2015\"", "", vec![a], "a"),
    ];

    for (i, (package, rest, sources, bin)) in cases.into_iter().enumerate() {
      let manifest = app(package, rest);
      let mut files: Vec<_> = sources.iter().map(|path| (*path, "")).collect();
      files.push(("Cargo.toml", manifest.as_str()));
      let c = load(&format!("bin-{i}"), &files, "Cargo.toml").unwrap();
      assert_eq!(c.bin, bin, "{package:?} {rest:?} {sources:?}");
    }

    // No edition is 2015.
    let old =
      APP.replace("edition = \"2021\"\n", "") + "[[bin]]\nname = \"x\"\npath = \"src/x.rs\"\n";
    let c = load("bin-2015", &[("Cargo.toml", &old), (main, "")], "Cargo.toml").unwrap();
    assert_eq!(c.bin, "x");

    let nameless = app("", "[[bin]]\npath = \"src/x.rs\"\n");
    assert_eq!(
      error("nameless", &[("Cargo.toml", &nameless)], "Cargo.toml"),
      "a [[bin]] has no name"
    );
  }

  #[test]
  fn publisher() {
    let cases = [
      ("", "", ""),
      ("authors = [\"Jane Doe <jane@example.com>\", \"Joe\"]", "", "Jane Doe"),
      ("authors = [\"Jane\"]", "", "Jane"),
      ("authors = [\"Jane\"]", "publisher = \"Acme\"\n", "Acme"),
      ("authors = [\"Jane\"]", "publisher = \"\"\n", "Jane"),
      ("", "publisher = \"Acme\"\n", "Acme"),
    ];

    for (i, (package, rest, publisher)) in cases.into_iter().enumerate() {
      let manifest = app(package, rest);
      let c = load(&format!("publisher-{i}"), &[("Cargo.toml", &manifest)], "Cargo.toml").unwrap();
      assert_eq!(c.publisher, publisher, "{package:?} {rest:?}");
    }

    let manifest = app("authors = \"Jane\"", "");
    let e = error("authors", &[("Cargo.toml", &manifest)], "Cargo.toml");
    assert_eq!(e, "package.authors is not a list of strings");
  }

  #[test]
  fn repository() {
    let cases = [
      ("https://github.com/me/app", "https://github.com/me/app"),
      ("https://github.com/me/app/", "https://github.com/me/app"),
      ("https://github.com/me/app.git", "https://github.com/me/app"),
      ("https://github.com/me/app.git/", "https://github.com/me/app"),
      ("https://github.com/me/app//", "https://github.com/me/app"),
      ("https://github.com/me/app.git.git", "https://github.com/me/app.git"),
    ];

    for (i, (repository, want)) in cases.into_iter().enumerate() {
      let manifest = app(&format!("repository = \"{repository}\""), "");
      let c = load(&format!("repository-{i}"), &[("Cargo.toml", &manifest)], "Cargo.toml").unwrap();
      assert_eq!(c.repository, want, "{repository}");
    }
  }

  #[test]
  fn target_dir() {
    let (ws, abs) = (std::env::temp_dir().join("ws"), std::env::temp_dir().join("elsewhere"));
    let cwd = std::env::current_dir().unwrap();
    let cases = [
      ("", "", ws.join("target")),
      ("out", "", cwd.join("out")),
      ("", "out", cwd.join("out")),
      (abs.to_str().unwrap(), "out", abs.clone()),
      ("", abs.to_str().unwrap(), abs.clone()),
    ];

    for (target_dir, build_target_dir, want) in cases {
      let env = |name: &str| match name {
        "CARGO_TARGET_DIR" => Some(target_dir.into()),
        "CARGO_BUILD_TARGET_DIR" => Some(build_target_dir.into()),
        _ => None,
      };
      assert_eq!(target(&env, &ws).unwrap(), want, "{target_dir:?} {build_target_dir:?}");
    }

    let dir = Dir::new("target", &[("Cargo.toml", APP)]);
    let env = |name: &str| (name == "CARGO_TARGET_DIR").then(|| abs.clone().into());
    let c = Config::read(&dir.join("Cargo.toml"), &env).unwrap();
    assert_eq!(c.target, abs);
  }

  /// Every key of `[package.metadata.shipo]` and of its tables, a nested `plist` key, and a table
  /// the config skips.
  const FULL: &str = r#"[package]
name = "hello-app"
version = "0.3.1"
edition = "2024"
authors = ["Jane Doe <jane@example.com>"]
repository = "https://github.com/example/hello.git/"
[package.metadata.shipo]
id = "com.example.hello"
name = "Hello"
icon = "assets/icon.png"
comment = "Says hello"
publisher = "Example"
[package.metadata.shipo.macos]
accessory = true
entitlements = "assets/hello.entitlements"
plist.LSEnvironment.HELLO_MODE = "quiet"
[package.metadata.shipo.windows]
app-id = "00000000-1111-0000-1111-000000000000"
copyright = "(c) Jane Doe"
[package.metadata.shipo.linux]
categories = ["Utility"]
deb-package = "hello"
deb-depends = ["libc6"]
deb-recommends = ["libasound2"]
[dependencies]
anyhow = "1"
"#;

  #[test]
  fn full() {
    let dir = Dir::new("full", &[("Cargo.toml", FULL), ("src/main.rs", "")]);
    let c = Config::read(&dir.join("Cargo.toml"), &none).unwrap();
    assert_eq!(
      (c.id.as_str(), c.name.as_str(), c.bin.as_str()),
      ("com.example.hello", "Hello", "hello-app")
    );
    assert_eq!(
      (c.version.as_str(), c.comment.as_str(), c.publisher.as_str()),
      ("0.3.1", "Says hello", "Example")
    );
    assert_eq!(c.authors, ["Jane Doe <jane@example.com>"]);
    assert_eq!(c.repository, "https://github.com/example/hello");
    assert_eq!(c.icon, dir.join("assets/icon.png"));
    assert_eq!((c.root, c.target), (dir.to_path_buf(), dir.join("target")));

    assert_eq!(c.macos["accessory"].as_bool(), Some(true));
    assert_eq!(c.macos["entitlements"].as_str(), Some("assets/hello.entitlements"));
    assert_eq!(c.macos["plist"]["LSEnvironment"]["HELLO_MODE"].as_str(), Some("quiet"));
    assert_eq!(c.windows["app-id"].as_str(), Some("00000000-1111-0000-1111-000000000000"));
    assert_eq!(c.windows["copyright"].as_str(), Some("(c) Jane Doe"));
    assert_eq!(c.linux["categories"][0].as_str(), Some("Utility"));
    assert_eq!(c.linux["deb-package"].as_str(), Some("hello"));
    assert_eq!(c.linux["deb-depends"][0].as_str(), Some("libc6"));
    assert_eq!(c.linux["deb-recommends"][0].as_str(), Some("libasound2"));
  }
}
