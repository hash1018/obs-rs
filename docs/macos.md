# Porting obs-rs to macOS

Where macOS stands, what the port has to add, and in what order. Written
from a read of the tree at `d3464e2` on a Linux machine; nothing here has
been built on a Mac yet. Correct it as the first build does.

Most of the work is in media-pp, which has no capture, audio output or GPU
backend for macOS: see `docs/macos.md` in that repository, which also has
the machine setup — Xcode Command Line Tools, Homebrew, Rust with CI's
toolchain, and an FFmpeg 8. What follows is this application's half.

## Where macOS stands: it does not build

`AGENTS.md` says a platform without a backend gets `engine/backend/unsupported.rs`
and compiles. That stopped being true as the other two backends grew. By
reading, a macOS build fails on:

1. **`src/engine/preview/mod.rs:25-27, 36`** — `mod platform;` has `path`s for
   Linux and Windows only, and `PreviewRenderer`/`PreviewSurface` are
   re-exported unconditionally.
2. **`src/engine/output/mod.rs:53-57`** — the same: `mod platform;` and
   `PreparedOutput`.
3. **`src/engine/source/display_capture/mod.rs:18-22`** — `mod platform;` and
   `open`.
4. **`src/engine/source/window_capture/mod.rs:25-34`** — `mod platform;` and
   `open`.
5. **`src/engine/source/filters.rs:168, 203`** — the `backend` alias the
   filter rack is built on exists for Windows and Linux only.
6. **`src/engine/source/pushed.rs:25`** — imports `filled_rack`, which
   `source/mod.rs:96-127` defines for Windows and Linux only.
7. **`src/engine/audio/filters.rs:37-41`** — uses `NoiseSuppressor`, which
   media-pp exports only with `rnnoise`; macOS's media-pp dependency
   (`Cargo.toml:94-99`) has no features at all.
8. **`src/engine/backend/unsupported.rs`** no longer matches what the engine
   calls — last changed before screenshots and nested Scenes (see below).
9. **Dead code under `-D warnings`.** Helpers only the platform `open`
   functions use become unused: `software_codec` and `PROBE_FPS` in
   `backend/mod.rs`; `new_rack`, `chroma_key_options`, `video_effect` in
   `source/filters.rs`; the private helpers of the text, color, drawing,
   image, rtsp, media_file and browser Sources.

More may turn up once these are fixed. Making the tree build on macOS with
the unsupported backend is the first step of the port, and keeping it
building is what a macOS CI job is for.

## Setting up

The machine setup is in media-pp's `docs/macos.md`. For this repository:

- **Build without the browser engine**: `cargo build --no-default-features`.
  `cef` is a dependency on Windows and Linux only, so `browser` does nothing
  on a Mac yet anyway, and the build is much smaller without it.
- **media-pp from the sibling checkout while both change.** `Cargo.toml`'s
  `[patch.crates-io]` pins media-pp to a git revision. For a change made in
  both at once, point it at the checkout for the duration —

  ```toml
  [patch.crates-io]
  media-pp = { path = "../media-pp/lib" }
  ```

  — and never commit that: push media-pp first, then move the pin to the
  pushed revision, changing only the media-pp lines of `Cargo.lock`.
- **Permissions.** Screen recording, the microphone and the camera are
  granted per application, in System Settings → Privacy & Security. Run
  from a terminal, the grant goes to the terminal (or the editor that
  started it), and a capture that was refused hands over black frames or
  nothing rather than an error — check the setting before debugging the
  code. A signed `.app` with its own identity comes later (see Packaging).
- **The data directory** is `~/Library/Application Support/obs-rs`, and
  recordings go to `~/Movies`, as `src/paths.rs` already says. Unlike Linux
  there is no `XDG_*` variable to point a test run elsewhere, so a test run
  uses the real project; copy it aside first if it matters.

## Step 1: build on macOS with the unsupported backend

- Give each `mod platform;` above a macOS arm — an `unsupported.rs` beside it,
  or a real `macos.rs` once there is one — and give `filled_rack`, the filter
  rack's `backend`, and each `pub use platform::…` the same.
- Add `rnnoise` to the media-pp features macOS gets. It is pure Rust.
- Bring `unsupported.rs` up to what the engine calls (next section).
- Silence the dead helpers the way the tree already does it:
  `#[cfg_attr(not(any(target_os = "linux", target_os = "windows")), allow(dead_code))]`
  — and remove those allowances again as the macOS backend starts using them.
- Fix `paths::show_in_file_manager`, which runs `xdg-open` everywhere but
  Windows; macOS has `open`.

The app then starts with no Preview and no engine (`Backend::start`
returns `Err`, so `app.rs` keeps `engine: None`), which is the baseline to
build on.

## The backend contract, as the engine calls it

`src/engine/backend/mod.rs:27-57` documents what a backend provides; the
engine calls more than that. The Linux backend (`engine/backend/linux/`,
`engine/output/linux.rs`) and the Windows one (`engine/backend/d3d11/`,
`engine/output/windows.rs`) both provide all of it.

- **`Backend`**
  - `start(render_state, size, fps, preview_fps, on_frame, meter_wake)` —
    registers one egui texture once, calls `on_frame` for every composited
    frame, passes the texture id only when the frame was drawn.
  - `set_preview_visible`, `stop`, `frame_rate() -> u32`,
    `set_frame_rate(u32) -> bool`.
  - `open_source(item, layer, fps, mixer, into: Target) -> OpenOutcome`,
    `remove_source(name)`.
  - Output, in `output/<os>.rs`: `available_encoders()`,
    `prepare_output(kind, fps, &OutputEncoding) -> PreparedOutput`,
    `attach_output(kind, PreparedOutput, sink) -> VideoTrack`,
    `detach_output(VideoTrack)`, `attach_screenshot(sink) -> BranchId`,
    `screenshot_picture(frame, ChainFormat, sink) -> Arc<Pipeline>`,
    `detach_screenshot(BranchId)`.
  - Fields read directly: `preview: Arc<Pipeline>` (stats, bus) and
    `size: [u32; 2]`.
  - `Send + Sync` (tests at `backend/mod.rs:175-198`).
- **`Layer`**: `set_layer`, `set_visible`, `set_opacity(f32)`,
  `latest_frame() -> Option<LayerFrame>`.
- **`RunningSource`**: `pause`, `resume`, `stop`, `ended`,
  `stats() -> Option<PipelineStats>`.
- **`PreparedOutput`**: `parameters()`, `time_base()`.

`unsupported.rs` lacks `Layer::set_opacity` and `latest_frame`,
`RunningSource::stats`, the `preview` and `size` fields, `open_source`'s
`into: Target`, and the screenshot methods; its `prepare_output` and
`attach_output` take the old arguments; and it defines the output methods
itself where the real backends put them in `output/<os>.rs`, so a macOS
`output` module and it would clash.

The per-kind Sources are opened by the backend: `text`, `color`, `drawing`,
`image` (through `pushed::wire`), `rtsp`, `media_file`, `browser`, the
captures, and nested Scenes (`scene/<os>.rs`, with `SceneRegistry` and
`key`). Each has a Windows and a Linux `open`; macOS adds a third with the
same arguments.

## The Preview

The Preview draws the composited frame through the wgpu device eframe
made, without a copy through system memory where the platform allows:

- **Windows**: wgpu is pinned to DX12 (`main.rs:222-227`). The compositor's
  BGRA D3D11 texture is shared with an NT handle and a keyed mutex, opened on
  wgpu's D3D12 device and wrapped with `create_texture_from_hal`
  (`preview/windows.rs`); each frame is a `CopyResource`.
- **Linux**: wgpu is pinned to Vulkan with external memory
  (`main.rs:244-265`). On CUDA, an exportable Vulkan buffer is imported into
  CUDA and each NV12 frame copied into it, then a WGSL pass resolves NV12 to
  RGBA (`preview/linux.rs`, `nv12.rs`). On Vulkan the frame is read back and
  written, which is also the fallback shape a first macOS Preview can take.
- **macOS**: wgpu's default backend there is Metal, and nothing pins or
  requests anything (`main.rs:229-230, 267-268`). The native route is the
  compositor's `IOSurface`, wrapped as a Metal texture on wgpu's own
  `MTLDevice` (`wgpu::hal::api::Metal`, `create_texture_from_hal`) — the
  same idea as Windows' shared texture, with no copy at all. A readback
  through system memory, as Linux's Vulkan path does, is the first thing to
  get working. Either fills the same `PreviewSurface`/`PreviewRenderer`
  shape.

## Capture, and what the user picks

`src/capture/mod.rs` already falls back for every function on an OS it does
not know, and `source_picker()` answers `SourcePicker::SystemDialog` there.
Its own docs describe macOS as enumerable through `SCShareableContent` once
permission is granted, which is the better answer: a `src/capture/macos.rs`
that enumerates displays, windows, audio devices and processes, cameras and
their modes, and watches audio devices — as `windows.rs` and `linux.rs` do.

Until then the `SystemDialog` answer has knock-on effects to know about:
"Add Display Capture" asks for `OpenSystemDisplayPicker`, which only Linux
handles (`app.rs:776-783`), so the button does nothing; "Add Window Capture"
adds a Wayland-shaped `Portal { restore_token: None }` target; and the
projector's screen list and the Properties dock's desktop picture are empty.
With enumeration, the existing `DisplayCaptureTarget::MonitorName` and
`WindowCaptureTarget::Window { process, title }` fit macOS and need no new
stored shapes.

## The rest, piece by piece

- **Audio** (`engine/audio/device.rs`): `open_capture` and `open_renderer`
  fall back to an error on macOS, so the mixer runs with no device. Needs
  media-pp's Core Audio capture and renderer.
- **Recording encoders** (`settings.rs:490-548`): `RecordingEncoder` has no
  VideoToolbox entry. Adding one touches `ALL`, `label`, and
  `software_codec`'s exhaustive match in `backend/mod.rs:127-137`.
- **Disk space** (`engine/output/disk.rs:79-86`): falls back to unknown;
  `statvfs` works on macOS as on Linux, with `libc` as a macOS dependency.
- **Resource usage** (`src/resources`): CPU, GPU and memory read blank. Needs
  a sampler (`proc_pid_rusage` / `task_info` for the process; GPU through
  IOKit is harder).
- **Single instance** (`src/instance`): the unsupported fallback takes no
  lock and cannot raise the running window. `flock` works on macOS (with
  `libc`); raising is `NSRunningApplication` activation.
- **Global hotkeys** (`hotkey/global.rs`): none on macOS. A
  `CGEventTap`-based keyboard can implement the same polling
  `tracker::Keyboard` trait Windows uses, and needs the Accessibility
  (input monitoring) permission.
- **The application's own shortcuts answer Control, not Command**
  (`ui/shell/hotkeys.rs:355`). Ctrl+R, Ctrl+P, Ctrl+1…9, Ctrl+, and
  Ctrl+Z/Y are matched with `matches_exact(Modifiers::CTRL)`. On macOS
  egui reports Command as `command` and `mac_cmd`, never as `ctrl`, so
  ⌘R does nothing and only the physical Control key works — and the
  Settings page and the menus say "Ctrl". What a Mac user expects is
  Command: match `Modifiers::COMMAND` there (Ctrl elsewhere, ⌘ on macOS),
  and show ⌘ in `Chord`'s label (`hotkey.rs:105`). Bindings the user sets
  already take Command as Ctrl (`Chord::from_press`, `hotkey.rs:77`), so a
  stored binding means the same key on both.
- **The menu bar** is drawn inside the window by egui (File, Edit, View),
  as on the other platforms; the macOS menu bar at the top of the screen
  shows only the application menu winit gives every app. That works, but a
  native menu there (for example through `muda`) is what makes it look
  like a Mac application — a later nicety, not part of making it run.
- **Browser Source** (`src/browser`): macOS gets `absent.rs`, which works.
  CEF on macOS needs an `.app` bundle with its helper apps and framework in
  place — the largest single piece, and the one to leave for last. Touches
  the `cef` target gate, `browser/mod.rs:35, 39`, `paths.rs` (cache dir),
  `build.rs`, `cef.rs`'s paint arms, and `engine/mod.rs:2893` (premultiplied
  alpha is decided per platform).
- **Fonts** (`i18n/font.rs`): already has macOS's
  `/System/Library/Fonts/AppleSDGothicNeo.ttc`, which is also the Text
  Source's default.

## Packaging, later

- An `.app` bundle with an `Info.plist` naming the bundle identifier and the
  usage strings the system shows when asking for permission
  (`NSCameraUsageDescription`, `NSMicrophoneUsageDescription`), so the
  permissions belong to obs-rs rather than to a terminal.
- FFmpeg inside the bundle, with the dylibs' install names rewritten to
  `@rpath` or `@executable_path` — the counterpart of the Linux archive's
  `$ORIGIN/lib`.
- Signing and notarization, without which Gatekeeper refuses a downloaded
  app. That needs a paid Apple Developer account; an ad-hoc signature is
  enough on the machine that built it.
- A macOS job in `release.yml`, and the `setup-ffmpeg` action taught an
  `arm64-osx-dynamic` triplet with the same LGPL feature set the other
  archives ship.

## A suggested order

1. Step 1 above: build on macOS with the unsupported backend, and add a
   macOS CI job that keeps it building (`macos-latest` runners are Apple
   silicon).
2. `src/capture/macos.rs` enumeration, `show_in_file_manager`, single
   instance, disk space — small, and independent of media-pp.
3. With media-pp's Core Audio pieces: the mixer's devices.
4. With media-pp's system-memory ScreenCaptureKit and camera sources: a
   first backend on the software compositor, and a readback Preview. Slow,
   but everything above the backend gets exercised.
5. With media-pp's Metal backend: the real backend, the `IOSurface` Preview,
   VideoToolbox recording.
6. Hotkeys, resource usage, and the browser engine; then packaging.
