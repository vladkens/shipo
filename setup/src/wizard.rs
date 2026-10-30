// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The setup's wizard when not silent, NSIS MUI2's pages in plain Win32: Welcome, the folder, the
//! progress with its log and Finish; for the uninstaller, confirm, progress and Finish.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, mpsc};
use std::{io, thread};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
  BeginPaint, COLOR_BTNFACE, CreateFontIndirectW, CreateSolidBrush, DeleteObject, EndPaint,
  FW_BOLD, FillRect, GetDC, GetDeviceCaps, GetMonitorInfoW, GetStockObject, HBRUSH, HDC, HFONT,
  InvalidateRect, LOGPIXELSY, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
  PAINTSTRUCT, ReleaseDC, SetBkColor, WHITE_BRUSH,
};
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows_sys::Win32::System::Com::{
  CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::Controls::{
  BST_CHECKED, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, PBM_SETPOS,
  PBM_SETRANGE32,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows_sys::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, SIGDN_FILESYSPATH};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::core::{BOOL, GUID, HRESULT, PWSTR};

use super::windows::{method, pwstr, user32, wide};
use super::{Step, bytes, folder};

// The layout in pixels at 96 DPI, as NSIS MUI2's.
const W: i32 = 497;
const H: i32 = 360;
const BANNER: i32 = 164;
const HEADER: i32 = 57;
const FOOT: i32 = 313;
const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
const SS_ETCHEDHORZ: u32 = 0x10;
const BAR: u32 = 1000;
/// The app's group icon, as the packer stamps it.
const ICON: usize = 1;
const BLUE: u32 = 0x00A5_6E3A;

// Controls' ids; from WHITE on, on a white ground.
const BACK: i32 = 3;
const BROWSE: i32 = 4;
const EDIT: i32 = 5;
const STATUS: i32 = 6;
const LIST: i32 = 7;
const PROGRESS: i32 = 8;
const FREE: i32 = 10;
/// The timers: the work's progress, the free space once typing stops.
const TICK: usize = 1;
const SPACE: usize = 2;
/// The message font's height the layout is drawn for, Segoe UI 9 pt at 96 DPI.
const EM: i32 = 12;
const WHITE: i32 = 100;
const RUN: i32 = 104;
const DESKTOP: i32 = 105;

/// How the install ended when it ran: its folder, and the finish page's boxes.
pub struct Finish {
  pub dir: PathBuf,
  pub run: bool,
  pub desktop: bool,
}

/// Installs `name` of `size` bytes by `work` where the user says, `dir` offered, or in `dir` alone
/// for an `update`, whose old folder would stay; none if cancelled.
pub fn install(
  title: &str,
  name: &str,
  (dir, update): (&Path, bool),
  size: u64,
  work: impl FnOnce(&Path, Step) -> io::Result<()> + Send,
) -> io::Result<Option<Finish>> {
  let w = Wizard { update, ..Wizard::new(title, name, false, dir, size) };
  let done = w.show(work)?;
  let Some(done) = done else { return Ok(None) };
  done?;
  let dir = PathBuf::from(w.dir.take());
  Ok(Some(Finish { dir, run: w.run.get(), desktop: w.desktop.get() }))
}

/// Uninstalls the app `name` from `dir` by `work` once the user agrees; none when not.
pub fn uninstall(
  title: &str,
  name: &str,
  dir: &Path,
  work: impl FnOnce(Step) -> io::Result<()> + Send,
) -> Option<io::Result<()>> {
  let w = Wizard::new(title, name, true, dir, 0);
  w.show(|_, step| work(step)).unwrap_or_else(|e| Some(Err(e)))
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
  Welcome,
  Folder,
  Confirm,
  Work,
  Finish,
}

/// What the work's thread tells the window: the bar, the log, the end.
#[derive(Default)]
struct Shared {
  bar: AtomicU32,
  log: Mutex<Vec<String>>,
  result: Mutex<Option<io::Result<()>>>,
}

/// The window's state, on its thread.
struct Wizard {
  hwnd: Cell<HWND>,
  title: String,
  name: String,
  uninstall: bool,
  update: bool,
  page: Cell<Page>,
  dir: RefCell<String>,
  size: u64,
  dpi: Cell<u32>,
  /// The message font's height, which the layout scales by, as dialog units do.
  em: Cell<i32>,
  /// The message font, bold, and bold and larger.
  fonts: Cell<[HFONT; 3]>,
  /// The app's icon for the banner and for the header.
  icons: Cell<[HICON; 2]>,
  banner: HBRUSH,
  shared: Shared,
  /// The log's lines in the list.
  shown: Cell<usize>,
  done: Cell<bool>,
  /// Starts the work in its folder; dropped, it ends the work's wait.
  start: Cell<Option<mpsc::Sender<PathBuf>>>,
  /// Whether the user cancelled before the work.
  cancelled: Cell<bool>,
  run: Cell<bool>,
  desktop: Cell<bool>,
}

impl Wizard {
  fn new(title: &str, name: &str, uninstall: bool, dir: &Path, size: u64) -> Self {
    Self {
      hwnd: Cell::new(null_mut()),
      title: title.into(),
      name: name.into(),
      uninstall,
      update: false,
      page: Cell::new(if uninstall { Page::Confirm } else { Page::Welcome }),
      dir: RefCell::new(dir.display().to_string()),
      size,
      dpi: Cell::new(96),
      em: Cell::new(EM),
      fonts: Cell::new([null_mut(); 3]),
      icons: Cell::new([null_mut(); 2]),
      // SAFETY: a solid brush, kept for the process.
      banner: unsafe { CreateSolidBrush(BLUE) },
      shared: Shared::default(),
      shown: Cell::new(0),
      done: Cell::new(false),
      start: Cell::new(None),
      cancelled: Cell::new(false),
      run: Cell::new(true),
      desktop: Cell::new(false),
    }
  }

  /// The window until it closes, `work` run on a thread once started; its result, if it ran.
  fn show(
    &self,
    work: impl FnOnce(&Path, Step) -> io::Result<()> + Send,
  ) -> io::Result<Option<io::Result<()>>> {
    common_controls();
    // SAFETY: COM on this thread, for the folder picker.
    unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) };
    let (start, wait) = mpsc::channel::<PathBuf>();
    self.start.set(Some(start));
    let shared = &self.shared;
    thread::scope(|s| {
      s.spawn(move || {
        let Ok(dir) = wait.recv() else { return };
        let step = |line: &str, part: f32| {
          shared.bar.store((part * BAR as f32) as u32, Ordering::Relaxed);
          let mut log = shared.log.lock().unwrap();
          if log.last().is_none_or(|l| l != line) {
            log.push(line.into());
          }
        };

        let done = work(&dir, &step);
        *shared.result.lock().unwrap() = Some(done);
      });

      let shown = self.open();
      self.start.take();
      shown
    })?;

    if self.cancelled.get() {
      return Ok(None);
    }

    Ok(self.shared.result.lock().unwrap().take())
  }

  /// The window made, centred on its screen and run until it closes.
  fn open(&self) -> io::Result<()> {
    let class = wide("ShipoSetup");
    let title = wide(&self.title);
    // SAFETY: this module's window class, `self` outliving its window and message loop.
    unsafe {
      let inst = GetModuleHandleW(null());
      let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: inst,
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: (COLOR_BTNFACE + 1) as usize as HBRUSH,
        lpszClassName: class.as_ptr(),
        ..Default::default()
      };

      RegisterClassExW(&wc);
      let (x, ex) = (CW_USEDEFAULT, WS_EX_CONTROLPARENT);
      let (me, c, t, n) =
        (self as *const Self as *const c_void, class.as_ptr(), title.as_ptr(), null_mut());
      let hwnd = CreateWindowExW(ex, c, t, STYLE, x, x, 0, 0, n, n, inst, me);
      if hwnd.is_null() {
        return Err(io::Error::last_os_error());
      }

      self.hwnd.set(hwnd);
      self.scale(dpi(hwnd));
      let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
      GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY), &mut mi);
      let ((cx, cy), a) = (self.outer(), mi.rcWork);
      let (x, y) = (a.left + (a.right - a.left - cx) / 2, a.top + (a.bottom - a.top - cy) / 2);
      SetWindowPos(hwnd, null_mut(), x, y, cx, cy, SWP_NOZORDER | SWP_NOACTIVATE);
      self.build();
      ShowWindow(hwnd, SW_SHOWNORMAL);
      let mut msg = MSG::default();
      while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        if IsDialogMessageW(hwnd, &msg) == 0 {
          TranslateMessage(&msg);
          DispatchMessageW(&msg);
        }
      }
    }

    Ok(())
  }

  /// `v` layout pixels at the window's font: its DPI and the user's text size.
  fn s(&self, v: i32) -> i32 {
    v * self.em.get() / EM
  }

  /// The window's size for its client area.
  fn outer(&self) -> (i32, i32) {
    let mut r = RECT { left: 0, top: 0, right: self.s(W), bottom: self.s(H) };
    let ex = WS_EX_CONTROLPARENT;
    type Adjust = unsafe extern "system" fn(*mut RECT, u32, BOOL, u32, u32) -> BOOL;
    // SAFETY: a rect to adjust; the export of its documented type, Windows 10 1607 and later.
    unsafe {
      match user32::<Adjust>(c"AdjustWindowRectExForDpi") {
        Some(adjust) => adjust(&mut r, STYLE, 0, ex, self.dpi.get()),
        None => AdjustWindowRectEx(&mut r, STYLE, 0, ex),
      };
    }

    (r.right - r.left, r.bottom - r.top)
  }

  /// The fonts and icons at `dpi`, the old ones freed.
  fn scale(&self, dpi: u32) {
    self.dpi.set(dpi);
    type Info = unsafe extern "system" fn(u32, u32, *mut c_void, u32, u32) -> BOOL;
    type Metric = unsafe extern "system" fn(i32, u32) -> i32;
    // SAFETY: the metrics into their own struct; the exports of their documented types, Windows 10
    // 1607 and later; GDI objects and icons this window alone uses.
    unsafe {
      let mut m =
        NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
      let (size, at) = (m.cbSize, (&raw mut m).cast());
      match user32::<Info>(c"SystemParametersInfoForDpi") {
        Some(info) => info(SPI_GETNONCLIENTMETRICS, size, at, 0, dpi),
        None => SystemParametersInfoW(SPI_GETNONCLIENTMETRICS, size, at, 0),
      };

      let mut lf = m.lfMessageFont;
      let em = lf.lfHeight.abs();
      self.em.set(if em > 0 { em } else { EM * dpi as i32 / 96 });
      let normal = CreateFontIndirectW(&lf);
      lf.lfWeight = FW_BOLD as i32;
      let bold = CreateFontIndirectW(&lf);
      lf.lfHeight = lf.lfHeight * 4 / 3;
      let big = CreateFontIndirectW(&lf);
      for f in self.fonts.replace([normal, bold, big]) {
        DeleteObject(f);
      }

      let inst = GetModuleHandleW(null());
      let icon = |size: i32| LoadImageW(inst, ICON as _, IMAGE_ICON, size, size, 0);
      for i in self.icons.replace([icon(self.s(96)), icon(self.s(32))]) {
        DestroyIcon(i);
      }

      let hwnd = self.hwnd.get();
      for (kind, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
        let size = match user32::<Metric>(c"GetSystemMetricsForDpi") {
          Some(of) => of(metric, dpi),
          None => GetSystemMetrics(metric),
        };

        let icon = icon(size) as isize;
        DestroyIcon(SendMessageW(hwnd, WM_SETICON, kind as usize, icon) as HICON);
      }
    }
  }

  /// A child control at `r`, `[x, y, w, h]` at 96 DPI, in [Self::fonts]' `font`.
  fn ctl(&self, kind: &str, text: &str, style: i32, r: [i32; 4], id: i32, font: usize) -> HWND {
    let ex = if matches!(kind, "Edit" | "ListBox") { WS_EX_CLIENTEDGE } else { 0 };
    let (kind, text) = (wide(kind), wide(text));
    let ([x, y, w, h], (k, t)) = (r.map(|v| self.s(v)), (kind.as_ptr(), text.as_ptr()));
    let style = WS_CHILD | WS_VISIBLE | style as u32;
    let (parent, id) = (self.hwnd.get(), id as usize as HMENU);
    // SAFETY: a child of this window, NUL-terminated strings.
    unsafe {
      let inst = GetModuleHandleW(null());
      let c = CreateWindowExW(ex, k, t, style, x, y, w, h, parent, id, inst, null());
      SendMessageW(c, WM_SETFONT, self.fonts.get()[font] as usize, 0);
      c
    }
  }

  fn item(&self, id: i32) -> HWND {
    // SAFETY: a child by its id, null when the page has none.
    unsafe { GetDlgItem(self.hwnd.get(), id) }
  }

  /// The page's controls made anew, what the user set kept.
  fn build(&self) {
    let hwnd = self.hwnd.get();
    // SAFETY: this window's own children.
    unsafe {
      loop {
        let child = GetWindow(hwnd, GW_CHILD);
        if child.is_null() {
          break;
        }

        DestroyWindow(child);
      }
    }

    let n = &self.name;
    let tab = WS_TABSTOP as i32;
    let off = WS_DISABLED as i32;
    let page = self.page.get();
    let (back, next, cancel) = match page {
      Page::Welcome => (None, "Next >", 0),
      Page::Folder => (Some(0), "Install", 0),
      Page::Confirm => (None, "Uninstall", 0),
      Page::Work => (Some(off), "Next >", off),
      Page::Finish => (Some(off), "Finish", off),
    };

    let next_off = if page == Page::Work && !self.done.get() { off } else { 0 };
    self.ctl("Static", "", SS_ETCHEDHORZ as i32, [0, FOOT, W, 2], 0, 0);
    if let Some(back) = back {
      self.ctl("Button", "< Back", tab | back, [252, 325, 75, 23], BACK, 0);
    }

    let def = BS_DEFPUSHBUTTON | tab | next_off;
    self.ctl("Button", next, def, [327, 325, 75, 23], IDOK, 0);
    self.ctl("Button", "Cancel", tab | cancel, [412, 325, 75, 23], IDCANCEL, 0);
    let header = |title: &str, sub: &str| {
      self.ctl("Static", title, 0, [15, 10, 400, 16], WHITE, 1);
      self.ctl("Static", sub, 0, [25, 28, 400, 26], WHITE + 1, 0);
      self.ctl("Static", "", SS_ETCHEDHORZ as i32, [0, HEADER, W, 2], 0, 0);
    };

    let dir = self.dir.borrow().clone();
    match page {
      Page::Welcome | Page::Finish => {
        let (title, text) = match (page, self.uninstall) {
          (Page::Welcome, _) => (
            format!("Welcome to {n} Setup"),
            format!("Setup will install {n} on your computer.\r\n\r\nClick Next to continue."),
          ),
          (_, false) => (
            format!("Completing {n} Setup"),
            format!("{n} has been installed on your computer.\r\n\r\nClick Finish to close Setup."),
          ),
          (_, true) => (
            format!("Completing {n} Uninstall"),
            format!("{n} has been uninstalled from your computer.\r\n\r\nClick Finish to close."),
          ),
        };

        self.ctl("Static", &title, 0, [176, 12, 311, 48], WHITE + 2, 2);
        self.ctl("Static", &text, 0, [176, 66, 311, 70], WHITE + 3, 0);
        if page == Page::Finish && !self.uninstall {
          let check = BS_AUTOCHECKBOX | tab;
          let run = self.ctl("Button", &format!("Run {n}"), check, [176, 150, 311, 20], RUN, 0);
          let desk = "Create desktop shortcut";
          let desk = self.ctl("Button", desk, check, [176, 174, 311, 20], DESKTOP, 0);
          for (c, on) in [(run, self.run.get()), (desk, self.desktop.get())] {
            // SAFETY: a check box of this window.
            unsafe { SendMessageW(c, BM_SETCHECK, on as usize, 0) };
          }
        }
      }
      Page::Folder => {
        header("Choose Install Location", &format!("Choose the folder in which to install {n}."));
        let text = match self.update {
          true => format!(
            "Setup will update {n} in the following folder. Click Install to start the installation."
          ),
          false => format!(
            "Setup will install {n} in the following folder. To install in a different folder, \
             click Browse and select another folder. Click Install to start the installation."
          ),
        };

        let fixed = if self.update { (ES_READONLY, off) } else { (0, 0) };
        self.ctl("Static", &text, 0, [22, 72, 453, 48], 0, 0);
        self.ctl("Button", "Destination Folder", BS_GROUPBOX, [22, 160, 453, 58], 0, 0);
        self.ctl("Edit", &dir, ES_AUTOHSCROLL | fixed.0 | tab, [36, 184, 330, 23], EDIT, 0);
        self.ctl("Button", "Browse...", fixed.1 | tab, [376, 183, 88, 25], BROWSE, 0);
        let need = format!("Space required: {}", bytes(self.size));
        self.ctl("Static", &need, 0, [22, 240, 453, 16], 0, 0);
        self.ctl("Static", "", 0, [22, 258, 453, 16], FREE, 0);
        self.free();
      }
      Page::Confirm => {
        header(&format!("Uninstall {n}"), &format!("Remove {n} from your computer."));
        let text = format!(
          "{n} will be uninstalled from the following folder. Click Uninstall to start the \
           uninstallation."
        );
        self.ctl("Static", &text, 0, [22, 72, 453, 32], 0, 0);
        self.ctl("Static", "Uninstalling from:", 0, [22, 127, 100, 16], 0, 0);
        self.ctl("Edit", &dir, ES_AUTOHSCROLL | ES_READONLY | tab, [124, 123, 351, 23], EDIT, 0);
      }
      Page::Work => {
        let (title, sub) = self.headline();
        header(title, &sub);
        self.ctl("Static", "", 0, [22, 72, 453, 16], STATUS, 0);
        let bar = self.ctl("msctls_progress32", "", 0, [22, 92, 453, 16], PROGRESS, 0);
        let list = LBS_NOINTEGRALHEIGHT | LBS_NOSEL | WS_VSCROLL as i32 | tab;
        self.ctl("ListBox", "", list, [22, 118, 453, 184], LIST, 0);
        // SAFETY: the bar of this window.
        unsafe { SendMessageW(bar, PBM_SETRANGE32, 0, BAR as isize) };
        self.shown.set(0);
        self.tick();
      }
    }

    // SAFETY: this window, repainted whole for its page's ground.
    unsafe {
      InvalidateRect(hwnd, null(), 1);
      SetFocus(self.item(IDOK));
    }
  }

  /// The page's state the user set, kept for the next build.
  fn save(&self) {
    match self.page.get() {
      Page::Folder => *self.dir.borrow_mut() = text(self.item(EDIT)),
      Page::Finish if !self.uninstall => {
        // SAFETY: the check boxes of this page.
        let on =
          |id| unsafe { SendMessageW(self.item(id), BM_GETCHECK, 0, 0) } == BST_CHECKED as isize;
        self.run.set(on(RUN));
        self.desktop.set(on(DESKTOP));
      }
      _ => {}
    }
  }

  fn go(&self, page: Page) {
    self.page.set(page);
    self.build();
  }

  /// Next, Install, Uninstall or Finish, and Enter.
  fn next(&self) {
    match self.page.get() {
      Page::Welcome => self.go(Page::Folder),
      Page::Folder | Page::Confirm => {
        self.save();
        let dir = self.dir.borrow().clone();
        // The uninstall's folder is where it is; the install's must be one `/D=` takes.
        let Some(dir) = (if self.uninstall { Some(dir.clone().into()) } else { folder(&dir) })
        else {
          let e =
            format!("Choose an absolute folder below a drive's or a share's root, not {dir:?}.");
          return message(self.hwnd.get(), &self.title, &e);
        };

        *self.dir.borrow_mut() = dir.display().to_string();
        if let Some(start) = self.start.take() {
          let _ = start.send(dir);
        }

        self.go(Page::Work);
        // SAFETY: a timer of this window, killed when the work ends.
        unsafe { SetTimer(self.hwnd.get(), TICK, 100, None) };
      }
      Page::Work if self.done.get() => self.go(Page::Finish),
      Page::Work => {}
      Page::Finish => {
        self.save();
        // SAFETY: this window, closed.
        unsafe { DestroyWindow(self.hwnd.get()) };
      }
    }
  }

  /// Cancel, Esc and the close box: nothing changed before the work, ignored during it.
  fn cancel(&self) {
    match self.page.get() {
      Page::Work => return,
      Page::Finish => {
        self.run.set(false);
        self.desktop.set(false);
      }
      _ => self.cancelled.set(true),
    }

    // SAFETY: this window, closed.
    unsafe { DestroyWindow(self.hwnd.get()) };
  }

  /// The work's progress shown; its end: the page complete, or the window closed for its error.
  fn tick(&self) {
    let hwnd = self.hwnd.get();
    let log = self.shared.log.lock().unwrap().clone();
    let list = self.item(LIST);
    // SAFETY: this page's controls, NUL-terminated strings.
    unsafe {
      for line in &log[self.shown.get().min(log.len())..] {
        SendMessageW(list, LB_ADDSTRING, 0, wide(line).as_ptr() as isize);
      }

      SendMessageW(list, LB_SETTOPINDEX, log.len().saturating_sub(1), 0);
      self.shown.set(log.len());
      if let Some(last) = log.last() {
        SetWindowTextW(self.item(STATUS), wide(last).as_ptr());
      }

      let bar = self.shared.bar.load(Ordering::Relaxed);
      SendMessageW(self.item(PROGRESS), PBM_SETPOS, bar as usize, 0);
      let ok = self.shared.result.lock().unwrap().as_ref().map(|r| r.is_ok());
      if ok.is_some() {
        KillTimer(hwnd, TICK);
      }

      match ok {
        Some(true) if !self.done.get() => {
          self.done.set(true);
          self.shared.log.lock().unwrap().push("Completed".into());
          self.shared.bar.store(BAR, Ordering::Relaxed);
          let (title, sub) = self.headline();
          SetWindowTextW(self.item(WHITE), wide(title).as_ptr());
          SetWindowTextW(self.item(WHITE + 1), wide(&sub).as_ptr());
          EnableWindow(self.item(IDOK), 1);
          SetFocus(self.item(IDOK));
          self.tick();
        }
        Some(false) => {
          DestroyWindow(hwnd);
        }
        _ => {}
      }
    }
  }

  /// The work page's header, under way or complete.
  fn headline(&self) -> (&'static str, String) {
    let n = &self.name;
    match (self.uninstall, self.done.get()) {
      (false, false) => ("Installing", format!("Please wait while {n} is being installed.")),
      (true, false) => ("Uninstalling", format!("Please wait while {n} is being uninstalled.")),
      (false, true) => ("Installation Complete", "Setup was completed successfully.".into()),
      (true, true) => ("Uninstallation Complete", "Uninstall was completed successfully.".into()),
    }
  }

  /// The free space where the folder in the box is, or its nearest folder that is.
  fn free(&self) {
    let dir = PathBuf::from(text(self.item(EDIT)));
    let free = dir.ancestors().find_map(|d| {
      let mut free = 0u64;
      let path = wide(&d.display().to_string());
      // SAFETY: a NUL-terminated path, the count into a u64.
      let ok = unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut free, null_mut(), null_mut()) };
      (ok != 0).then_some(free)
    });

    let text = free.map(|f| format!("Space available: {}", bytes(f))).unwrap_or_default();
    // SAFETY: a NUL-terminated string.
    unsafe { SetWindowTextW(self.item(FREE), wide(&text).as_ptr()) };
  }

  /// The system's folder picker; the folder picked, with the app's name added, in the box.
  fn browse(&self) {
    let Some(dir) = pick(self.hwnd.get()) else { return };
    let dir = Path::new(&dir);
    let dir = if dir.ends_with(&self.name) { dir.to_path_buf() } else { dir.join(&self.name) };
    // SAFETY: a NUL-terminated string.
    unsafe { SetWindowTextW(self.item(EDIT), wide(&dir.display().to_string()).as_ptr()) };
  }

  fn paint(&self) {
    let hwnd = self.hwnd.get();
    let [big, small] = self.icons.get();
    let s = |v| self.s(v);
    // SAFETY: this window's paint, between its begin and end.
    unsafe {
      let mut ps = PAINTSTRUCT::default();
      let hdc = BeginPaint(hwnd, &mut ps);
      let white = GetStockObject(WHITE_BRUSH) as HBRUSH;
      let fill = |r: [i32; 4], b| {
        FillRect(hdc, &RECT { left: r[0], top: r[1], right: r[2], bottom: r[3] }, b)
      };
      if matches!(self.page.get(), Page::Welcome | Page::Finish) {
        fill([0, 0, s(BANNER), s(FOOT)], self.banner);
        fill([s(BANNER), 0, s(W), s(FOOT)], white);
        DrawIconEx(hdc, s(34), s(40), big, s(96), s(96), 0, null_mut(), DI_NORMAL);
      } else {
        fill([0, 0, s(W), s(HEADER)], white);
        DrawIconEx(hdc, s(W - 48), s(12), small, s(32), s(32), 0, null_mut(), DI_NORMAL);
      }

      EndPaint(hwnd, &ps);
    }
  }
}

/// The window's procedure; its [Wizard] in GWLP_USERDATA from WM_NCCREATE on.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
  // SAFETY: the Wizard `open` passed, alive while its window is.
  unsafe {
    if msg == WM_NCCREATE {
      let cs = &*(l as *const CREATESTRUCTW);
      SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
    }

    let me = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Wizard;
    let Some(me) = me.as_ref() else { return DefWindowProcW(hwnd, msg, w, l) };
    match msg {
      WM_COMMAND => match (w & 0xFFFF) as i32 {
        IDOK => me.next(),
        IDCANCEL => me.cancel(),
        BACK => {
          me.save();
          me.go(Page::Welcome);
        }
        BROWSE => me.browse(),
        EDIT if (w >> 16) as u32 == EN_CHANGE && me.page.get() == Page::Folder => {
          SetTimer(hwnd, SPACE, 500, None);
        }
        _ => {}
      },
      WM_CLOSE => me.cancel(),
      WM_TIMER if w == SPACE => {
        KillTimer(hwnd, SPACE);
        me.free();
      }
      WM_TIMER => me.tick(),
      WM_PAINT => me.paint(),
      WM_CTLCOLORSTATIC if GetDlgCtrlID(l as HWND) >= WHITE => {
        SetBkColor(w as HDC, 0x00FF_FFFF);
        return GetStockObject(WHITE_BRUSH) as LRESULT;
      }
      WM_DPICHANGED => {
        let r = &*(l as *const RECT);
        me.save();
        me.scale((w >> 16) as u32 & 0xFFFF);
        let (cx, cy) = me.outer();
        SetWindowPos(hwnd, null_mut(), r.left, r.top, cx, cy, SWP_NOZORDER | SWP_NOACTIVATE);
        me.build();
      }
      WM_DESTROY => PostQuitMessage(0),
      _ => return DefWindowProcW(hwnd, msg, w, l),
    }
  }

  0
}

/// comctl32 6's controls, by the manifest; loaded only here, so `/S` never loads it.
fn common_controls() {
  type Init = unsafe extern "system" fn(*const INITCOMMONCONTROLSEX) -> BOOL;
  let lib = wide("comctl32.dll");
  // SAFETY: a NUL-terminated name, the library kept loaded; the export of its documented type.
  unsafe {
    let lib = LoadLibraryW(lib.as_ptr());
    let Some(f) = GetProcAddress(lib, c"InitCommonControlsEx".as_ptr().cast()) else { return };
    let init = INITCOMMONCONTROLSEX {
      dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
      dwICC: ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES,
    };
    std::mem::transmute::<unsafe extern "system" fn() -> isize, Init>(f)(&init);
  }
}

/// A folder picked in the shell's `IFileOpenDialog`, owned by `hwnd`.
fn pick(hwnd: HWND) -> Option<String> {
  type Unary = unsafe extern "system" fn(*mut c_void) -> u32;
  type Options = unsafe extern "system" fn(*mut c_void, u32) -> HRESULT;
  type GetOptions = unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT;
  type Show = unsafe extern "system" fn(*mut c_void, HWND) -> HRESULT;
  type Out = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT;
  type Name = unsafe extern "system" fn(*mut c_void, i32, *mut PWSTR) -> HRESULT;
  const CLSID: GUID = GUID::from_u128(0xDC1C5A9C_E88A_4DDE_A5A1_60F82A20AEF7);
  const IID: GUID = GUID::from_u128(0xD57C7288_D4AD_4768_BE02_9D969532D960);
  let (mut dlg, mut item, mut path) = (null_mut(), null_mut(), null_mut());
  // SAFETY: COM on this thread, [Wizard::show]'s; IFileDialog's methods 9
  // SetOptions, 3 Show, 20 GetResult, IShellItem's 5 GetDisplayName, IUnknown's 2 Release; each
  // object released once, the path freed once read.
  unsafe {
    if CoCreateInstance(&CLSID, null_mut(), CLSCTX_INPROC_SERVER, &IID, &mut dlg) < 0 {
      return None;
    }

    let mut opts = 0;
    let picked = method::<GetOptions>(dlg, 10)(dlg, &mut opts) >= 0
      && method::<Options>(dlg, 9)(dlg, opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM) >= 0
      && method::<Show>(dlg, 3)(dlg, hwnd) >= 0
      && method::<Out>(dlg, 20)(dlg, &mut item) >= 0;
    let mut dir = None;
    if picked {
      if method::<Name>(item, 5)(item, SIGDN_FILESYSPATH, &mut path) >= 0 {
        dir = Some(pwstr(path));
        CoTaskMemFree(path as *const c_void);
      }

      method::<Unary>(item, 2)(item);
    }

    method::<Unary>(dlg, 2)(dlg);
    dir
  }
}

/// The window's DPI: its monitor's, or the system's before Windows 10 1607.
fn dpi(hwnd: HWND) -> u32 {
  type Dpi = unsafe extern "system" fn(HWND) -> u32;
  // SAFETY: the export of its documented type; the screen's DC, released.
  unsafe {
    if let Some(dpi) = user32::<Dpi>(c"GetDpiForWindow") {
      return dpi(hwnd);
    }

    let dc = GetDC(null_mut());
    let dpi = GetDeviceCaps(dc, LOGPIXELSY as i32);
    ReleaseDC(null_mut(), dc);
    dpi as u32
  }
}

/// The text of the control `h`.
fn text(h: HWND) -> String {
  // SAFETY: a buffer of the text's length and its NUL.
  unsafe {
    let mut buf = vec![0u16; GetWindowTextLengthW(h) as usize + 1];
    let len = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
  }
}

/// An error's message box, owned by the wizard.
fn message(hwnd: HWND, title: &str, text: &str) {
  let (text, title) = (wide(text), wide(title));
  // SAFETY: NUL-terminated strings.
  unsafe { MessageBoxW(hwnd, text.as_ptr(), title.as_ptr(), MB_ICONERROR | MB_OK) };
}
