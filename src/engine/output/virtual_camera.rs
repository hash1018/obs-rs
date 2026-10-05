//! Installing the virtual camera from inside obs-rs.
//!
//! On Windows the camera is media-pp's: a Media Foundation media source in a
//! DLL of its own, which Windows loads into its Frame Server service. That
//! service reads only what is registered for the whole machine, so the DLL
//! has to be copied where it can read it and registered there — both
//! administrator's work. The Windows archive carries the DLL and media-pp's
//! `install.ps1` in `vcam\` beside the executable; this runs that script
//! elevated, which is Windows' own consent prompt, and says how it came out.
//!
//! On Linux the camera is a v4l2loopback device, which the kernel module of
//! that name makes. The module is the distribution's to ship and loading it
//! is root's, so installing here is loading it — through `pkexec`, the
//! desktop's own administrator prompt — labelled "obs-rs" and with
//! `exclusive_caps=1`, without which browsers do not list it. A machine
//! without the module's package installed is told which one to install.

use std::fmt;

/// What starting the camera answers where it is not there to start — the
/// DLL not registered, or the loopback module not loaded: the one failure
/// that installing mends, and so the one the Controls dock offers to
/// install for.
#[derive(Debug)]
pub(in crate::engine) struct NotInstalled;

impl fmt::Display for NotInstalled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if cfg!(target_os = "linux") {
            f.write_str("the v4l2loopback module is not loaded")
        } else {
            f.write_str("it is not installed on this computer")
        }
    }
}

impl std::error::Error for NotInstalled {}

/// Runs the installer on a thread of its own and hands `done` what came of
/// it: the consent prompt and the install take as long as the person and
/// the service do, and the engine's thread must not wait on either.
pub(in crate::engine) fn install(done: impl FnOnce(Result<(), String>) + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name("virtual-camera-install".to_owned())
        .spawn(move || done(platform::run()));
    if let Err(error) = spawned {
        tracing::error!("could not start installing the virtual camera: {error}");
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::path::PathBuf;

    use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED};
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject};
    use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::{HRESULT, HSTRING, w};

    /// Where the archive puts the installer and the DLL: `vcam\` beside the
    /// executable.
    fn files() -> Result<(PathBuf, PathBuf), String> {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let dir = exe
            .parent()
            .ok_or("the executable has no folder")?
            .join("vcam");
        let script = dir.join("install.ps1");
        let dll = dir.join("media_pp_vcam.dll");
        if !script.is_file() || !dll.is_file() {
            // A development build, run from `target`: nothing was put beside
            // it. media-pp's own script installs the camera it builds.
            return Err(format!(
                "this copy of obs-rs has no camera to install in {} — run media-pp's \
                 vcam\\install.ps1 as administrator instead",
                dir.display()
            ));
        }
        Ok((script, dll))
    }

    pub(super) fn run() -> Result<(), String> {
        let (script, dll) = files()?;
        let parameters = HSTRING::from(format!(
            "-NoProfile -ExecutionPolicy Bypass -File \"{}\" -Dll \"{}\"",
            script.display(),
            dll.display()
        ));
        // ShellExecuteEx wants COM on the thread that calls it; this thread
        // is its own, so it is set up and let go of here.
        // SAFETY: paired with the `CoUninitialize` below on this thread.
        let com =
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            // Elevated: what Windows asks consent for. The window is hidden —
            // the script says nothing worth reading, and its exit code is
            // what is reported.
            lpVerb: w!("runas"),
            lpFile: w!("powershell.exe"),
            lpParameters: windows::core::PCWSTR(parameters.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // SAFETY: `info` and the strings it points at outlive the call.
        let started = unsafe { ShellExecuteExW(&mut info) };
        let outcome = match started {
            Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => {
                Err("the administrator prompt was declined".to_owned())
            }
            Err(error) => Err(format!("the installer could not be started: {error}")),
            Ok(()) => {
                let process = info.hProcess;
                let mut code = 1u32;
                // SAFETY: `process` is the handle `SEE_MASK_NOCLOSEPROCESS`
                // asked for, waited on and closed here once.
                let read = unsafe {
                    WaitForSingleObject(process, INFINITE);
                    let read = GetExitCodeProcess(process, &mut code);
                    let _ = CloseHandle(process);
                    read
                };
                match read {
                    Err(error) => Err(error.to_string()),
                    Ok(()) if code == 0 => Ok(()),
                    Ok(()) => Err(format!("the installer failed (exit code {code})")),
                }
            }
        };
        if com.is_ok() {
            // SAFETY: balances the successful `CoInitializeEx` above.
            unsafe { CoUninitialize() };
        }
        outcome
    }
}

/// The label the module is loaded with, which every application's camera
/// list shows, and which the device to write to is picked by.
#[cfg(target_os = "linux")]
const CARD_LABEL: &str = "obs-rs";

/// The v4l2loopback device the camera writes to: the one labelled
/// [`CARD_LABEL`] where there is one, any free one otherwise — a module
/// someone loaded by hand is used as it is.
///
/// `NotInstalled` where the module is not loaded at all, which loading
/// mends; a plain error where it is loaded and every device it made is in
/// use, which loading again would not.
#[cfg(target_os = "linux")]
pub(in crate::engine) fn loopback_device()
-> Result<media_pp::elements::V4l2Device, crate::engine::backend::BackendError> {
    let mut devices = media_pp::elements::V4l2VirtualCamera::list_devices()?;
    if devices.is_empty() {
        if std::path::Path::new("/sys/module/v4l2loopback").exists() {
            return Err("every v4l2loopback device is already in use".into());
        }
        return Err(Box::new(NotInstalled));
    }
    let labelled = devices.iter().position(|device| device.name == CARD_LABEL);
    Ok(devices.swap_remove(labelled.unwrap_or(0)))
}

#[cfg(target_os = "linux")]
mod platform {
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::CARD_LABEL;

    /// How long a device just made is waited for. The kernel makes the node
    /// as the module loads, but udev gives the person at the desk access to
    /// it a moment later; a start in between finds nothing it may open.
    const NODE_WAIT: Duration = Duration::from_secs(5);

    /// `pkexec`'s answer when the person dismissed the prompt, or was not
    /// allowed: it says so rather than running anything.
    const DISMISSED: i32 = 126;
    /// `pkexec`'s answer when it could not authenticate or run the program.
    const NOT_AUTHORIZED: i32 = 127;

    /// Loads the module through the desktop's administrator prompt, and
    /// says how it came out in words the status bar can show.
    pub(super) fn run() -> Result<(), String> {
        let output = Command::new("pkexec")
            .arg(modprobe())
            .args([
                "v4l2loopback",
                "exclusive_caps=1",
                &format!("card_label={CARD_LABEL}"),
            ])
            .output()
            .map_err(|error| format!("pkexec could not be started: {error}"))?;
        match output.status.code() {
            Some(0) => {
                wait_for_node();
                Ok(())
            }
            Some(DISMISSED | NOT_AUTHORIZED) => {
                Err("the administrator prompt was declined".to_owned())
            }
            _ => Err(failure(&String::from_utf8_lossy(&output.stderr))),
        }
    }

    /// Waits, up to [`NODE_WAIT`], until a loopback device can be opened,
    /// so the start that follows the install finds one. Gives up quietly:
    /// that start then says what is wrong.
    fn wait_for_node() {
        let deadline = Instant::now() + NODE_WAIT;
        while Instant::now() < deadline {
            let found = media_pp::elements::V4l2VirtualCamera::list_devices()
                .is_ok_and(|devices| !devices.is_empty());
            if found {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        tracing::warn!("no v4l2loopback device could be opened after loading the module");
    }

    /// `modprobe` by its full path: `pkexec` runs what it is given with a
    /// bare environment, and a user's `PATH` need not name `/usr/sbin`.
    fn modprobe() -> &'static str {
        ["/usr/sbin/modprobe", "/sbin/modprobe"]
            .into_iter()
            .find(|path| std::path::Path::new(path).exists())
            .unwrap_or("modprobe")
    }

    /// What a failed `modprobe` said, as the status bar should say it. The
    /// common failure is that the module's package is not installed, which
    /// names the package rather than repeating modprobe's own words.
    fn failure(stderr: &str) -> String {
        let said = stderr.trim();
        if said.contains("not found") {
            return "the v4l2loopback module is not installed — install your \
                    distribution's v4l2loopback package (v4l2loopback-dkms on Debian \
                    and Ubuntu) and try again"
                .to_owned();
        }
        if said.is_empty() {
            "the module could not be loaded".to_owned()
        } else {
            format!("the module could not be loaded: {said}")
        }
    }

    #[cfg(test)]
    mod tests {
        use super::failure;

        #[test]
        fn a_module_not_found_names_the_package_to_install() {
            let said = failure(
                "modprobe: FATAL: Module v4l2loopback not found in directory \
                 /lib/modules/7.0.0-34-generic\n",
            );
            assert!(said.contains("v4l2loopback-dkms"), "{said}");
        }

        #[test]
        fn any_other_failure_is_passed_on() {
            let said = failure(
                "modprobe: ERROR: could not insert 'v4l2loopback': Key was rejected by service\n",
            );
            assert!(said.contains("Key was rejected"), "{said}");
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
mod platform {
    pub(super) fn run() -> Result<(), String> {
        Err("the virtual camera is available only on Windows and Linux".to_owned())
    }
}
