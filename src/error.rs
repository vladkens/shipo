// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The library's error: what a check, a download or an install fails on, by its meaning.

/// Everything that can fail in an update: reading the feed, fetching the files, checking them and
/// putting the new version in.
///
/// It is `#[non_exhaustive]`: a later version may tell more apart, so keep a `_` arm when
/// matching on it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
  /// A file or a process failed: the staging folder (`PermissionDenied` in a folder the user
  /// can't write, as `/Applications` for a standard user or `/opt`), the downloaded file, or a
  /// tool the update runs (`hdiutil`, `ditto` on macOS).
  #[error(transparent)]
  Io(#[from] std::io::Error),
  /// The feed is not one `spec.md` gives: larger than 1 MiB, not UTF-8, a record malformed or
  /// repeated, no version or one that doesn't compare with the app's, a URL not https; or on
  /// Windows a release URL that names no setup.
  #[error("invalid feed: {0}")]
  Feed(String),
  /// The feed is not signed by any of the app's keys: its last line is not its one `sig` line, or
  /// no key verifies the signature. A feed changed on the way, or signed by a key the app doesn't
  /// hold.
  #[error("bad feed signature: {0}")]
  Signature(String),
  /// The app's own [`Feed::keys`](crate::Feed::keys) are none or one is not 64 hex digits of a
  /// valid Ed25519 public key: the app's mistake, whatever the feed.
  #[error("the app has no public key for its feed, or a malformed one")]
  Keys,
  /// A request failed: an HTTP status other than 2xx, or no response at all (the network, the
  /// TLS handshake, a transfer that stalled for a minute), with the OS's message.
  #[error("{url}: {message}")]
  Http {
    /// The URL requested.
    url: String,
    /// The HTTP status of an error response (404: no such file); `None` when no response came.
    status: Option<u16>,
    /// What went wrong: `HTTP <status>`, or the OS's HTTP stack's message.
    message: String,
  },
  /// The downloaded file is not the release's: larger than its size in the feed, or of another
  /// size or SHA-512. A file changed on the way or on the server; another download may pass.
  #[error("the download doesn't match the feed: {0}")]
  Mismatch(String),
  /// On macOS the release's bundle can't replace the running one: the image holds no `.app` or
  /// more than one, or the bundle has another id or doesn't meet the running app's designated
  /// requirement, as an ad-hoc signed build's: every release must be signed with one identity.
  #[error("the release can't replace this app: {0}")]
  Bundle(String),
  /// This app can't update in place: a dev build (a pre-release version), not an installed app
  /// (a build, a Windows exe outside `%LOCALAPPDATA%\Programs`, a Linux one not run as an
  /// AppImage), a bundle macOS translocated, an app the user can't write, a bundle on a volume
  /// without APFS's swap, or Linux without libcurl.
  #[error("can't update in place: {0}")]
  Unsupported(String),
  /// Putting the new version in failed: from [`install`](crate::install) an installer that gave
  /// no sign in 10 s; from [`finish`](crate::finish) the error the installer that started this
  /// run noted in `SHIPO_UPDATE_ERROR`: the swap or the setup failed (the setup's exit code), or
  /// the new version didn't start.
  #[error("installing the update: {0}")]
  Install(String),
}

/// `Result` with shipo's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn an_error_goes_to_another_thread() {
    const fn sendable<T: Send + Sync + 'static>() {}
    sendable::<Error>();
    let e: Box<dyn std::error::Error + Send + Sync> = Box::new(Error::Keys);
    assert!(e.to_string().contains("public key"));
  }
}
