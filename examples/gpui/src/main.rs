// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! A GPUI app, drawn with GPUI Kit's components, that updates itself from a shipo feed: its
//! version, a button that checks the feed, the release found with its notes, the download's
//! progress, then the install, after which it quits and the new version starts in its place.

mod update;

use std::mem;

use futures::StreamExt;
use futures::channel::mpsc::{self, UnboundedSender};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::link::Link;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme, Disableable, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use update::{Event, Step, VERSION};

struct Updater {
  step: Step,
  /// The error of the update that started this run, from [shipo::finish].
  failed: Option<String>,
  /// Why this run can't update in place, as a build started by `cargo run`.
  unsupported: Option<String>,
  send: UnboundedSender<Event>,
}

impl Updater {
  fn new(failed: Option<String>, unsupported: Option<String>, cx: &mut Context<Self>) -> Self {
    let (send, mut recv) = mpsc::unbounded();
    cx.spawn(async move |this, cx| {
      while let Some(event) = recv.next().await {
        let open = this.update(cx, |this, cx| {
          this.step = mem::take(&mut this.step).next(event);
          if let Step::Quit = this.step {
            cx.quit();
          }

          cx.notify();
        });

        if open.is_err() {
          break;
        }
      }
    })
    .detach();

    Self { step: Step::Idle, failed, unsupported, send }
  }

  /// What a thread calls with its event: the event queued for the window.
  fn sender(&self) -> impl Fn(Event) + Send + 'static {
    let send = self.send.clone();
    move |event| drop(send.unbounded_send(event))
  }

  /// A button that runs `act`, enabled while no call runs.
  fn button(&self, id: &'static str, act: fn(&mut Self), cx: &mut Context<Self>) -> Button {
    let enabled = self.unsupported.is_none() && !self.step.busy();
    Button::new(id).disabled(!enabled).on_click(cx.listener(move |this, _, _, cx| {
      act(this);
      cx.notify();
    }))
  }
}

impl Render for Updater {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    let danger = cx.theme().danger;
    let step = match &self.step {
      Step::Idle => div(),
      Step::Checking => div().child("Checking…"),
      Step::Latest => div().child(format!("{VERSION} is the latest")),
      Step::Found(release) => v_flex()
        .gap_2()
        .child(format!("{} is out", release.version))
        .when_some(release.notes.clone(), |el, notes| {
          el.child(Link::new("notes").href(notes).child("What's new"))
        })
        .child(
          self.button("download", |s| s.step.download(s.sender()), cx).label("Download").primary(),
        ),
      Step::Downloading(release, done, total) => v_flex()
        .gap_2()
        .child(format!("Downloading {}", release.version))
        .child(Progress::new("progress").value(*done as f32 * 100.0 / (*total).max(1) as f32)),
      Step::Ready(release, _) => {
        v_flex().gap_2().child(format!("{} is ready", release.version)).child(
          self
            .button("install", |s| s.step.install(s.sender()), cx)
            .label("Install and restart")
            .primary(),
        )
      }
      Step::Installing | Step::Quit => div().child("Restarting…"),
      Step::Failed(e) => div().text_color(danger).child(e.clone()),
    };

    v_flex()
      .size_full()
      .p_4()
      .gap_3()
      .bg(cx.theme().background)
      .text_color(cx.theme().foreground)
      .child(div().text_xl().child(format!("ShipoGpui {VERSION}")))
      .when_some(self.failed.clone(), |el, e| {
        el.child(div().text_color(danger).child(format!("The last update failed: {e}")))
      })
      .when_some(self.unsupported.clone(), |el, why| el.child(why))
      .child(self.button("check", |s| s.step.check(s.sender()), cx).label("Check for updates"))
      .child(step)
  }
}

fn main() {
  // First, before the arguments and any window: started by an update, this is its installer.
  let failed = shipo::finish().map(|e| e.to_string());

  let unsupported = shipo::supported().err().map(|e| e.to_string());
  gpui_kit::application().run(move |cx| {
    gpui_kit::init(cx);
    cx.on_window_closed(|cx, _| cx.quit()).detach();
    let bounds = Bounds::centered(None, size(px(360.0), px(220.0)), cx);
    let options =
      WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() };
    gpui_kit::open_window(options, cx, |_, cx| cx.new(|cx| Updater::new(failed, unsupported, cx)))
      .expect("the window opens");
  });
}
