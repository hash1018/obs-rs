# Porting obs-rs to macOS

Where macOS stands, what the port has to add, and in what order. First
written from a read of the tree at `d3464e2` on a Linux machine; corrected
as the port goes on a Mac.

`media-pp` has what a macOS backend is made of now: ScreenCaptureKit display
and window capture, AVFoundation cameras, Core Audio capture and playback,
VideoToolbox decode, encode, upload and download, and on its frames a Metal
compositor, filters (scale, convert, chroma key, effects), `MetalRenderer` for
an application's own Metal drawing and `MetalSharedTextureSource` for another
producer's `IOSurface`s. Its `docs/building/macos.md` has the machine setup —
the Xcode Command Line Tools, Rust, and FFmpeg 8.0.1. What follows is this
application's half.

## Where macOS stands: a Metal backend, with its captures

The backend is `engine/backend/macos`: the Linux backend's shape with one
GPU in it. `macos::gpu` is Linux's `Gpu`, `Compositor` and `Layer` over
VideoToolbox frames drawn with Metal — `VideoToolboxUpload` and
`VideoToolboxDownload`, `MetalConverter`, `MetalChromaKey`,
`MetalVideoEffect`, `DecodeTarget::VideoToolbox`, `MetalVideoCompositor` — so
every Source written against that interface is the same code on both: Color,
Drawing, Text, Image (`pushed::wire`), media files, streams, the filter rack,
nested Scenes (`scene/gpu.rs`, which was `scene/linux.rs`) and the Browser
Source, which has the Linux shape and, with no engine here, says so.

- **The Canvas is NV12**, BT.709 at limited range, as on Linux.
- **The Preview** (`preview/macos.rs`) is handed each frame by `media-pp`'s
  `MetalRenderer` as Metal textures over its `IOSurface`, made on wgpu's own
  device; their planes are copied with a Metal blit on wgpu's own queue into
  the two textures `nv12`'s resolve pass reads, and resolved as on Linux.
  Nothing goes through system memory. wgpu is pinned to Metal in `main.rs`.
  The planes are written through wgpu once when they are made: wgpu clears a
  texture it has never seen written before the first pass that reads it, and
  it does not see a Metal blit.
- **Recording** (`output/macos.rs`) is VideoToolbox (`RecordingEncoder::VideoToolbox`,
  new), fed the compositor's NV12 as it is, or a software encoder after a
  download; a smaller file is scaled with `MetalScaler`. Screenshots are the
  Linux twin's.
- Checked on a Mac: a Color, a Text and a looping media file composited and
  shown at 60 fps; recordings through VideoToolbox (with NVENC stored and
  falling back to it) and OpenH264, 1920x1080 at 60, BT.709 tagged; a
  screenshot. The Preview has a test that reads back what it resolved, the
  caption test has a Metal twin, and the media file tests — seeking, speed,
  playing backwards, looping — run here too.

- **Captures.** A Display Capture and a Window Capture are one
  ScreenCaptureKit stream (`source/screencapturekit_capture.rs`), handing on the
  pixel buffers it draws as BGRA VideoToolbox frames — nothing copied or
  converted before the compositor — and shared between the items showing
  one display or one window, as Windows shares its captures: a stream with a
  `Tee` that grows a branch per item. Nothing refuses a second stream here,
  so this is Windows' plain saving rather than its display's necessity, and
  none of Linux's portal reasons for not sharing apply. A display is found
  by the id its stored name carries (`Built-in Display (1)`), a window by
  its application and title as on Windows; either not there is `Absent`,
  and a stream that ends — a window closed, a display disconnected — puts
  its items back to be looked for. A camera is AVFoundation's, its pixel
  buffers handed on as NV12, and shared the same way
  (`video_capture/macos.rs`).
- **What the user picks from** (`capture/macos.rs`) is listed through Core
  Graphics rather than ScreenCaptureKit, which would ask for screen
  recording: displays with their bounds in points, and every application
  window at the normal level on any Space — its title only once screen
  recording is allowed, and one off screen only where it has a title. Cameras and their modes are `media-pp`'s.
- Checked on a Mac: a display shown twice from one stream, a window on
  another Space and the built-in camera composited beside the rest at 60
  fps. The registry's tests are the Windows display's, on the main display;
  they need screen recording and skip without it.

- **Audio** (`engine/audio/device.rs`, `capture/macos.rs`): the mixer's
  inputs and outputs are Core Audio's — an output captured through a process
  tap on it, as WASAPI loops back a render endpoint — and an application's
  sound through a tap on that process (macOS 14.2 and newer). Monitoring
  plays through `CoreAudioRenderer`. Devices are stored by their UID, and
  watched for by a poll, as on Linux. Checked on a Mac: both default
  channels open, and a recording made while a sound played carries it.

- **Smaller pieces.** Free disk space is `statfs` (`output/disk.rs`) —
  not `statvfs`, whose block counts are 32 bits on macOS. A second launch
  brings the running copy forward (`instance/macos.rs`): since macOS 14 an
  application is activated only by an active one yielding to it, so the
  launch activates itself as an accessory, with no Dock icon, and yields;
  checked with Finder in front. The application's own shortcuts are
  Command where they are Ctrl elsewhere — ⌘R, ⌘1…9, ⌘, — matched as
  Command and shown as a Mac shows them (`⌥⇧⌘F9`), while a settings file
  still says `Ctrl+R`, so it means the same keys on every platform
  (`hotkey.rs`).
- **Resource usage** (`resources/macos.rs`): CPU is `getrusage` over every
  core, as Linux reads it; memory is the physical footprint Activity
  Monitor shows, with no committed figure beside it; the GPU is this
  process's own, from the GPU time its driver clients have been charged in
  the I/O Registry, where Activity Monitor reads it — the kernel's own
  per-task GPU figure reads zero on Apple silicon. Checked on a Mac: 17% of
  the GPU for the process while the whole device read 28%.
- **Global hotkeys** (`hotkey/global.rs`): the thread Windows uses, asking
  which bound keys are down (`CGEventSourceKeyState`) a hundred times a
  second, so push-to-talk sees a key let go as well as pressed. macOS
  reads every key as up to a process not allowed Input Monitoring, so a
  hotkey is taken from the window only once it is; the system is asked the
  first time one is bound, and until it is allowed each works while obs-rs
  has focus. Key codes are places on the keyboard, named for the US
  layout's letters. Checked on a Mac: a screenshot key taken with Finder in
  front, once allowed.

Not yet: the browser engine and packaging.

What step 1 did, before there was a backend: every `mod platform;` without a
macOS arm compiles where it has one, `output` has an `unsupported.rs`, the
single-instance lock is `flock` (`instance/macos.rs`), `show_in_file_manager`
runs `open`, and CI builds, lints and tests on `macos-15` with `setup-ffmpeg`
taught `arm64-osx-dynamic`.

## Setting up

The machine setup is in media-pp's `docs/building/macos.md`. For this repository:

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

Done — see *Where macOS stands*. The existing
`DisplayCaptureTarget::MonitorName` and `WindowCaptureTarget::Window
{ process, title }` fit macOS, so no stored shape is new.

## The rest, piece by piece

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

1. ~~Build on macOS with the unsupported backend, and a macOS CI job that
   keeps it building.~~ Done — see *Where macOS stands*.
2. ~~Disk space, raising the running copy, Command shortcuts, and
   `src/capture/macos.rs`.~~ Done.
3. ~~With media-pp's Core Audio pieces: the mixer's devices.~~ Done.
4. ~~The Metal backend: `MetalVideoCompositor`, the Preview, VideoToolbox
   recording, and its captures.~~ Done.
5. ~~Resource usage and global hotkeys.~~ Done.
6. The browser engine; then packaging.
