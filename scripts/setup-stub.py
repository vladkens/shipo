# Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
# /// script
# requires-python = ">=3.11"
# ///
"""The Windows setup stub (setup/) built reproducibly for x64 and arm64, the bytes the packer embeds.

uv run scripts/setup-stub.py           build both into stubs/ with their SHA256SUMS (`make setup`)
uv run scripts/setup-stub.py --check   build both and fail unless they match stubs/SHA256SUMS

Any Mac makes the same bytes, wherever the checkout and the cargo home are: setup/rust-toolchain.toml
pins rustc; VSMAN pins Visual Studio's package manifest, which names each CRT and SDK package by URL
and SHA-256, so xwin (XWIN) unpacks the same import libraries for CRT and SDK below even after
Microsoft republishes a version; and the linker is that rustc's own rust-lld, as lld-link. The paths
a host would leave in the exe are remapped (the cargo home's sources, the toolchain's rust-src) or
dropped (panic locations), and /Brepro puts a hash of the exe where the linker's timestamp was. A
Windows or Linux host's rustc, the same version, lays the functions out in another order, so the
stubs are built and checked on a Mac alone.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SETUP = ROOT / "setup"
STUBS = ROOT / "stubs"
SUMS = STUBS / "SHA256SUMS"
TARGETS = {"x64": "x86_64-pc-windows-msvc", "arm64": "aarch64-pc-windows-msvc"}
CRT, SDK, XWIN = "14.44.17.14", "10.0.26100", "0.10.0"

# Visual Studio 17.14.44's, from its channel manifest (a release's manifests stay at their URLs;
# https://aka.ms/vs/17/release/channel names the latest). The hash in the URL is never the one of
# the bytes served.
VSMAN = (
  "https://download.visualstudio.microsoft.com/download/pr/6bd63d7f-1818-4e03-a552-9feb11496c19/"
  "4cc372d79f02e331c999a926b833a49eb74b30357d30f41ba9cffa1dc0369379/VisualStudio.vsman"
)
VSMAN_SHA256 = "963760b01f621ed4fa40d1cf97aeb042f7bc6e04e3176c13c38abb19d265f789"


def out(*cmd: str) -> str:
  return subprocess.run(cmd, cwd=SETUP, check=True, capture_output=True, text=True).stdout.strip()


def libs() -> Path:
  """The CRT and SDK splatted by xwin from VSMAN for both targets, once per pin."""
  root = SETUP / "target" / "xwin"
  splat = root / "splat"
  pin = f"{XWIN} {VSMAN_SHA256} {CRT} {SDK}\n"
  if (splat / "PIN").is_file() and (splat / "PIN").read_text() == pin:
    return splat

  subprocess.run(["cargo", "install", "--locked", "--root", root, f"xwin@{XWIN}"], cwd=SETUP, check=True)

  # xwin takes a channel manifest, this one naming VSMAN alone, and reads the package manifest from
  # its cache when there, so it unpacks from these verified bytes.
  vsman = root / "dl" / f"pkg_manifest_{VSMAN_SHA256}.vsman"
  if not vsman.is_file() or hashlib.sha256(vsman.read_bytes()).hexdigest() != VSMAN_SHA256:
    with urllib.request.urlopen(VSMAN) as rs:
      data = rs.read()
    if hashlib.sha256(data).hexdigest() != VSMAN_SHA256:
      sys.exit(f"{VSMAN}: SHA-256 is not VSMAN_SHA256")
    vsman.parent.mkdir(parents=True, exist_ok=True)
    vsman.write_bytes(data)

  pld = {"fileName": "VisualStudio.vsman", "sha256": VSMAN_SHA256, "size": vsman.stat().st_size, "url": VSMAN}
  item = {"id": "Microsoft.VisualStudio.Manifests.VisualStudio", "version": "17.14.44", "type": "Manifest"}
  channel = root / "channel.json"
  channel.write_text(json.dumps({"channelItems": [{**item, "payloads": [pld]}]}))

  arches = ",".join(x.split("-")[0] for x in TARGETS.values())
  cmd = [root / "bin" / "xwin", "--accept-license", "--cache-dir", root, "--manifest", channel]
  cmd += ["--crt-version", CRT, "--sdk-version", SDK, "--arch", arches, "splat", "--output", splat]
  subprocess.run(cmd, check=True)
  shutil.rmtree(root / "dl")
  shutil.rmtree(root / "unpack")
  (splat / "PIN").write_text(pin)
  return splat


def build(dest: Path) -> dict[str, str]:
  """Both stubs built into `dest` as shipo-setup-<arch>.exe; their SHA-256 by file name."""
  # Run in setup/, so rustup takes setup/rust-toolchain.toml.
  subprocess.run(["rustup", "toolchain", "install"], cwd=SETUP, check=True)
  sysroot = Path(out("rustc", "--print", "sysroot"))
  commit = next(x.split()[1] for x in out("rustc", "-vV").splitlines() if x.startswith("commit-hash"))
  cargo_home = Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")
  splat = libs()

  flags = [
    "-Ctarget-feature=+crt-static",
    "-Zlocation-detail=none",
    f"--remap-path-prefix={cargo_home}=/cargo",
    f"--remap-path-prefix={sysroot / 'lib' / 'rustlib' / 'src' / 'rust'}=/rustc/{commit}",
    f"--remap-path-prefix={SETUP}=/setup",
    "-Clinker=rust-lld",
    "-Clink-arg=/Brepro",
    "-Clink-arg=/DEBUG:NONE",
    # The static UCRT even for a library that names its DLL's, as cargo-xwin links it.
    "-Clink-arg=-nodefaultlib:ucrt",
    "-Clink-arg=-defaultlib:libucrt",
  ]

  # Each target's own libraries, in config so a path may hold spaces; RUSTFLAGS would override it.
  env = {k: v for k, v in os.environ.items() if k not in ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS")}
  cmd = ["cargo", "build", "--release", "--locked"]
  for target in TARGETS.values():
    arch = target.split("-")[0]
    rustflags = [*flags, *(f"-Lnative={splat / x / arch}" for x in ("crt/lib", "sdk/lib/um", "sdk/lib/ucrt"))]
    cmd += ["--target", target, "--config", f"target.{target}.rustflags={json.dumps(rustflags)}"]
  subprocess.run(cmd, cwd=SETUP, env=env, check=True)

  dest.mkdir(parents=True, exist_ok=True)
  sums = {}
  for arch, target in TARGETS.items():
    name = f"shipo-setup-{arch}.exe"
    shutil.copyfile(SETUP / "target" / target / "release" / "shipo-setup.exe", dest / name)
    sums[name] = hashlib.sha256((dest / name).read_bytes()).hexdigest()

  return sums


def sums_text(sums: dict[str, str]) -> str:
  return "".join(f"{sha}  {name}\n" for name, sha in sums.items())


def main() -> None:
  ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
  ap.add_argument("--check", action="store_true", help="rebuild and compare with stubs/SHA256SUMS")
  args = ap.parse_args()
  if sys.platform != "darwin":
    sys.exit("make setup runs on a Mac: a Windows or Linux host builds the stubs in other bytes")

  if not args.check:
    sums = build(STUBS)
    SUMS.write_text(sums_text(sums), newline="\n")
    print(sums_text(sums), end="")
    return

  # The listed sums and the files include_bytes! embeds, each against a build from source.
  built = build(SETUP / "target" / "bin")
  committed = SUMS.read_text()
  embedded = {name: hashlib.sha256((STUBS / name).read_bytes()).hexdigest() for name in built}
  print(f"stubs/SHA256SUMS:\n{committed}stubs/*.exe:\n{sums_text(embedded)}", end="")
  print(f"built from source:\n{sums_text(built)}", end="")
  if committed != sums_text(built) or embedded != built:
    sys.exit("stubs/ differs from the stubs built from source: run `make setup` and commit all three")


if __name__ == "__main__":
  main()
