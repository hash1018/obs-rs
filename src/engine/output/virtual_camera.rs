//! Installing the virtual camera from inside obs-rs.
//!
//! The camera is media-pp's: a Media Foundation media source in a DLL of its
//! own, which Windows loads into its Frame Server service. That service reads
//! only what is registered for the whole machine, so the DLL has to be copied
//! where it can read it and registered there — both administrator's work.
//! The Windows archive carries the DLL and media-pp's `install.ps1` in
//! `vcam\` beside the executable; this runs that script elevated, which is
//! Windows' own consent prompt, and says how it came out.

use std::fmt;

/// What starting the camera answers where the DLL is not registered: the
/// one failure that installing mends, and so the one the Controls dock
/// offers to install for.
#[derive(Debug)]
pub(in crate::engine) struct NotInstalled;

impl fmt::Display for NotInstalled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("it is not installed on this computer")
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

#[cfg(not(target_os = "windows"))]
mod platform {
    pub(super) fn run() -> Result<(), String> {
        Err("the virtual camera is available only on Windows".to_owned())
    }
}
