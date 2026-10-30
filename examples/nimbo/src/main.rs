// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! A nimbo app that updates itself from a shipo feed: its version, a button that checks the feed,
//! the release found with its notes, the download's progress, then the install, after which it
//! quits and the new version starts in its place.

// A Windows app, not a console one: no terminal opens with its window.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod update;

use std::mem;
use std::sync::mpsc::{self, Receiver, Sender};

use nimbo::{AppInfo, Config, Main, Png, Presence, Proxy, Style, TextStyle, Ui};
use update::{Event, Step, VERSION};

/// The window's icon on Windows and Linux; macOS shows the bundle's.
const ICON: Png = Png::icon(include_bytes!("../icon.png"));

const CHECK: &str = "check";
const NOTES: &str = "notes";
const DOWNLOAD: &str = "download";
const INSTALL: &str = "install";

struct App {
  step: Step,
  /// The error of the update that started this run, from [shipo::finish].
  failed: Option<String>,
  /// Why this run can't update in place, as a build started by `cargo run`.
  unsupported: Option<String>,
  send: Sender<Event>,
  recv: Receiver<Event>,
  /// Quits the app once the installer runs.
  proxy: Proxy,
}

impl App {
  /// What a thread calls with its event: the event queued and the window woken to take it.
  fn sender(&self, ui: &Ui) -> impl Fn(Event) + Send + 'static {
    let (send, waker) = (self.send.clone(), ui.waker().clone());
    move |event| {
      let _ = send.send(event);
      waker.wake();
    }
  }
}

impl nimbo::App for App {
  fn view(&mut self, ui: &mut Ui) {
    while let Ok(event) = self.recv.try_recv() {
      self.step = mem::take(&mut self.step).next(event);
      if let Step::Quit = self.step {
        self.proxy.post(Main::quit);
      }
    }

    let t = ui.theme();
    let error = t.body().color(t.danger);
    let (mut check, mut download, mut install) = (false, false, false);
    let enabled = self.unsupported.is_none() && !self.step.busy();
    ui.col(Style::new().grow().pad(20.0).gap(12.0).bg(t.bg), |ui| {
      ui.text(format!("ShipoNimbo {VERSION}"), TextStyle::new(t.bold, t.text));
      if let Some(e) = &self.failed {
        ui.text(format!("The last update failed: {e}"), error);
      }

      if let Some(why) = &self.unsupported {
        ui.text(why, t.body());
      }

      check = ui.disabled(!enabled).id(CHECK).button("Check for updates").clicked;
      match &self.step {
        Step::Idle => {}
        Step::Checking => ui.text("Checking…", t.body()),
        Step::Latest => ui.text(format!("{VERSION} is the latest"), t.body()),
        Step::Found(release) => {
          ui.text(format!("{} is out", release.version), t.body());
          if let Some(notes) = &release.notes
            && ui.id(NOTES).link("What's new", t.body().color(t.link)).clicked
          {
            let _ = nimbo::open::url(notes);
          }

          download = ui.disabled(!enabled).id(DOWNLOAD).button("Download").clicked;
        }
        Step::Downloading(release, done, total) => {
          ui.text(format!("Downloading {}", release.version), t.body());
          ui.progress(Some(*done as f32 / (*total).max(1) as f32));
        }
        Step::Ready(release, _) => {
          ui.text(format!("{} is ready", release.version), t.body());
          install = ui.disabled(!enabled).id(INSTALL).button("Install and restart").clicked;
        }
        Step::Installing | Step::Quit => ui.text("Restarting…", t.body()),
        Step::Failed(e) => ui.text(e, error),
      }
    });

    let send = self.sender(ui);
    if check {
      self.step.check(send);
    } else if download {
      self.step.download(send);
    } else if install {
      self.step.install(send);
    }
  }
}

fn main() {
  // First, before the arguments and any window: started by an update, this is its installer.
  let failed = shipo::finish().map(|e| e.to_string());

  let unsupported = shipo::supported().err().map(|e| e.to_string());
  let info =
    AppInfo { id: "dev.shipo.example.nimbo", name: "ShipoNimbo", presence: Presence::Regular };
  nimbo::app(info, |main| {
    let (send, recv) = mpsc::channel();
    let app = App { step: Step::Idle, failed, unsupported, send, recv, proxy: main.proxy() };
    let window = main.open(&Config::new("ShipoNimbo", 380.0, 240.0).png_icon(ICON), app);
    window.on_close(Main::quit);
  });
}
