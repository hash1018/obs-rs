//! What the helper applications inside a macOS bundle run: Chromium's
//! render, GPU and utility processes, and nothing of obs-rs.
//!
//! On Windows and Linux those processes are obs-rs itself started again,
//! which tells from its arguments what it is (`browser::helper_process`). A
//! Mac bundle keeps them as applications of their own — five of them — and
//! an executable in each that is all of obs-rs is five copies of FFmpeg's
//! users, the compositor and the rest, for a process that only ever calls
//! into CEF. This is that call, and a fraction of the size.
//!
//! It starts Chromium's processes with the same switches obs-rs does
//! (`browser/page_app.rs`), and loads the framework from where a helper is
//! in the bundle, three directories in from `Contents/Frameworks`. The
//! development bundle `cargo run` makes still runs obs-rs itself as each
//! helper, since this is built only when asked for — see `make-app.sh`.
//!
//! Anywhere but macOS, or without the browser engine, there is nothing for
//! it to do.

#[cfg(target_os = "macos")]
fn main() {
    helper::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("obs-rs-helper is the browser engine's helper on macOS, and nothing elsewhere");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
#[path = "../browser/page_app.rs"]
mod page_app;

#[cfg(target_os = "macos")]
mod helper {
    use cef::args::Args;
    use cef::library_loader::LibraryLoader;
    use cef::{api_hash, execute_process, sys};

    use super::page_app::PageApp;

    pub(super) fn run() {
        let Ok(executable) = std::env::current_exe() else {
            std::process::exit(1);
        };
        // Loaded for the life of the process, which `exit` ends without
        // dropping it: unloading the framework under a running process is
        // not a thing to do.
        let loader = LibraryLoader::new(&executable, true);
        if !loader.load() {
            eprintln!("could not load the browser engine's framework");
            std::process::exit(1);
        }
        let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
        let args = Args::new();
        let mut app = PageApp::new();
        let code = execute_process(
            Some(args.as_main_args()),
            Some(&mut app),
            std::ptr::null_mut(),
        );
        // Negative means this was not started as one of Chromium's
        // processes, which is the only thing it can be for.
        std::process::exit(if code < 0 { 1 } else { code });
    }
}
