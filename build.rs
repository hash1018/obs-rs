//! Stamps the Windows executable with an icon and its version information.
//!
//! Without this the application is a generic Windows icon in the task bar and
//! a file with no version, product name or copyright in its properties — which
//! is what a first release looked like, and the first thing anybody noticed
//! about it.
//!
//! # Two gates, not one, and they are asking different questions
//!
//! `#[cfg(windows)]` is about the machine doing the building. A build script
//! is compiled for the *host*, and `winresource` is a
//! `[target.'cfg(target_os = "windows")'.build-dependencies]` entry, which
//! Cargo resolves against the host too — so on Linux the crate is simply not
//! there and naming it at all fails to compile. An early `return` does not
//! help: the path still has to resolve.
//!
//! `CARGO_CFG_TARGET_OS` is about the machine being built *for*, and it is
//! what makes a Windows host cross-compiling to Linux skip this rather than
//! attach Windows resources to an ELF binary.
//!
//! The first version of this file had only the second, which reads correctly
//! and broke the Linux build.

fn main() {
    println!("cargo:rerun-if-changed=assets/obs-rs.ico");

    #[cfg(windows)]
    stamp_windows_resources();
}

#[cfg(windows)]
fn stamp_windows_resources() {
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
