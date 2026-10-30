// Copyright (c) vladkens | MIT License | https://github.com/vladkens/shipo
//! The stub's link on Windows: the DLLs its imports name loaded from System32 only, never from the
//! setup's folder (LOAD_LIBRARY_SEARCH_SYSTEM32 as /DEPENDENTLOADFLAG), so one planted beside a
//! setup in Downloads never runs.

fn main() {
  if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
    println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
  }
}
