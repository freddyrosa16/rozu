//! The rozu desktop app: a sidebar of threads and a centered chat, drawn with GPUI
//! (`docs/design/app/index.html` is the spec). It drives rozu-core the way `rozu chat` does.

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod backend;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod feed;

#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod composer;
#[cfg(target_os = "macos")]
mod orb;
#[cfg(target_os = "macos")]
mod theme;

#[cfg(target_os = "macos")]
fn main() {
    app::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("rozu-app runs on macOS only for now; use `rozu chat`.");
    std::process::exit(1);
}
