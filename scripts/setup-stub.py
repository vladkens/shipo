# Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
# /// script
# requires-python = ">=3.11"
# ///
"""The Windows setup stub (setup/) built reproducibly for x64 and arm64, the bytes the packer embeds.

uv run scripts/setup-stub.py           build both into stubs/ with their SHA256SUMS (`make setup`)
uv run scripts/setup-stub.py --check   build both and fail unless they match stubs/SHA256SUMS

Any Mac makes the same bytes, wherever the checkout and the cargo home are: setup/rust-toolchain.toml
pins rustc, cargo-xwin the MSVC CRT and SDK libraries at CRT and SDK below, and the linker is that
rustc's own rust-lld, as lld-link. The paths a host would leave in the exe are remapped (the cargo
home's sources, the toolchain's rust-src) or dropped (panic locations), and /Brepro puts a hash of
the exe where the linker's timestamp was. A Windows or Linux host's rustc, the same version, lays
the functions out in another order, so the stubs are built and checked on a Mac alone.
"""

import argparse
import hashlib
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SETUP = ROOT / "setup"
STUBS = ROOT / "stubs"
SUMS = STUBS / "SHA256SUMS"
TARGETS = {"x64": "x86_64-pc-windows-msvc", "arm64": "aarch64-pc-windows-msvc"}
CRT, SDK = "14.44.17.14", "10.0.26100"


def out(*cmd: str) -> str:
  return subprocess.run(cmd, cwd=SETUP, check=True, capture_output=True, text=True).stdout.strip()


def build(dest: Path) -> dict[str, str]:
  """Both stubs built into `dest` as shipo-setup-<arch>.exe; their SHA-256 by file name."""
  # Run in setup/, so rustup takes setup/rust-toolchain.toml.
  subprocess.run(["rustup", "toolchain", "install"], cwd=SETUP, check=True)
  sysroot = Path(out("rustc", "--print", "sysroot"))
  commit = next(x.split()[1] for x in out("rustc", "-vV").splitlines() if x.startswith("commit-hash"))
  cargo_home = Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")
  host_bin = Path(out("rustc", "--print", "target-libdir")).parent / "bin"

  # cargo-xwin links with the first lld-link on PATH: this rustc's rust-lld under that name.
  tools = SETUP / "target" / "lld"
  shutil.rmtree(tools, ignore_errors=True)
  tools.mkdir(parents=True)
  shutil.copy2(host_bin / "rust-lld", tools / "lld-link")

  flags = [
    "-Ctarget-feature=+crt-static",
    "-Zlocation-detail=none",
    f"--remap-path-prefix={cargo_home}=/cargo",
    f"--remap-path-prefix={sysroot / 'lib' / 'rustlib' / 'src' / 'rust'}=/rustc/{commit}",
    f"--remap-path-prefix={SETUP}=/setup",
    "-Clink-arg=/Brepro",
    "-Clink-arg=/DEBUG:NONE",
  ]

  env = {
    **os.environ,
    "RUSTFLAGS": " ".join(flags),
    "XWIN_CACHE_DIR": str(SETUP / "target" / "xwin"),
    "XWIN_VERSION": "17",
    "XWIN_CRT_VERSION": CRT,
    "XWIN_SDK_VERSION": SDK,
    "XWIN_ARCH": "x86_64,aarch64",
    "PATH": os.pathsep.join([str(tools), os.environ["PATH"]]),
  }

  targets = [x for t in TARGETS.values() for x in ("--target", t)]
  subprocess.run(["cargo", "xwin", "build", "--release", "--locked", *targets], cwd=SETUP, env=env, check=True)

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
