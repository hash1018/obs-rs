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
                // Linux: no display server at all. A page here is drawn
                // off-screen and handed over as pixels, so Chromium has no
                // window to put anywhere, and left to itself it connects to
                // the session's Wayland or X11 server anyway — measured, on
                // Wayland, with a GTK complaint and a GPU process reporting
                // that platform incompatible with Vulkan. Headless draws the
                // same pictures at the same cost with neither, and does not
                // care which kind of session obs-rs itself is running in.
                #[cfg(target_os = "linux")]
                command_line.append_switch_with_value(
                    Some(&CefString::from("ozone-platform")),
                    Some(&CefString::from("headless")),
                );
            }
        }
    }
}
