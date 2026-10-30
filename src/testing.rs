// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! What the tests of more than one module share: a feed signed by OpenSSL and its key, RFC 8032's
//! key to sign others, and folders of files.

use std::fs;
use std::path::{Path, PathBuf};

use ed25519_compact::{KeyPair, Seed};

/// A feed signed with `openssl pkeyutl -sign -rawin` by the key whose public half is [KEY]; its
/// `tags` line is a word shipo doesn't know.
pub const FEED: &str = concat!(
  "version 1.2.0\n",
  "tags beta\n",
  "macos-aarch64 3 ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f https://example.com/app.zip\n",
  "linux-x86_64 3 ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f https://example.com/app.AppImage\n",
  "windows-x86_64 3 ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f https://example.com/app.zip\n",
  "test-http 3 ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f http://example.com/app.zip\n",
  "sig e965a7ea705bcf508b05a6c8ba1c74bf3737f91f0da936a7b5ce4b729a17f67dce041429b42008d440c5715c70fd1ae473e1e179b4b76b8641748b095442b105\n",
);
/// The last 32 bytes of `openssl pkey -pubout -outform DER`, in hex.
pub const KEY: &str = "6bf026c9174d547c1d68f3ec8267273426167628161897f3661a4677a6c24b6f";
/// The SHA-512 of `abc` (FIPS 180-2).
pub const ABC: &str = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";
/// RFC 8032's test 1: the seed and its public key, the spare in the tests.
const RFC_SEED: [u8; 32] = [
  0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c, 0xc4,
  0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae, 0x7f, 0x60,
];
pub const RFC_KEY: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";

/// The signature of `data` by [RFC_KEY]'s seed, in hex.
pub fn sign(data: &[u8]) -> String {
  let sig = KeyPair::from_seed(Seed::new(RFC_SEED)).sk.sign(data, None);
  sig.iter().map(|b| format!("{b:02x}")).collect()
}

/// A new empty folder in the temp dir.
pub fn dir(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("shipo-test-{}-{name}", std::process::id()));
  let _ = fs::remove_dir_all(&dir);
  fs::create_dir_all(&dir).unwrap();
  dir
}

/// Writes each `(path, text)` under `root`, with its folders.
pub fn write(root: &Path, files: &[(&str, &str)]) {
  for (path, text) in files {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
  }
}
