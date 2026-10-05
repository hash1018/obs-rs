# Getting obs-rs

Running a released build. Building it yourself is in
[building.md](building.md).

A release carries one archive per platform. Unpack it and run what is inside;
there is no installer, and nothing is written outside the folder you unpack it
into except the settings and project files under your own user directory.

FFmpeg travels with the archive, so it does not have to be installed. What
does have to be there:

**Windows**

- Nothing else. The Visual C++ runtime the executable needs is in the archive
  beside it.
- The executable is not code-signed, so SmartScreen will offer to protect you
  from it the first time. *More info → Run anyway*.
- The virtual camera (Windows 11) is the one thing installed outside the
  folder: Windows loads it into a service of its own, which reads only what
  is registered for the whole computer. The first **Start Virtual Camera**
  finds it missing and offers **Install Virtual Camera**, which asks for
  administrator permission once and copies it to
  `Program Files\media-pp\vcam`. To remove it, run
  `vcam\install.ps1 -Uninstall` from the archive in an administrator
  PowerShell.

**Linux**

- **A GPU with a Vulkan driver** — Mesa's, for AMD and Intel — **or
  NVIDIA's driver.** See [how each platform composites](building.md#how-each-platform-composites). Neither is in the
  archive: both belong to the system.
- **PipeWire.** `libpipewire-0.3.so.0` is a client for a system service, so a
  bundled copy would only be a version to disagree with the one running.
  Every current desktop distribution has it.
- A glibc no older than the one the archive was built against, which is the
  current Ubuntu LTS. An older distribution needs a build from source.
- For the virtual camera, the **v4l2loopback** kernel module — on Debian and
  Ubuntu `sudo apt install v4l2loopback-dkms`; most distributions package
  it under that name or `v4l2loopback`. Installing the package is all: the
  first **Start Virtual Camera** finds the module not loaded and offers
  **Install Virtual Camera**, which loads it labelled "obs-rs" through the
  desktop's administrator prompt. It stays loaded until the next restart;
  `sudo modprobe -r v4l2loopback` unloads it sooner. A module loaded by hand
  is used as it is, though browsers list it only if it was loaded with
  `exclusive_caps=1`. With Secure Boot on, the package's module must be
  signed with a key the firmware trusts — the package's own install says
  how.

Run `./obs-rs` from the unpacked directory. It finds its own copy of FFmpeg
beside it and needs no `LD_LIBRARY_PATH`.

For a name and an icon in the launcher, run `./install-desktop-entry.sh`. It
writes a desktop entry and an icon under `~/.local/share` and needs no root.
On Wayland this is the only way the window gets an icon at all — the protocol
has no call for a client to set its own, so the compositor looks for an
installed entry instead. Moving the folder afterwards breaks the entry, since
it records where the executable is; run the script again, or
`--uninstall` to remove it.

**macOS**

- A Mac with Apple silicon, on macOS 12.3 or newer; capturing what other
  applications play needs 14.2.
- Move `obs-rs.app` to Applications, or run it where it is. It carries its
  own FFmpeg.
- The app is signed ad hoc rather than by a registered developer, so the
  first time macOS says it cannot check it for malicious software. Open
  **System Settings → Privacy & Security** and choose *Open Anyway* beside
  obs-rs, once.
- macOS asks for each permission the first time it is needed, under
  obs-rs's own name: screen recording for a Display or Window Capture, the
  camera, the microphone, and Input Monitoring for a hotkey that works while
  another application has focus.
