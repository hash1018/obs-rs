//! Stamps the Windows executable with an icon and its version information.
//!
//! Without this the application is a generic Windows icon in the task bar and
//! a file with no version, product name or copyright in its properties — which
//! is what a first release looked like, and the first thing anybody noticed
//! about it.
//!
//! Nothing here runs anywhere else. `CARGO_CFG_TARGET_OS` rather than
//! `cfg!(target_os)`: this file is compiled for the *host*, so the `cfg!`
//! would answer for the machine doing the building rather than the machine
//! being built for.

fn main() {
    println!("cargo:rerun-if-changed=assets/obs-rs.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/obs-rs.ico");
    // `FileVersion` and `ProductVersion` come from `CARGO_PKG_VERSION` on
    // their own; these are the fields that would otherwise be blank or say
    // "obs-rs" twice.
    resource.set("ProductName", "obs-rs");
    resource.set(
        "FileDescription",
        "obs-rs — scene compositor and screen recorder",
    );
    resource.set("LegalCopyright", "Copyright (c) 2026 Seungho Ha");

    if let Err(error) = resource.compile() {
        // Loud rather than skipped. An executable that quietly lost its icon
        // is exactly the failure this file exists to prevent, and it would
        // only be noticed once an archive was already downloaded.
        panic!("could not compile the Windows resources: {error}");
    }
}
