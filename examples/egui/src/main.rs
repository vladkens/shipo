// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! An egui app that updates itself from a shipo feed: its version, a button that checks the feed,
//! the release found with its notes, the download's progress, then the install, after which it
//! quits and the new version starts in its place.

mod update;

use std::mem;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui;
use update::{Event, Step, VERSION};

struct App {
  step: Step,
  /// The error of the update that started this run, from [shipo::finish].
  failed: Option<String>,
  /// Why this run can't update in place, as a build started by `cargo run`.
  unsupported: Option<String>,
  send: Sender<Event>,
  recv: Receiver<Event>,
}

impl App {
  /// What a thread calls with its event: the event queued and the window woken to take it.
  fn sender(&self, ctx: &egui::Context) -> impl Fn(Event) + Send + 'static {
    let (send, ctx) = (self.send.clone(), ctx.clone());
    move |event| {
      let _ = send.send(event);
      ctx.request_repaint();
    }
  }
}

impl eframe::App for App {
  fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
    while let Ok(event) = self.recv.try_recv() {
      self.step = mem::take(&mut self.step).next(event);
    }

    if let Step::Quit = self.step {
      ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }

    let (mut check, mut download, mut install) = (false, false, false);
    let enabled = self.unsupported.is_none() && !self.step.busy();
    egui::CentralPanel::default().show(ui, |ui| {
      ui.heading(format!("ShipoEgui {VERSION}"));
      if let Some(e) = &self.failed {
        ui.colored_label(ui.visuals().error_fg_color, format!("The last update failed: {e}"));
      }

      if let Some(why) = &self.unsupported {
        ui.label(why);
      }

      check = ui.add_enabled(enabled, egui::Button::new("Check for updates")).clicked();
      match &self.step {
        Step::Idle => {}
        Step::Checking => drop(ui.label("Checking…")),
        Step::Latest => drop(ui.label(format!("{VERSION} is the latest"))),
        Step::Found(release) => {
          ui.label(format!("{} is out", release.version));
          if let Some(notes) = &release.notes {
            ui.hyperlink_to("What's new", notes);
          }

          download = ui.add_enabled(enabled, egui::Button::new("Download")).clicked();
        }
        Step::Downloading(release, done, total) => {
          ui.label(format!("Downloading {}", release.version));
          ui.add(egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32).show_percentage());
        }
        Step::Ready(release, _) => {
          ui.label(format!("{} is ready", release.version));
          install = ui.add_enabled(enabled, egui::Button::new("Install and restart")).clicked();
        }
        Step::Installing | Step::Quit => drop(ui.label("Restarting…")),
        Step::Failed(e) => drop(ui.colored_label(ui.visuals().error_fg_color, e)),
      }
    });

    let send = self.sender(ui.ctx());
    if check {
      self.step.check(send);
    } else if download {
      self.step.download(send);
    } else if install {
      self.step.install(send);
    }
  }
}

fn main() -> eframe::Result {
  // First, before the arguments and any window: started by an update, this is its installer.
  let failed = shipo::finish().map(|e| e.to_string());

  let (send, recv) = mpsc::channel();
  let unsupported = shipo::supported().err().map(|e| e.to_string());
  let app = App { step: Step::Idle, failed, unsupported, send, recv };
  let viewport = egui::ViewportBuilder::default().with_inner_size([360.0, 200.0]);
  let options = eframe::NativeOptions { viewport, ..Default::default() };
  eframe::run_native("ShipoEgui", options, Box::new(|_| Ok(Box::new(app))))
}
