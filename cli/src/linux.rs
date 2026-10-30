// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The Linux packages of the app's binary: its `.desktop` entry, the AppImage (the type2 runtime
//! with a squashfs of the AppDir appended) and the `.deb`, both built in memory.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use std::{io, iter};

use toml::Value;

use crate::config::Config;
use crate::image::{Image, icons};
use crate::tree::Tree;
use crate::{bad, deb, squashfs, tool};

/// The release of the AppImage type2 runtime: static, so the AppImage needs no libfuse2.
const RUNTIME: &str = "20251108";
/// The runtime's SHA-256 per arch.
const RUNTIME_SHA256: [(&str, &str); 2] = [
  ("x86_64", "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"),
  ("aarch64", "00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444"),
];

/// The AppImage's icon's side in px: hicolor has no folder past 512.
const ICON: usize = 512;
/// The `.deb`'s icons' sides in px, each in its hicolor folder.
const HICOLOR: [usize; 8] = [16, 24, 32, 48, 64, 128, 256, 512];

/// The characters an `Exec` argument holds only in quotes, by the desktop entry spec.
const RESERVED: &str = " \t\n\"'\\><~|&;$*?#()`";

// MARK: Desktop entry

/// The `.desktop` entry, `<id>.desktop`: the app started by its binary's name on the PATH, its
/// windows matched to the entry by the app's id (X11's `WM_CLASS`, Wayland's app id), in
/// `linux.categories`, `Utility` if none.
pub fn desktop(cfg: &Config) -> io::Result<String> {
  let categories = strings(cfg, "categories")?;
  let categories = match categories.is_empty() {
    true => vec!["Utility".into()],
    false => categories,
  };

  let mut out = format!("[Desktop Entry]\nType=Application\nName={}\n", escape(&cfg.name));
  if !cfg.comment.is_empty() {
    let _ = writeln!(out, "Comment={}", escape(&cfg.comment));
  }

  let _ = writeln!(out, "Exec={}", escape(&exec(&cfg.bin)));
  let _ = writeln!(out, "Icon={}", escape(&cfg.id));
  let _ = writeln!(out, "StartupWMClass={}", escape(&cfg.id));
  out += "StartupNotify=true\nTerminal=false\n";
  let list: String = categories.iter().map(|c| escape(c).replace(';', "\\;") + ";").collect();
  let _ = writeln!(out, "Categories={list}");
  Ok(out)
}

/// `text` as a desktop entry's string value: `\\`, `\n`, `\t`, `\r`, and `\s` for a space at
/// either end, which a reader would trim.
fn escape(text: &str) -> String {
  let last = text.chars().count().saturating_sub(1);
  let mut out = String::new();
  for (i, c) in text.chars().enumerate() {
    match c {
      '\\' => out += "\\\\",
      '\n' => out += "\\n",
      '\t' => out += "\\t",
      '\r' => out += "\\r",
      ' ' if i == 0 || i == last => out += "\\s",
      c => out.push(c),
    }
  }

  out
}

/// `arg` as an argument of `Exec`, before [escape]: `%` doubled, not a field code; in double
/// quotes when it holds a [RESERVED] character, each `"`, `` ` ``, `$` and `\` in it after a `\`.
fn exec(arg: &str) -> String {
  let arg = arg.replace('%', "%%");
  if !arg.contains(|c| RESERVED.contains(c)) {
    return arg;
  }

  let quoted =
    arg.chars().flat_map(|c| iter::once('\\').filter(move |_| "\"`$\\".contains(c)).chain([c]));
  format!("\"{}\"", quoted.collect::<String>())
}

/// The `linux` table's list `key`, empty if none.
fn strings(cfg: &Config, key: &str) -> io::Result<Vec<String>> {
  let not = || bad(format!("[package.metadata.shipo.linux] {key} is not a list of strings"));
  match cfg.linux.get(key) {
    None => Ok(vec![]),
    Some(Value::Array(a)) => {
      a.iter().map(|v| v.as_str().map(String::from).ok_or_else(not)).collect()
    }
    Some(_) => Err(not()),
  }
}

/// The icon's visible tile at each of `sizes`, PNGs by size: docks show the image as it is, so
/// the margin a macOS icon keeps would shrink it.
fn pngs(cfg: &Config, sizes: &[usize]) -> io::Result<BTreeMap<usize, Vec<u8>>> {
  let images = icons(&Image::read(&cfg.icon)?, sizes, true)?;
  images.into_iter().map(|(n, image)| Ok((n, image.encode()?))).collect()
}

// MARK: AppImage

/// `<Name>-dev-linux-<arch>.AppImage` in `out_dir` of the binary `bin`, 0755: the type2 runtime of
/// `arch`, fetched into `<target>/tools` once and pinned by SHA-256, with the squashfs of the
/// [appdir] appended. The runtime mounts it, by FUSE or unpacked, and sets `APPIMAGE` and `APPDIR`.
pub fn appimage(cfg: &Config, bin: &Path, arch: &str, out_dir: &Path) -> io::Result<PathBuf> {
  let Some(&(_, sha256)) = RUNTIME_SHA256.iter().find(|(a, _)| *a == arch) else {
    return Err(bad(format!("the Linux arch is x86_64 or aarch64, not {arch}")));
  };

  let icon = pngs(cfg, &[ICON])?.remove(&ICON).unwrap_or_default();
  let url =
    format!("https://github.com/AppImage/type2-runtime/releases/download/{RUNTIME}/runtime-{arch}");
  let runtime = cfg.target.join("tools").join(format!("runtime-{RUNTIME}-{arch}"));
  let runtime = tool::fetch(&url, sha256, &runtime)?;

  let mtime = u32::try_from(mtime(bin)?).map_err(|_| bad("the squashfs's time is past 2106"))?;
  let squashfs = squashfs::build(&appdir(cfg, read(bin)?, icon)?, mtime)?;
  let out = out_dir.join(format!("{}-dev-linux-{arch}.AppImage", cfg.name));
  place(&out, |part| {
    let mut file = File::create(part)?;
    io::copy(&mut File::open(&runtime)?, &mut file)?;
    io::Write::write_all(&mut file, &squashfs)?;
    file.sync_all()?;
    tool::set_mode(part, 0o755)
  })?;
  Ok(out)
}

/// The AppDir of the binary `bin` and the icon's PNG: `usr/bin/<bin>` (0755), `AppRun` a link to
/// it, `<id>.desktop`, `<id>.png` and `.DirIcon` a link to it.
fn appdir(cfg: &Config, bin: Vec<u8>, icon: Vec<u8>) -> io::Result<Tree> {
  let mut tree = Tree::default();
  tree.file(&format!("usr/bin/{}", cfg.bin), 0o755, bin);
  tree.file(&format!("{}.desktop", cfg.id), 0o644, desktop(cfg)?);
  tree.file(&format!("{}.png", cfg.id), 0o644, icon);
  tree.link("AppRun", &format!("usr/bin/{}", cfg.bin));
  tree.link(".DirIcon", &format!("{}.png", cfg.id));
  Ok(tree)
}

// MARK: Debian package

/// `<Name>-dev-linux-<arch>.deb` in `out_dir` of the binary `bin`: `/usr/bin/<bin>`, the
/// `.desktop` entry, the icon at each [HICOLOR] size and the [license] as the copyright. Its
/// version is a `dev` build's or a release's.
pub fn deb(cfg: &Config, bin: &Path, arch: &str, dev: bool, out_dir: &Path) -> io::Result<PathBuf> {
  let pkg = package(cfg)?;
  let pngs = pngs(cfg, &HICOLOR)?;
  let license = license(cfg);
  if license.is_none() {
    eprintln!(
      "shipo: no LICENSE in {} or its workspace: the .deb has no copyright",
      cfg.root.display()
    );
  }

  let mut tree = Tree::default();
  tree.file(&format!("usr/bin/{}", cfg.bin), 0o755, read(bin)?);
  tree.file(&format!("usr/share/applications/{}.desktop", cfg.id), 0o644, desktop(cfg)?);
  for (n, png) in pngs {
    tree.file(&format!("usr/share/icons/hicolor/{n}x{n}/apps/{}.png", cfg.id), 0o644, png);
  }

  if let Some(license) = license {
    tree.file(&format!("usr/share/doc/{pkg}/copyright"), 0o644, read(&license)?);
  }

  let control = control(cfg, &pkg, arch, dev, tree.size())?;
  let data = deb::build(&control, &tree, mtime(bin)?)?;
  let out = out_dir.join(format!("{}-dev-linux-{arch}.deb", cfg.name));
  place(&out, |part| fs::write(part, &data))?;
  Ok(out)
}

/// The `.deb`'s control file for the files of `size` bytes: [package] `pkg`, [deb_version] of a
/// dev build if `dev`, [maintainer], `linux.deb-depends` and `deb-recommends`, and the
/// [description]. A field holds one line; an empty one is left out.
fn control(cfg: &Config, pkg: &str, arch: &str, dev: bool, size: u64) -> io::Result<String> {
  let fields = [
    ("Package", pkg.to_string()),
    ("Version", deb_version(&cfg.version, dev)),
    ("Architecture", deb_arch(arch)?.into()),
    ("Maintainer", maintainer(cfg)?),
    ("Installed-Size", size.div_ceil(1024).to_string()),
    ("Depends", strings(cfg, "deb-depends")?.join(", ")),
    ("Recommends", strings(cfg, "deb-recommends")?.join(", ")),
    ("Section", "utils".into()),
    ("Priority", "optional".into()),
  ];

  let mut out = String::new();
  for (key, value) in fields {
    if value.contains(['\n', '\r']) {
      return Err(bad(format!("the .deb's {key} is one line, not {value:?}")));
    }

    if !value.is_empty() {
      let _ = writeln!(out, "{key}: {value}");
    }
  }

  Ok(out + &description(cfg))
}

/// The `Description` field of the comment, else the name: its first line the synopsis, the rest
/// the extended description, if any, each line after a space and an empty one ` .`.
fn description(cfg: &Config) -> String {
  let text = match cfg.comment.trim() {
    "" => cfg.name.trim(),
    comment => comment,
  };

  let mut lines = text.lines().map(|l| l.trim_end_matches('\r').trim_end());
  let synopsis = lines.next().unwrap_or_default();
  let rest: Vec<_> = lines.collect();
  let first = rest.iter().position(|l| !l.is_empty()).unwrap_or(rest.len());

  let mut out = format!("Description: {synopsis}\n");
  for line in &rest[first..] {
    match line.is_empty() {
      true => out += " .\n",
      false => out += &format!(" {line}\n"),
    }
  }

  out
}

/// `version`, Cargo's, as dpkg orders it: without its `+` build, the `-` before the pre-release
/// as `~`, so it sorts before its release, and each `-` in the pre-release as `.`, which dpkg would
/// take for a revision. A `dev` build gets `~dev` after, so it sorts before the version it builds.
pub fn deb_version(version: &str, dev: bool) -> String {
  let version = version.split('+').next().unwrap_or(version);
  let version = match version.split_once('-') {
    Some((core, pre)) => format!("{core}~{}", pre.replace('-', ".")),
    None => version.to_string(),
  };

  match dev {
    true => version + "~dev",
    false => version,
  }
}

/// Debian's name of the arch `arch`, as Rust names it.
fn deb_arch(arch: &str) -> io::Result<&'static str> {
  match arch {
    "x86_64" => Ok("amd64"),
    "aarch64" => Ok("arm64"),
    _ => Err(bad(format!("the Linux arch is x86_64 or aarch64, not {arch}"))),
  }
}

/// The package's name: `linux.deb-package`, else the binary's in lower case with `_` as `-`, as
/// Debian allows one: `[a-z0-9][a-z0-9+.-]+`.
fn package(cfg: &Config) -> io::Result<String> {
  let name = match cfg.linux.get("deb-package") {
    None => cfg.bin.to_lowercase().replace('_', "-"),
    Some(Value::String(s)) => s.clone(),
    Some(_) => return Err(bad("[package.metadata.shipo.linux] deb-package is not a string")),
  };

  let alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
  let valid = name.len() >= 2
    && name.starts_with(alnum)
    && name.chars().all(|c| alnum(c) || "+-.".contains(c));
  match valid {
    true => Ok(name),
    false => Err(bad(format!(
      "{name:?} is no Debian package name, [a-z0-9][a-z0-9+.-]+: set linux.deb-package"
    ))),
  }
}

/// The `.deb`'s `Name <mail>`: the publisher, and the mail of the first author with one, as
/// Debian wants a maintainer.
fn maintainer(cfg: &Config) -> io::Result<String> {
  let mail = |author: &String| {
    let (_, mail) = author.split_once('<')?;
    let mail = mail.strip_suffix('>')?;
    mail.contains('@').then(|| mail.to_string())
  };

  match cfg.authors.iter().find_map(mail) {
    Some(mail) if !cfg.publisher.is_empty() => Ok(format!("{} <{mail}>", cfg.publisher)),
    _ => Err(bad("the .deb's Maintainer is `Name <mail>`: an author `Name <mail>` in Cargo.toml")),
  }
}

/// The `LICENSE` file in the package's folder, else in its workspace's; `None` if neither has one.
fn license(cfg: &Config) -> Option<PathBuf> {
  [&cfg.root, &cfg.workspace].into_iter().map(|dir| dir.join("LICENSE")).find(|p| p.is_file())
}

// MARK: Files

/// The bytes of the file at `path`, an error naming it.
fn read(path: &Path) -> io::Result<Vec<u8>> {
  fs::read(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
}

/// The time of the packages' files: `$SOURCE_DATE_EPOCH` when set, else the binary `bin`'s, so a
/// build of the same binary packs the same bytes.
fn mtime(bin: &Path) -> io::Result<u64> {
  if let Some(epoch) = std::env::var_os("SOURCE_DATE_EPOCH") {
    let epoch = epoch.to_string_lossy();
    return epoch
      .trim()
      .parse()
      .map_err(|_| bad(format!("SOURCE_DATE_EPOCH is {epoch:?}, not seconds")));
  }

  let time = fs::metadata(bin)
    .and_then(|m| m.modified())
    .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", bin.display())))?;
  Ok(time.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs()))
}

/// The file `out`, made by `make` at `<out>.part` and renamed: on any error the part is removed,
/// so a folder a release job uploads never holds half a file.
fn place(out: &Path, make: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
  let mut name = out.file_name().unwrap_or_default().to_os_string();
  name.push(".part");
  let part = out.with_file_name(name);
  let made = make(&part).and_then(|_| fs::rename(&part, out));
  if made.is_err() {
    let _ = fs::remove_file(&part);
  }

  made
}

#[cfg(test)]
mod tests {
  #[cfg(target_os = "linux")]
  use std::process::Command;
  use std::time::Duration;

  use toml::Table;

  use super::*;
  use crate::tool::Temp;
  #[cfg(target_os = "linux")]
  use crate::tool::run;
  use crate::tree::Node;

  /// A config in `dir` with the `[linux]` table `linux`.
  fn config(dir: &Path, linux: &str) -> Config {
    Config {
      id: "com.example.hello".into(),
      name: "Hello World".into(),
      icon: dir.join("icon.png"),
      comment: "Says hello".into(),
      version: "1.2.0-beta.1".into(),
      bin: "hello".into(),
      macos: Table::new(),
      windows: Table::new(),
      linux: toml::from_str(linux).unwrap(),
      publisher: "Example".into(),
      authors: vec!["Jane Doe <jane@example.com>".into()],
      repository: String::new(),
      root: dir.to_path_buf(),
      workspace: dir.to_path_buf(),
      target: dir.join("target"),
    }
  }

  const LINUX: &str = r#"
categories = ["AudioVideo", "Audio"]
deb-depends = ["libc6 (>= 2.31)"]
deb-recommends = ["libgtk-3-0t64 | libgtk-3-0", "libasound2t64"]
"#;

  #[test]
  fn desktop_entry() {
    let mut cfg = config(Path::new("/app"), LINUX);
    let want = "[Desktop Entry]\nType=Application\nName=Hello World\nComment=Says hello\n\
      Exec=hello\nIcon=com.example.hello\nStartupWMClass=com.example.hello\nStartupNotify=true\n\
      Terminal=false\nCategories=AudioVideo;Audio;\n";
    assert_eq!(desktop(&cfg).unwrap(), want);

    cfg.comment = String::new();
    cfg.linux = Table::new();
    let plain = desktop(&cfg).unwrap();
    assert!(!plain.contains("Comment="), "{plain}");
    assert!(plain.ends_with("Categories=Utility;\n"), "{plain}");

    cfg.name = " Hi\\there\t ".into();
    cfg.comment = "line\nnext\r".into();
    cfg.bin = "my app".into();
    cfg.linux = toml::from_str("categories = [\"X-Odd;One\"]").unwrap();
    let odd = desktop(&cfg).unwrap();
    assert!(odd.contains("\nName=\\sHi\\\\there\\t\\s\n"), "{odd}");
    assert!(odd.contains("\nComment=line\\nnext\\r\n"), "{odd}");
    assert!(odd.contains("\nExec=\"my app\"\n"), "{odd}");
    assert!(odd.ends_with("\nCategories=X-Odd\\;One;\n"), "{odd}");

    cfg.linux = toml::from_str("categories = \"Utility\"").unwrap();
    let e = desktop(&cfg).unwrap_err().to_string();
    assert_eq!(e, "[package.metadata.shipo.linux] categories is not a list of strings");
  }

  #[test]
  fn exec_args() {
    let cases = [
      ("hello", "hello"),
      ("hello-app_2", "hello-app_2"),
      ("100%", "100%%"),
      ("my app", "\"my app\""),
      ("a$b`c\"d", "\"a\\$b\\`c\\\"d\""),
      ("it's", "\"it's\""),
    ];

    for (arg, want) in cases {
      assert_eq!(exec(arg), want, "{arg}");
    }

    // Quoted for Exec, then escaped as a string: a `\` is four.
    assert_eq!(escape(&exec("a\\b")), "\"a\\\\\\\\b\"");
  }

  #[test]
  fn control_file() {
    let cfg = config(Path::new("/app"), LINUX);
    let want = "Package: hello\nVersion: 1.2.0~beta.1~dev\nArchitecture: arm64\n\
      Maintainer: Example <jane@example.com>\nInstalled-Size: 3\nDepends: libc6 (>= 2.31)\n\
      Recommends: libgtk-3-0t64 | libgtk-3-0, libasound2t64\nSection: utils\n\
      Priority: optional\nDescription: Says hello\n";
    assert_eq!(control(&cfg, "hello", "aarch64", true, 3000).unwrap(), want);

    let mut cfg = config(Path::new("/app"), "");
    cfg.comment = String::new();
    cfg.authors = vec!["Joe".into(), "Jane <jane@example.com>".into()];
    let want = "Package: hello\nVersion: 1.2.0~beta.1\nArchitecture: amd64\n\
      Maintainer: Example <jane@example.com>\nInstalled-Size: 1\nSection: utils\n\
      Priority: optional\nDescription: Hello World\n";
    assert_eq!(control(&cfg, "hello", "x86_64", false, 1).unwrap(), want);

    let e = control(&cfg, "hello", "riscv64", false, 1).unwrap_err().to_string();
    assert_eq!(e, "the Linux arch is x86_64 or aarch64, not riscv64");

    cfg.linux = toml::from_str("deb-depends = [\"a\\nb\"]").unwrap();
    let e = control(&cfg, "hello", "x86_64", false, 1).unwrap_err().to_string();
    assert_eq!(e, "the .deb's Depends is one line, not \"a\\nb\"");
    cfg.linux = Table::new();

    for authors in [vec![], vec!["Jane".to_string()], vec!["Jane <jane>".to_string()]] {
      cfg.authors = authors;
      let e = control(&cfg, "hello", "x86_64", false, 1).unwrap_err().to_string();
      assert!(e.starts_with("the .deb's Maintainer is `Name <mail>`"), "{e}");
    }

    cfg.authors = vec!["Jane <jane@example.com>".into()];
    cfg.linux = toml::from_str("deb-depends = [\"libc6\", 1]").unwrap();
    let e = control(&cfg, "hello", "x86_64", false, 1).unwrap_err().to_string();
    assert_eq!(e, "[package.metadata.shipo.linux] deb-depends is not a list of strings");
  }

  #[test]
  fn descriptions() {
    let mut cfg = config(Path::new("/app"), "");
    let cases = [
      ("Says hello", "Description: Says hello\n"),
      ("  Says hello \n\n \n", "Description: Says hello\n"),
      ("", "Description: Hello World\n"),
      (
        "Says hello\n\nTo you.\r\n\n  In two lines.\n\n",
        "Description: Says hello\n To you.\n .\n   In two lines.\n",
      ),
      ("Says hello\nTo you.", "Description: Says hello\n To you.\n"),
    ];

    for (comment, want) in cases {
      cfg.comment = comment.into();
      assert_eq!(description(&cfg), want, "{comment:?}");
    }
  }

  #[test]
  fn package_names() {
    let mut cfg = config(Path::new("/app"), "");
    assert_eq!(package(&cfg).unwrap(), "hello");
    cfg.bin = "Hello_App".into();
    assert_eq!(package(&cfg).unwrap(), "hello-app");
    cfg.linux = toml::from_str("deb-package = \"hello2.0+x\"").unwrap();
    assert_eq!(package(&cfg).unwrap(), "hello2.0+x");

    for name in ["Hello", "h", "-hello", "hello world", "héllo"] {
      cfg.linux = toml::from_str(&format!("deb-package = \"{name}\"")).unwrap();
      let e = package(&cfg).unwrap_err().to_string();
      assert!(e.contains("is no Debian package name"), "{name}: {e}");
    }

    cfg.linux = toml::from_str("deb-package = 1").unwrap();
    let e = package(&cfg).unwrap_err().to_string();
    assert_eq!(e, "[package.metadata.shipo.linux] deb-package is not a string");
  }

  /// Cargo's versions, a dev build's or not, each before the next as dpkg orders them.
  const ORDERED: [(&str, bool, &str); 13] = [
    ("1.1.9", false, "1.1.9"),
    ("1.2.0-beta.2", true, "1.2.0~beta.2~dev"),
    ("1.2.0-beta.2", false, "1.2.0~beta.2"),
    ("1.2.0-beta.10", false, "1.2.0~beta.10"),
    ("1.2.0-rc.1", false, "1.2.0~rc.1"),
    ("1.2.0-rc.2+build-5", false, "1.2.0~rc.2"),
    ("1.2.0-rc-3", false, "1.2.0~rc.3"),
    ("1.2.0-rc.4-x+y-z", false, "1.2.0~rc.4.x"),
    ("1.2.0", false, "1.2.0"),
    ("1.2.1+build-5", true, "1.2.1~dev"),
    ("1.2.1+build-5", false, "1.2.1"),
    ("1.10.0-alpha", false, "1.10.0~alpha"),
    ("1.10.0", false, "1.10.0"),
  ];

  #[test]
  fn deb_versions() {
    for (version, dev, want) in ORDERED {
      assert_eq!(deb_version(version, dev), want, "{version} {dev}");
    }

    assert_eq!(deb_version("1.2.0-rc.1+build-5", true), "1.2.0~rc.1~dev");
  }

  /// [ORDERED] as dpkg orders them, each dev build before the version it builds.
  #[cfg(target_os = "linux")]
  #[test]
  fn dpkg_orders() {
    let mut pairs: Vec<_> = ORDERED.windows(2).map(|w| (w[0].2, w[1].2)).collect();
    pairs.extend([("1.2.0~dev", "1.2.0"), ("1.2.0~rc.1~dev", "1.2.0~rc.1")]);
    // A `-` left in the pre-release would be a revision: `1.2.0~rc-3` is `1.2.0~rc`, revision 3.
    pairs.push(("1.2.0~rc-3", "1.2.0~rc.3"));
    for (a, b) in pairs {
      run(Command::new("dpkg").args(["--compare-versions", a, "lt", b])).unwrap();
    }
  }

  #[test]
  fn appdir_layout() {
    let cfg = config(Path::new("/app"), LINUX);
    let tree = appdir(&cfg, b"exe".to_vec(), b"png".to_vec()).unwrap();
    let files: Vec<_> = tree
      .iter()
      .map(|(path, mode, node)| match node {
        Node::Link(to) => format!("{path} -> {to}"),
        _ => format!("{path} {mode:o}"),
      })
      .collect();
    let want = [
      ".DirIcon -> com.example.hello.png",
      "AppRun -> usr/bin/hello",
      "com.example.hello.desktop 644",
      "com.example.hello.png 644",
      "usr 755",
      "usr/bin 755",
      "usr/bin/hello 755",
    ];
    assert_eq!(files, want);
    assert_eq!(tree.get("usr/bin/hello"), Some((0o755, &Node::File(b"exe".to_vec()))));
    assert_eq!(tree.get("com.example.hello.png"), Some((0o644, &Node::File(b"png".to_vec()))));
    let entry = Node::File(desktop(&cfg).unwrap().into_bytes());
    assert_eq!(tree.get("com.example.hello.desktop"), Some((0o644, &entry)));
  }

  /// The binary's time, unless `$SOURCE_DATE_EPOCH` gives one.
  #[test]
  fn times() {
    if std::env::var_os("SOURCE_DATE_EPOCH").is_some() {
      return;
    }

    let tmp = Temp::new().unwrap();
    let bin = tmp.join("bin");
    let file = File::create(&bin).unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)).unwrap();
    assert_eq!(mtime(&bin).unwrap(), 1_700_000_000);
    let e = mtime(&tmp.join("none")).unwrap_err().to_string();
    assert!(e.starts_with(&format!("{}: ", tmp.join("none").display())), "{e}");
  }

  #[test]
  fn icon_sizes() {
    let tmp = Temp::new().unwrap();
    let cfg = config(&tmp, "");
    // A 1024 px icon with a margin of 112 px, as macOS keeps: cropped to its tile, then scaled.
    let px = (0..1024 * 1024)
      .flat_map(|i| {
        let (x, y) = (i % 1024, i / 1024);
        let inside = (112..912).contains(&x) && (112..912).contains(&y);
        [200, 100, 50, if inside { 255 } else { 0 }]
      })
      .collect();
    fs::write(&cfg.icon, Image { w: 1024, h: 1024, px }.encode().unwrap()).unwrap();
    let pngs = pngs(&cfg, &HICOLOR).unwrap();
    assert_eq!(pngs.keys().copied().collect::<Vec<_>>(), HICOLOR);
    for (n, png) in pngs {
      let png = Image::decode(&png).unwrap();
      assert_eq!((png.w, png.h), (n, n));
      assert!(png.px.as_chunks::<4>().0.iter().all(|p| *p == [200, 100, 50, 255]), "{n}");
    }
  }

  #[test]
  fn placed() {
    let tmp = Temp::new().unwrap();
    let out = tmp.join("App.deb");
    let listed = || {
      let mut names: Vec<_> = fs::read_dir(&*tmp)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
      names.sort();
      names
    };

    // Half written, then failed: no part is left, nor the file.
    let e = place(&out, |part| {
      fs::write(part, "half")?;
      Err(io::Error::other("disk full"))
    });
    assert_eq!(e.unwrap_err().to_string(), "disk full");
    assert!(listed().is_empty());

    place(&out, |part| fs::write(part, "whole")).unwrap();
    assert_eq!(listed(), ["App.deb"]);
    assert_eq!(fs::read_to_string(&out).unwrap(), "whole");

    // A failed one leaves the one before as it was.
    assert!(place(&out, |part| fs::write(part, "half").and(Err(io::Error::other("no")))).is_err());
    assert_eq!(listed(), ["App.deb"]);
    assert_eq!(fs::read_to_string(&out).unwrap(), "whole");
  }

  #[test]
  fn licenses() {
    let tmp = Temp::new().unwrap();
    let mut cfg = config(&tmp.join("ws/app"), "");
    cfg.workspace = tmp.join("ws");
    fs::create_dir_all(&cfg.root).unwrap();
    fs::write(tmp.join("LICENSE"), "outside").unwrap();
    assert_eq!(license(&cfg), None);

    fs::write(cfg.workspace.join("LICENSE"), "workspace").unwrap();
    assert_eq!(license(&cfg), Some(cfg.workspace.join("LICENSE")));
    fs::write(cfg.root.join("LICENSE"), "own").unwrap();
    assert_eq!(license(&cfg), Some(cfg.root.join("LICENSE")));
  }

  /// The lines of `tar -tv`-like `text` (`dpkg-deb --contents`, `unsquashfs -ll`) as
  /// `<mode> <path>[ -> <link>]`, sorted; each owned by root, else it panics.
  #[cfg(target_os = "linux")]
  fn listing(text: &str) -> Vec<String> {
    let lines = text.lines().filter(|l| !l.is_empty() && !l.starts_with("Parallel unsquashfs"));
    let lines = lines.map(|line| {
      let parts: Vec<_> = line.split_whitespace().collect();
      assert_eq!(parts.get(1), Some(&"root/root"), "{line}");
      format!("{} {}", parts[0], parts[5..].join(" "))
    });
    sorted(lines.collect())
  }

  #[cfg(target_os = "linux")]
  fn sorted(mut lines: Vec<String>) -> Vec<String> {
    lines.sort();
    lines
  }

  /// Both packages of a binary, read back by the system's tools: `unsquashfs` and the runtime,
  /// `dpkg-deb` and `desktop-file-validate`. The binary is `$SHIPO_TEST_ELF`, else a script, and
  /// prints `hello 1.2.0-beta.1`; the packages stay in `$SHIPO_TEST_OUT` when it is set.
  #[cfg(target_os = "linux")]
  #[test]
  #[ignore = "fetches the AppImage runtime from GitHub; needs squashfs-tools, desktop-file-utils"]
  fn packages() {
    use std::env;

    let tmp = Temp::new().unwrap();
    let mut cfg = config(&tmp, LINUX);
    // The target folder of this test's `<target>/debug/deps/<test>`, where the runtime stays.
    cfg.target = env::current_exe().unwrap().ancestors().nth(3).unwrap().to_path_buf();
    let px = (0..1024 * 1024).flat_map(|i| [(i % 256) as u8, (i / 4096) as u8, 128, 255]).collect();
    fs::write(&cfg.icon, Image { w: 1024, h: 1024, px }.encode().unwrap()).unwrap();
    fs::write(tmp.join("LICENSE"), "MIT").unwrap();
    let bin = env::var_os("SHIPO_TEST_ELF").map_or_else(
      || {
        let path = tmp.join("hello");
        fs::write(&path, "#!/bin/sh\necho hello 1.2.0-beta.1\n").unwrap();
        path
      },
      PathBuf::from,
    );

    let out = env::var_os("SHIPO_TEST_OUT").map_or_else(|| tmp.join("out"), PathBuf::from);
    fs::create_dir_all(&out).unwrap();
    let arch = env::consts::ARCH;
    let text = |cmd: &mut Command| String::from_utf8(run(cmd).unwrap()).unwrap();

    // Both made first, so a host that can't run the AppImage still checks the .deb: an emulator's
    // binfmt entry refuses the AppImage's magic bytes in the ELF header.
    let image = appimage(&cfg, &bin, arch, &out).unwrap();
    let deb = deb(&cfg, &bin, arch, true, &out).unwrap();
    assert_eq!(deb, out.join(format!("Hello World-dev-linux-{arch}.deb")));
    let info = text(Command::new("dpkg-deb").arg("--info").arg(&deb));
    let control = control(&cfg, "hello", arch, true, 0).unwrap();
    for line in control.lines().filter(|l| !l.starts_with("Installed-Size")) {
      assert!(info.contains(&format!("\n {line}\n")), "{line}: {info}");
    }

    let mut want: Vec<String> = [
      "drwxr-xr-x ./",
      "drwxr-xr-x ./usr/",
      "drwxr-xr-x ./usr/bin/",
      "-rwxr-xr-x ./usr/bin/hello",
      "drwxr-xr-x ./usr/share/",
      "drwxr-xr-x ./usr/share/applications/",
      "-rw-r--r-- ./usr/share/applications/com.example.hello.desktop",
      "drwxr-xr-x ./usr/share/doc/",
      "drwxr-xr-x ./usr/share/doc/hello/",
      "-rw-r--r-- ./usr/share/doc/hello/copyright",
      "drwxr-xr-x ./usr/share/icons/",
      "drwxr-xr-x ./usr/share/icons/hicolor/",
    ]
    .map(String::from)
    .to_vec();
    for n in HICOLOR {
      let dir = format!("./usr/share/icons/hicolor/{n}x{n}");
      want.push(format!("drwxr-xr-x {dir}/"));
      want.push(format!("drwxr-xr-x {dir}/apps/"));
      want.push(format!("-rw-r--r-- {dir}/apps/com.example.hello.png"));
    }

    let contents = text(Command::new("dpkg-deb").arg("--contents").arg(&deb));
    assert_eq!(listing(&contents), sorted(want));

    let files = tmp.join("deb");
    run(Command::new("dpkg-deb").arg("-x").arg(&deb).arg(&files)).unwrap();
    let entry = files.join("usr/share/applications/com.example.hello.desktop");
    assert_eq!(text(Command::new("desktop-file-validate").arg(&entry)), "");
    for n in HICOLOR {
      let png = files.join(format!("usr/share/icons/hicolor/{n}x{n}/apps/com.example.hello.png"));
      assert_eq!(Image::read(&png).map(|p| (p.w, p.h)).unwrap(), (n, n));
    }

    assert_eq!(image, out.join(format!("Hello World-dev-linux-{arch}.AppImage")));
    let offset = text(Command::new(&image).arg("--appimage-offset"));
    let runtime = cfg.target.join(format!("tools/runtime-{RUNTIME}-{arch}"));
    assert_eq!(offset.trim(), fs::metadata(&runtime).unwrap().len().to_string());
    let list = text(Command::new("unsquashfs").args(["-ll", "-o", offset.trim()]).arg(&image));
    let want = [
      "drwxr-xr-x squashfs-root",
      "lrwxrwxrwx squashfs-root/.DirIcon -> com.example.hello.png",
      "lrwxrwxrwx squashfs-root/AppRun -> usr/bin/hello",
      "-rw-r--r-- squashfs-root/com.example.hello.desktop",
      "-rw-r--r-- squashfs-root/com.example.hello.png",
      "drwxr-xr-x squashfs-root/usr",
      "drwxr-xr-x squashfs-root/usr/bin",
      "-rwxr-xr-x squashfs-root/usr/bin/hello",
    ];
    assert_eq!(listing(&list), sorted(want.map(String::from).to_vec()));

    let said = text(Command::new(&image).arg("--appimage-extract-and-run"));
    assert_eq!(said, "hello 1.2.0-beta.1\n");
  }
}
