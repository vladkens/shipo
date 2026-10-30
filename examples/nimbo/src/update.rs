// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The update's steps apart from the window: where the update is, what the user can do next and
//! what the threads that run shipo's blocking calls report back.

use std::{mem, thread};

use shipo::{Feed, Release, Staged};

/// The feed and the public keys `shipo keygen update.key update-spare.key` printed: the working
/// one and the spare. Put in your own; until then a check fails on the placeholders' keys. For a
/// local test, the URL is that of `shipo feed --url http://127.0.0.1:8000`'s feed, served there.
pub const FEED: Feed = Feed::new(
  "https://github.com/me/hello/releases/latest/download/update.txt",
  &["<working public key>", "<spare public key>"],
);

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the update is.
#[derive(Debug, Default)]
pub enum Step {
  /// Nothing asked yet.
  #[default]
  Idle,
  Checking,
  /// The feed offers nothing newer.
  Latest,
  /// A newer release, to download.
  Found(Release),
  /// The release's file coming: the bytes done and the total.
  Downloading(Release, u64, u64),
  /// Downloaded and verified, to install.
  Ready(Release, Staged),
  Installing,
  /// The installer runs and waits for this app to end: the app quits now.
  Quit,
  Failed(String),
}

/// What a thread sends back.
#[derive(Debug)]
pub enum Event {
  Checked(shipo::Result<Option<Release>>),
  Progress(u64, u64),
  Downloaded(shipo::Result<Staged>),
  Installed(shipo::Result<()>),
}

impl Step {
  /// A call runs: the buttons wait for it.
  pub fn busy(&self) -> bool {
    matches!(self, Step::Checking | Step::Downloading(..) | Step::Installing | Step::Quit)
  }

  /// The step after `event`.
  pub fn next(self, event: Event) -> Step {
    match (self, event) {
      (_, Event::Checked(Ok(None))) => Step::Latest,
      (_, Event::Checked(Ok(Some(release)))) => Step::Found(release),
      (Step::Downloading(release, ..), Event::Progress(done, total)) => {
        Step::Downloading(release, done, total)
      }
      (Step::Downloading(release, ..), Event::Downloaded(Ok(staged))) => {
        Step::Ready(release, staged)
      }
      (_, Event::Installed(Ok(()))) => Step::Quit,
      (_, Event::Checked(Err(e)) | Event::Downloaded(Err(e)) | Event::Installed(Err(e))) => {
        Step::Failed(e.to_string())
      }
      (step, _) => step,
    }
  }

  /// Checks the feed, unless a call runs.
  pub fn check(&mut self, send: impl Fn(Event) + Send + 'static) {
    if self.busy() {
      return;
    }

    *self = Step::Checking;
    thread::spawn(move || send(Event::Checked(shipo::check(&FEED, VERSION))));
  }

  /// Downloads the release found.
  pub fn download(&mut self, send: impl Fn(Event) + Send + 'static) {
    let Step::Found(release) = self else { return };
    let release = release.clone();
    *self = Step::Downloading(release.clone(), 0, release.size);
    thread::spawn(move || {
      let staged = shipo::download(&release, |done, total| send(Event::Progress(done, total)));
      send(Event::Downloaded(staged));
    });
  }

  /// Starts the installer of the release downloaded; the app quits once it runs.
  pub fn install(&mut self, send: impl Fn(Event) + Send + 'static) {
    match mem::take(self) {
      Step::Ready(_, staged) => {
        *self = Step::Installing;
        thread::spawn(move || send(Event::Installed(shipo::install(staged))));
      }
      step => *self = step,
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::mpsc;

  use super::*;

  /// The step `act` starts, and the one after its thread's last event.
  fn run(mut step: Step, act: fn(&mut Step, mpsc::Sender<Event>)) -> (String, Step) {
    let (send, recv) = mpsc::channel();
    act(&mut step, send);
    let busy = format!("{step:?}");
    (busy, recv.iter().fold(step, Step::next))
  }

  #[test]
  fn check_fails_on_the_placeholder_keys() {
    let (busy, done) = run(Step::Idle, |s, send| s.check(move |e| send.send(e).unwrap()));
    assert_eq!(busy, "Checking");
    assert!(matches!(&done, Step::Failed(e) if e.contains("no public key")), "{done:?}");
  }

  #[test]
  fn download_fails_out_of_an_installed_app() {
    let found = Step::Found(Release::test("2.0.0"));
    let (busy, done) = run(found, |s, send| s.download(move |e| send.send(e).unwrap()));
    assert!(busy.starts_with("Downloading"), "{busy}");
    assert!(matches!(&done, Step::Failed(e) if e.starts_with("can't update in place")), "{done:?}");
  }

  #[test]
  fn steps() {
    let release = || Release::test("2.0.0");
    assert!(matches!(Step::Checking.next(Event::Checked(Ok(None))), Step::Latest));
    assert!(matches!(Step::Checking.next(Event::Checked(Ok(Some(release())))), Step::Found(_)));

    let step = Step::Downloading(release(), 0, 10).next(Event::Progress(4, 10));
    assert!(matches!(step, Step::Downloading(_, 4, 10)));
    assert!(matches!(Step::Idle.next(Event::Progress(4, 10)), Step::Idle));

    assert!(matches!(Step::Installing.next(Event::Installed(Ok(()))), Step::Quit));
    let failed = Step::Installing.next(Event::Installed(Err(shipo::Error::Keys)));
    assert!(matches!(failed, Step::Failed(e) if e.contains("no public key")));
  }

  #[test]
  fn nothing_starts_out_of_its_step() {
    let mut step = Step::Checking;
    step.check(|_| panic!("a second check"));
    step.download(|_| panic!("a download"));
    step.install(|_| panic!("an install"));
    assert!(matches!(step, Step::Checking));
  }
}
