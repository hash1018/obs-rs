//! What this application is, as far as Chromium is concerned. One thing
//! only: the switches its processes start with.
//!
//! A file of its own because two executables apply it: obs-rs, whose
//! processes are all itself on Windows and Linux, and on macOS the helper
//! executable the bundle's helper applications run (`src/bin/obs-rs-helper.rs`).
//! A child started with other switches than the browser process's is a
//! child enforcing another policy.
//!
//! A page in a Source has nobody to click it, and Chromium will not let a
//! page play sound — or an autoplaying video start — until someone has. That
//! rule is for a browser somebody is browsing with; here it would mean a
//! Source that is silent until a click that can never happen. OBS turns it
//! off for the same reason.
//!
//! Appended for every process type, including the renderers this is called
//! again for as they are launched: the policy is enforced where the page
//! runs.

use cef::*;

// The macro writes the struct, so it takes neither doc comments nor derives.
wrap_app! {
    pub struct PageApp;

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(command_line) = command_line {
                command_line.append_switch_with_value(
                    Some(&CefString::from("autoplay-policy")),
                    Some(&CefString::from("no-user-gesture-required")),
                );
                // Chromium's first-run flow — the profile's welcome and
                // default-browser steps — runs inside `initialize` the first
                // time a profile is used, and nobody is there to finish it:
                // measured on Linux, a fresh profile held `initialize` for as
                // long as it was waited on, and every run after the one that
                // left a `First Run` marker took a tenth of a second.
                command_line.append_switch(Some(&CefString::from("no-first-run")));
                // Linux: a page's cookies are sealed with a key Chromium
                // otherwise keeps in the desktop's keyring, and a keyring
                // that is locked asks to be unlocked — in a dialog of its
                // own, over whatever is being recorded, the first time a page
                // stores something. The basic store keeps the key with the
                // profile instead, which is where everything else a Source's
                // page keeps already is.
                #[cfg(target_os = "linux")]
                command_line.append_switch_with_value(
                    Some(&CefString::from("password-store")),
                    Some(&CefString::from("basic")),
                );
                // macOS: the same key, which Chromium otherwise keeps in the
                // login keychain — and asking for it there is the system's
                // own dialog asking to let obs-rs use "Chromium Safe
                // Storage", over whatever is being recorded. The mock
                // keychain keeps it with the profile, as the basic store
                // does on Linux.
                #[cfg(target_os = "macos")]
                command_line.append_switch(Some(&CefString::from("use-mock-keychain")));
                // Linux: which display platform Chromium runs on decides how
                // a page's pictures can arrive — see `pictures_on_gpu`.
                // Without GPU pictures, no display server at all: a page
                // drawn as pixels needs none, and left to itself Chromium
                // connects to the session's anyway — measured, on Wayland,
                // with a GTK complaint and a GPU process reporting that
                // platform incompatible with Vulkan.
                #[cfg(target_os = "linux")]
                command_line.append_switch_with_value(
                    Some(&CefString::from("ozone-platform")),
                    Some(&CefString::from(if pictures_on_gpu() {
                        "wayland"
                    } else {
                        "headless"
                    })),
                );
            }
        }
    }
}

/// Whether a Linux page hands its pictures over as GPU buffers rather than
/// as pixels: on a Wayland session alone.
///
/// Measured with CEF 156 on an NVIDIA GPU (595.91.07), a page off-screen
/// on Chromium's Wayland platform hands over DMA-BUFs in NVIDIA's own tiled
/// layout, holding the page. Its X11 platform, with GL or Vulkan, and
/// headless hand over no GPU buffer at all, or ones nothing was drawn into.
/// `OBS_RS_BROWSER_GPU=0` keeps the pixels regardless.
///
/// The same answer in every process, which each reaches by itself from the
/// same environment: a child started on another platform than the browser
/// process's could not share its buffers.
#[cfg(target_os = "linux")]
pub fn pictures_on_gpu() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|display| !display.is_empty())
        && std::env::var("OBS_RS_BROWSER_GPU").as_deref() != Ok("0")
}
