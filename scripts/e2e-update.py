# Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
# /// script
# requires-python = ">=3.11"
# ///
"""The end-to-end update: examples/update.rs built at 1.0.0 and 2.0.0, packed by shipo, 1.0.0
installed as a user installs it, then updated in place from 2.0.0's feed served on loopback.

uv run scripts/e2e-update.py build <dir>   the example at both versions, <dir>/<version>/<triple>/hello[.exe]
uv run scripts/e2e-update.py pack <dir>    `shipo release` of each version, a throwaway key, 2.0.0's feed,
                                           and <dir>/SHA256SUMS of what it packed
uv run scripts/e2e-update.py run <dir>     1.0.0 installed and updated; checks that 2.0.0 is what runs
  [--v1 <setup>]                           Windows: 1.0.0 installed by this setup (an NSIS one), not <dir>'s
uv run scripts/e2e-update.py all <dir>     the three on this host (`make e2e`)

`pack` packs the binaries `build` left in <dir> for any OS, so any host packs every OS's, the same
bytes on each: the files' times are the commit's ($SOURCE_DATE_EPOCH) and the key is fixed. Only a
DMG differs, as it updates only when packed on a Mac (signed by "Hello Local Signing", which v1 and
v2 must share; elsewhere it is signed ad hoc), and with it the feed. `run` takes this OS's files and leaves the app installed: in a
temporary folder on macOS and Linux; on Windows for this user, then uninstalled by its setup's
uninstaller, which must leave nothing; before that, with the app running, its setup and uninstaller
must each exit 2 and change nothing. CI runs the steps on each OS (.github/workflows/main.yml).
"""

import argparse
import functools
import hashlib
import http.server
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERSIONS = ("1.0.0", "2.0.0")
NAME, BIN, ID = "Hello", "hello", "dev.shipo.hello"
PORT = 18765
URL = f"http://127.0.0.1:{PORT}"
EXE = ".exe" if sys.platform == "win32" else ""
WAIT = 120  # seconds for the update to land

MANIFEST = """[package]
name = "{bin}"
version = "{version}"
edition = "2024"
authors = ["shipo e2e <e2e@example.com>"]
repository = "https://github.com/vladkens/shipo"

[package.metadata.shipo]
id = "{id}"
name = "{name}"
icon = "../icon.png"
comment = "shipo's end-to-end update"
"""


def log(msg: str) -> None:
  print(f"e2e: {msg}", flush=True)


def run(*args, **kw) -> str:
  """Runs a command to its end, its stdout given back; its stderr passes through."""
  log(" ".join(str(a) for a in args))
  return subprocess.run([str(a) for a in args], check=True, stdout=subprocess.PIPE, text=True, **kw).stdout


def target_dir() -> Path:
  return Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))


def build(out: Path) -> None:
  """The example at each version, as `release` takes binaries: <out>/<version>/<triple>/hello."""
  host = next(ln.split()[1] for ln in run("rustc", "-vV").splitlines() if ln.startswith("host:"))
  for version in VERSIONS:
    env = os.environ | {"UPDATE_VERSION": version}
    cmd = ["cargo", "build", "--release", "--example", "update", "--no-default-features"]
    run(*cmd, cwd=ROOT, env=env)
    dest = out / version / host / f"{BIN}{EXE}"
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(target_dir() / "release/examples" / f"update{EXE}", dest)
    log(f"built {dest}")


def png(n: int) -> bytes:
  """A square opaque PNG, n×n, of one colour."""

  def chunk(kind: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

  raw = b"".join(b"\x00" + bytes([40, 120, 200, 255]) * n for _ in range(n))
  head = struct.pack(">IIBBBBB", n, n, 8, 6, 0, 0, 0)
  return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", head) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


def keychain(out: Path) -> None:
  """CI on a Mac: "Hello Local Signing", the identity shipo signs a dev build by, in a keychain of
  its own that codesign may use without a prompt. shipo would make it in the login keychain, where
  codesign's first use asks for the password: a prompt nobody answers on a runner."""
  kc, pw, name = out / "e2e.keychain-db", "e2e", f"{NAME} Local Signing"
  key, cert, p12 = out / "signing.key", out / "signing.pem", out / "signing.p12"
  openssl = "/usr/bin/openssl"  # LibreSSL, whose PKCS#12 `security import` reads
  ext = ["-addext", "keyUsage=critical,digitalSignature", "-addext", "extendedKeyUsage=critical,codeSigning"]
  run(openssl, "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", f"/CN={name}", *ext, "-keyout", key, "-out", cert)
  run(openssl, "pkcs12", "-export", "-inkey", key, "-in", cert, "-out", p12, "-passout", f"pass:{pw}")
  run("security", "create-keychain", "-p", pw, kc)
  run("security", "set-keychain-settings", kc)
  run("security", "unlock-keychain", "-p", pw, kc)
  run("security", "import", p12, "-k", kc, "-P", pw, "-T", "/usr/bin/codesign")
  run("security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:", "-s", "-k", pw, kc)
  listed = [ln.strip().strip('"') for ln in run("security", "list-keychains", "-d", "user").splitlines()]
  run("security", "list-keychains", "-d", "user", "-s", kc, *listed)


def pack(out: Path) -> None:
  """Each version's installers, a throwaway key (key.pub its public half) and 2.0.0's feed,
  out/update.txt, of the files served at http://127.0.0.1:<port>/<version>/."""
  run("cargo", "build", "--release", "--bin", "shipo", cwd=ROOT)
  shipo = target_dir() / "release" / f"shipo{EXE}"
  # The tools shipo fetches (rcodesign, the AppImage runtime) once for both versions.
  env = os.environ | {"CARGO_TARGET_DIR": str(out / "target")}
  env.setdefault("SOURCE_DATE_EPOCH", run("git", "log", "-1", "--format=%ct", cwd=ROOT).strip())
  (out / "icon.png").write_bytes(png(1024))

  key = out / "update.key"
  key.write_text(hashlib.sha256(b"shipo e2e").hexdigest() + "\n")
  (out / "key.pub").write_text(run(shipo, "keygen", "--public", key, env=env).strip())

  if sys.platform == "darwin" and os.environ.get("CI"):
    keychain(out)

  for version in VERSIONS:
    manifest = out / version / "Cargo.toml"
    manifest.write_text(MANIFEST.format(bin=BIN, version=version, id=ID, name=NAME))
    # LF, as git checks it out on Windows with CRLF: the setup and the .deb hold it.
    (out / version / "LICENSE").write_bytes((ROOT / "LICENSE").read_bytes().replace(b"\r\n", b"\n"))
    print(run(shipo, "release", out / version, "--manifest", manifest, env=env), end="")

  newest = out / VERSIONS[-1]
  files = sorted(f for f in newest.glob(f"{NAME}-dev-*") if f.suffix != ".deb")
  url = f"{URL}/{{version}}"
  feed = run(shipo, "feed", "--manifest", newest / "Cargo.toml", "--url", url, "--key", key, *files, env=env)
  shutil.copy2(feed.strip(), out / "update.txt")
  print((out / "update.txt").read_text(), end="")

  packed = sorted(f.relative_to(out).as_posix() for f in out.glob(f"*/{NAME}-dev-*")) + ["update.txt"]
  sums = "".join(f"{hashlib.sha256((out / f).read_bytes()).hexdigest()}  {f}\n" for f in packed)
  (out / "SHA256SUMS").write_text(sums, newline="\n")
  print(sums, end="")


def serve(root: Path) -> None:
  """Serves `root` on loopback in a thread for the rest of the run."""
  handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(root))
  server = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), handler)
  threading.Thread(target=server.serve_forever, daemon=True).start()


def one(folder: Path, pattern: str) -> Path:
  found = sorted(folder.glob(pattern))
  if len(found) != 1:
    sys.exit(f"e2e: {folder} holds {len(found)} of {pattern}, not one")

  return found[0]


def install(packed: Path, home: Path, v1_setup: Path | None = None) -> Path:
  """1.0.0 installed the way a user does it, on Windows by `v1_setup` if given; its executable."""
  v1 = packed / VERSIONS[0]
  if sys.platform == "darwin":
    mount = home / "volume"
    run("hdiutil", "attach", "-nobrowse", "-readonly", "-mountpoint", mount, one(v1, "*.dmg"))
    apps = home / "Applications"
    apps.mkdir()
    run("ditto", mount / f"{NAME}.app", apps / f"{NAME}.app")
    run("hdiutil", "detach", mount)
    return apps / f"{NAME}.app/Contents/MacOS" / BIN

  if sys.platform == "win32":
    run(v1_setup or one(v1, "*-setup.exe"), "/S")
    return Path(os.environ["LOCALAPPDATA"]) / "Programs" / NAME / f"{NAME}.exe"

  image = home / f"{NAME}.AppImage"
  shutil.copy2(one(v1, "*.AppImage"), image)
  image.chmod(0o755)
  return image


UNINSTALL = r"Software\Microsoft\Windows\CurrentVersion\Uninstall"
RUN = r"Software\Microsoft\Windows\CurrentVersion\Run"


def uninstall_keys() -> list[dict]:
  """Windows: the values of each uninstall key whose DisplayName is the app's."""
  import winreg

  found = []
  with winreg.OpenKey(winreg.HKEY_CURRENT_USER, UNINSTALL) as keys:
    for i in range(winreg.QueryInfoKey(keys)[0]):
      with winreg.OpenKey(keys, winreg.EnumKey(keys, i)) as key:
        values = {}
        for j in range(winreg.QueryInfoKey(key)[1]):
          name, value, _ = winreg.EnumValue(key, j)
          values[name] = value

        if values.get("DisplayName") == NAME:
          found.append(values)

  return found


def uninstall_key() -> dict | None:
  """Windows: the values of the app's uninstall key."""
  return next(iter(uninstall_keys()), None)


def registered() -> str | None:
  """Windows: the installed version the setup wrote last, its uninstall key's DisplayVersion."""
  return (uninstall_key() or {}).get("DisplayVersion")


def lnk_path() -> Path:
  return Path(os.environ["APPDATA"]) / r"Microsoft\Windows\Start Menu\Programs" / f"{NAME}.lnk"


def state(folder: Path):
  """Windows: what a setup or uninstaller changes: the folder's files, the key, the shortcut; not
  the updater's staging folder, which the app clears as it starts."""
  files = folder.iterdir()
  files = sorted((p.name, p.stat().st_size, p.stat().st_mtime_ns) for p in files if not p.name.startswith(".shipo-update-"))
  lnk = lnk_path()
  return files, uninstall_keys(), lnk.is_file() and lnk.stat().st_mtime_ns


def in_use(packed: Path, exe: Path) -> None:
  """Windows: with the app running, its setup and its uninstaller each exit 2 and change nothing."""
  # A feed that never answers holds the app in its check.
  hold = socket.create_server(("127.0.0.1", 0))
  key = (packed / "key.pub").read_text().strip()
  app = subprocess.Popen([exe, "--check", f"http://127.0.0.1:{hold.getsockname()[1]}/update.txt", key])
  try:
    time.sleep(2)
    if app.poll() is not None:
      sys.exit(f"e2e: the app held by a silent feed ended: {app.returncode}")

    before = state(exe.parent)
    for cmd in (one(packed / VERSIONS[1], "*-setup.exe"), exe.parent / "Uninstall.exe"):
      code = subprocess.run([cmd, "/S"], timeout=60).returncode
      if code != 2 or (after := state(exe.parent)) != before:
        sys.exit(f"e2e: {cmd.name} /S with the app running: {code}, {before} -> {after}")

    log("with the app running, the setup and the uninstaller exit 2, nothing changed")
  finally:
    app.kill()
    app.wait()
    hold.close()


def uninstall(exe: Path) -> None:
  """Windows: the install as spec.md has it, then removed by its QuietUninstallString: the folder,
  the key, the Start menu entry and the launch at login all gone."""
  import winreg

  key = uninstall_key()
  folder = exe.parent
  lnk = lnk_path()
  want = {
    "InstallLocation": str(folder),
    "DisplayIcon": str(exe),
    "QuietUninstallString": f'"{folder / "Uninstall.exe"}" /S',
    "NoModify": 1,
    "NoRepair": 1,
  }

  wrong = {k: (key or {}).get(k) for k, v in want.items() if (key or {}).get(k) != v}
  if wrong or not lnk.is_file() or not (folder / "LICENSE.txt").is_file():
    sys.exit(f"e2e: the install is not spec.md's: {wrong}, {lnk} {lnk.is_file()}")

  # Our key alone, whole, and our files alone, whatever installed 1.0.0 (an NSIS setup).
  names = {*want, "DisplayName", "DisplayVersion", "Publisher", "UninstallString", "EstimatedSize"}
  files = sorted(p.name for p in folder.iterdir() if not p.name.startswith(".shipo-update-"))
  ours = b"shipo-setup-v1\0\0" in (folder / "Uninstall.exe").read_bytes()
  keys = len(uninstall_keys())
  if keys != 1 or set(key) != names or files != ["Hello.exe", "LICENSE.txt", "Uninstall.exe"] or not ours:
    sys.exit(f"e2e: not ours alone: {keys} keys, {sorted(key)}, {files}, our uninstaller {ours}")

  log(f"installed as spec.md has it: {sorted(key)}")
  with winreg.CreateKey(winreg.HKEY_CURRENT_USER, RUN) as run_key:
    winreg.SetValueEx(run_key, ID, 0, winreg.REG_SZ, f'"{exe}"')

  log(key["QuietUninstallString"])
  subprocess.run(key["QuietUninstallString"], check=True, timeout=60)
  with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN) as run_key:
    try:
      winreg.QueryValueEx(run_key, ID)
      left = True
    except OSError:
      left = False

  if folder.exists() or uninstall_key() or lnk.exists() or left:
    listed = sorted(p.name for p in folder.iterdir()) if folder.exists() else None
    sys.exit(f"e2e: left after uninstalling: {listed}, key {uninstall_key()}, {lnk.exists()}, {left}")

  log("uninstalled, nothing left")


def stamp(exe: Path):
  """What changes when the update lands. Windows: the setup's last write, as reading the exe
  could hold it open against the setup; elsewhere the file, swapped or renamed in one step."""
  if sys.platform == "win32":
    return registered()

  st = exe.stat()
  return (st.st_ino, st.st_size, st.st_mtime_ns)


def version(exe: Path) -> str:
  return run(exe, timeout=30).strip()


def check(packed: Path, v1_setup: Path | None = None) -> None:
  """Installs 1.0.0, has it update itself from the served feed, and waits for 2.0.0 to run."""
  key = (packed / "key.pub").read_text().strip()
  serve(packed)
  home = Path(tempfile.mkdtemp(prefix="shipo-e2e-", dir=os.environ.get("RUNNER_TEMP")))
  exe = install(packed, home, v1_setup)
  if (v := version(exe)) != VERSIONS[0]:
    sys.exit(f"e2e: the installed app is {v}, not {VERSIONS[0]}")

  before = stamp(exe)
  out = run(exe, "--update", f"{URL}/update.txt", key, timeout=WAIT)
  print(out, end="")
  if f"installed {VERSIONS[1]}" not in out:
    sys.exit("e2e: the app did not install the update")

  deadline = time.monotonic() + WAIT
  while stamp(exe) == before:
    if time.monotonic() > deadline:
      sys.exit(f"e2e: {exe} unchanged after {WAIT} s")

    time.sleep(1)

  log(f"{exe} replaced")
  while (v := version(exe)) != VERSIONS[1]:
    if time.monotonic() > deadline:
      sys.exit(f"e2e: the installed app is {v}, not {VERSIONS[1]}")

    time.sleep(2)

  log(f"the installed app is {v}")
  # The installer removes its staging folder and the old copy as it ends (Windows': the next run).
  if sys.platform != "win32":
    folder = exe.parent.parent.parent.parent if sys.platform == "darwin" else exe.parent
    while left := [p.name for p in folder.iterdir() if ".shipo-" in p.name]:
      if time.monotonic() > deadline:
        sys.exit(f"e2e: left in {folder}: {left}")

      time.sleep(1)

  log("updated in place")
  if sys.platform == "win32":
    in_use(packed, exe)
    uninstall(exe)


def main() -> None:
  ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
  ap.add_argument("step", choices=("build", "pack", "run", "all"))
  ap.add_argument("dir", type=Path)
  ap.add_argument("--v1", type=Path, help="run on Windows: the setup that installs 1.0.0")
  args = ap.parse_args()
  out = args.dir.resolve()
  out.mkdir(parents=True, exist_ok=True)
  steps = {"build": build, "pack": pack, "run": lambda out: check(out, args.v1 and args.v1.resolve())}
  for step in steps if args.step == "all" else [args.step]:
    steps[step](out)


if __name__ == "__main__":
  main()
