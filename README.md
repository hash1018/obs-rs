# obs-rs

An OBS-style scene compositor, screen recorder and streamer, written in Rust
on top of [`media-pp`](https://github.com/hash1018/media-pp). Captures your
desktop, composites it on the GPU, and records it or sends it to an RTMP
service — with an annotation layer you can draw on while it runs.

![The obs-rs window: the Scenes, Sources and Properties docks down the left;
the Preview in the middle, where a still picture, a selected video — outlined
in red, its cropped left edge marked in green, and the distance to each canvas
edge written beside it — and a second video in the corner are composited
together; and the Audio Mixer and Controls along the
bottom.](docs/screenshot.png)

## What it does

- **Scenes and sources.** A Scene is a list of sources placed on a 1920×1080
  canvas. Move, resize, reorder, hide, lock and rename them. Duplicating a
  Scene places the same sources again rather than capturing them twice, so a
  source can appear in more than one Scene with its own position in each.
- **Display and window capture** on Windows and Linux. Frames go straight
  into GPU memory and stay there through compositing and encoding — the
  desktop never passes through system memory on its way to the recording.
  A captured window that is closed is not an error: the source waits where it
  is and picks the window up again when it comes back.
- **Video capture.** A webcam in a Scene, on both platforms — Media
  Foundation on Windows, Video4Linux2 on Linux. The camera states which
  picture sizes and rates it offers and the Properties dock lists them, or
  leave it on Automatic and take whichever the camera puts first. A camera
  that is unplugged is not an error: the source waits and opens it again when
  it comes back, as long as it goes into the same port — the device path a
  camera is stored by contains the one it was plugged into.
- **Drawing.** A source you draw on rather than one that captures something:
  pen, highlighter and eraser, with undo and clear. It is a real layer, so
  what you draw is in the recording, not just on your screen.
- **Image.** A still picture as a source, at its own size. Decoded once
  through the same library that opens a video, so whatever FFmpeg reads is a
  source — PNG, JPEG, WebP and the rest.
- **Media file.** A video file as a source, decoded on the GPU and composited
  like anything else, with its own channel in the audio mixer. It can be set
  to start again at its end; switching that off part way through lets the
  pass it is on play out rather than stopping where it is, and the Sources
  list says when one has finished — and play starts it again. It can be
  paused and scrubbed from the Properties dock, and seeking a paused clip
  moves the picture without starting it again.
- **Network stream.** An RTSP session in a Scene — an IP camera, usually —
  decoded on the GPU like a media file, with a channel in the mixer for its
  sound. A camera that stops answering is not an error: the stream is put
  back and tried again on its own, at an interval you choose, or left alone
  entirely if you turn that off. TCP or UDP, because no one transport gets
  through every network.
- **Browser.** A web page as a source, on Windows and Linux: an overlay, an
  alert box, a chat window. It is rendered off-screen by Chromium; on
  Windows it is handed over as a GPU texture, so the page reaches the
  compositor without a copy through system memory, and on Linux as its
  pixels, which are copied up. Either way its transparency is real
  transparency, not a black rectangle. Address, size and frame rate are set in the Properties dock.
  Whatever the page plays gets a channel in the audio mixer, like a media
  file's sound: it is recorded, and heard as well if you switch monitoring
  on. Select the source and the Preview's toolbar grows an **Interact**
  switch: with it on, your clicks, scrolling and typing go to the page
  instead of moving the layer, which is how a page that has to be logged
  into or scrolled gets handled. Switch it off to go back to dragging the
  source around. A page nothing is showing stops drawing but stays where it
  was, so it comes back as it was; **Shut down when hidden** gives that up
  and closes the browser instead, for a heavy page in a Scene you are rarely
  on. **Refresh** loads the page again there and then, ignoring whatever was
  cached for it — what an overlay you have just edited on disk needs — and
  **Refresh when shown** does that every time the source comes back into
  view.
- **Transitions.** Switching Scenes can be nothing at all — the default, and
  what it has always been — or a fade, or a fade through black, with the
  length in milliseconds. Under the Scenes list, because it is one setting
  for the whole project. A fade brings the arriving Scene up over the one
  being left, which is exactly a dissolve wherever the arriving Scene covers
  the canvas; where it does not, fading through black is the one that is
  right. Switching again part way through ends the switch that was running
  and starts the new one.
- **A Scene inside a Scene.** Any Scene can be placed in another as a
  source: an overlay with your logo, a watermark and an alert box, kept in
  one Scene and shown in every Scene that needs it. Editing it
  once changes it everywhere. It arrives as one picture, so it is moved,
  scaled, cropped and filtered as one thing — and it is transparent where it
  draws nothing, so what is under it still shows. A Scene cannot be put
  inside itself, directly or round a loop, and deleting one that others show
  says where it is used before it goes.
- **Crop.** Alt-drag a source's handle to cut into its picture instead of
  resizing it: the dragged edge moves, the opposite one stays, and what is
  being cut off shows faintly behind while you aim. Alt+double-click puts an
  edge back, and the Properties dock takes the four numbers exactly.
- **Projector.** The Canvas on another screen, from `View → Projector`:
  a window with nothing in it but what is being composited — no selection
  outlines, no toolbar, no docks. Pick a display and it fills it, or take it
  as an ordinary window; Escape closes it, and so does the key you bound,
  which reopens it on the screen it was last on. It costs one more draw of a
  picture that exists either way, so it is a second view rather than a
  second output: nothing is encoded, and the recording does not know it is
  there.
- **Recording** to MP4, Matroska or HLS, with hardware encoding where the
  machine has it. One recording can be split into several files by elapsed
  time or by size.
- **Streaming** to any RTMP service — Twitch, YouTube, an nginx of your own.
  The server and the stream key are kept apart, because that is how services
  hand them out and because only one of the two is a secret: the key is
  masked unless you ask to see it, and never written to the log. The
  broadcast has its own encoder, bit rate, keyframe interval and height, so
  publishing at 720p while recording at 1080p is a setting rather than a
  compromise, and a broadcast that drops is opened again on its own at an
  interval you choose.
- **Filters.** On a picture: chroma key, luma key, and colour correction
  (brightness, contrast, saturation, hue, gamma and opacity). On sound:
  noise suppression, a gate, a compressor and a limiter, on a mixer channel
  or on a Source's own audio. They are added, reordered and tuned while
  everything runs — nothing is reopened, so a slider does not cost a camera
  a visible stall.
- **Replay buffer.** Keeps the last stretch of what is being composited in
  memory and writes it out when you press the key — the clip you only knew
  you wanted after it happened. It runs with or without a recording.
- **Screenshots** of the canvas, or of one Source's own picture, saved
  beside the recordings. Both are hotkeys, and unbound to begin with because
  the keys are global.
- **Audio.** Desktop and microphone channels with faders, mute, and level
  meters that read after the fader. A fader can boost past unity, and a lamp
  reports a channel that clipped. The Scene's own media files get a channel
  each — fader, mute and meter alike — so what a clip is playing can be set
  against everything else.
- **One application's sound**, on its own channel: add one with the `+` in
  the Audio Mixer and pick what is playing. The game goes to the recording
  and the chat program does not, or the other way round. It is remembered by
  the executable, so it finds the application again after a restart, and the
  channel stays where it is — with the fader and filters you set — while
  that application is closed. Windows only so far; note that Desktop Audio
  is carrying the same sound unless you mute it, which the picker says while
  you are choosing.
- **Monitoring.** Hear a media file, a stream or a microphone while you work.
  It goes to an endpoint you choose in Settings, kept separate from the one
  Desktop Audio is captured from so that what you are listening to does not
  end up in the recording twice. Switching a channel on or off does not
  interrupt what is playing. Everything monitored is still recorded — the
  switch is about your ears, not about the file. Desktop Audio has no such
  control: it is captured by listening to what your machine is already
  playing, so there is nothing there to play back.
- **Properties.** Selecting a source describes it in a dock of its own —
  where it sits, how large it is, and what it is actually capturing. A Color
  source's colour is edited there.
- **A workspace that stays put.** Docks can be moved, resized and closed, and
  where they were is remembered along with the window and the Preview's zoom.
  Launching obs-rs while it is already running brings that window forward
  rather than opening a second one.
- **English and Korean**, chosen from `View → Language`.

## Using it

Add a source from the **+** under the Sources dock. A Display Capture asks
which monitor and a Window Capture which window; on Wayland the system's own
picker opens instead, and the choice is remembered so later runs do not ask
again.

A Window Capture is stored as the window's program and title rather than as a
handle, which only means something while that window is open. Closing the
window empties the layer and reopening it fills it again — a browser you quit
for the day is still in the Scene tomorrow.

Drag a source in the Preview to move it and its handles to resize it. Sources
higher in the list are drawn in front of lower ones.

The Preview marks what it is showing you. The selected source is outlined in
red, with the distance from each of its four edges to the edge of the canvas
written alongside — the numbers for placing something exactly, centred or
flush, instead of doing that arithmetic by eye. An edge that a crop cut is
drawn in a green dashed line instead, so a cropped source does not read as
merely a smaller one. Whatever hangs outside the canvas is hatched, being in
the Scene but not in the recording. And the source under the pointer is
outlined thinly in blue, which is the only way to tell what a click would take
where one source covers another.

Double-click a source's name in the dock to change it, the same way a Scene's
name is changed. The name belongs to the source rather than to the Scene, so
one placed in two Scenes is renamed in both at once; a name already taken is
refused where you typed it, and Escape leaves the old one alone.

Select a **Drawing** and the Preview's toolbar grows a pen. It stays in Select
until you pick one, so a stray click cannot leave a mark. The eraser takes
whole strokes rather than rubbing at pixels, and undo is the same thing aimed
at the last one.

The **Properties** dock says what the selected source is: its name and kind,
where it sits on the canvas and how large, and what it captures — the monitor
and its place in the desktop, or the program and title of a window. What a
source can be told is there too: a Color's colour, a camera's picture size
and rate, a page's address, a clip's playback and scrub bar, a stream's
transport and reconnect interval, and the four crop numbers exactly.

The **Filters** dock is the selected source's own chain, under a **Picture**
tab and a **Sound** one: a chroma key, a luma key or colour correction on
what it shows, and noise suppression, a gate, a compressor or a limiter on
what it plays. Buttons move a filter earlier or later in the chain, and
switching one off does not cost it what it was tuned to. They belong to the
source rather than to the Scene, so one placed in two Scenes is filtered in
both. A mixer channel has a chain of its own, reached from the channel.
Everything is applied while it runs: no slider reopens a camera.

**Start Recording** writes to your Videos folder unless Settings says
otherwise. What it records is the canvas, not the window — the selection
outlines and the pen toolbar are editor-only and never appear in the file.

**Start Streaming** needs a server and a stream key in
**Settings → Streaming**. The Controls dock says when a broadcast is live and
the status bar counts how long it has been; a broadcast that drops says so
and comes back on its own unless you turned that off. Recording and streaming
are independent — either, both, or one started part way through the other.

The **replay buffer** keeps the last stretch of the canvas in memory rather
than writing it: start it from the Controls dock, and the key you bound
writes what it holds to a file beside the recordings. It says how many
seconds it is holding, and that it cannot save anything in its first second.

Keys: `Ctrl+R` starts and stops recording, `Ctrl+P` pauses and resumes one,
`Ctrl+1` … `Ctrl+9` switch to that Scene, `F11` goes fullscreen, and `Ctrl+,`
opens Settings. Starting and stopping a broadcast, running the replay buffer
and saving from it, the two screenshots, and opening and closing the
projector have keys of their own, unbound to begin with. None of them fire
while you are typing a name.

All but the Scene keys can be changed in **Settings → Hotkeys**: click a
binding, press the key you want, or Backspace to clear it. They work while
another application has focus, which is the point of them — a recorder has
to be reachable from inside the game it is recording. On Windows that is a
thread asking the system which bound keys are down; on Linux it is the
desktop's own global-shortcuts portal, which asks you once whether to allow
them. Each mixer channel gets a push-to-talk and a push-to-mute binding on
that page too, which is why a key has to report being let go as well as
pressed. Every Scene has a key of its own there, and so does every source
in one: one key shows or hides that placement, so the same source in two
Scenes is two keys rather than one that takes both.

**File → Show Recordings** opens that folder in your file manager, and
**File → Settings** is the same dialog the Controls dock's button opens —
there because that dock can be closed.

Settings has six pages: General, Video (output resolution and frame rate),
Audio (sample rate, channels, and where monitoring is played), Recording
(where files go, format, encoder, bit rates, splitting, the replay buffer),
Streaming (server, key, its own encoder and bit rates, reconnecting), and
Hotkeys.

## Getting it

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

**Linux**

- **An NVIDIA driver.** Not for speed — for anything: see the note under
  *Building*. `libcuda.so.1` comes from the driver and is deliberately not in
  the archive.
- **PipeWire.** `libpipewire-0.3.so.0` is a client for a system service, so a
  bundled copy would only be a version to disagree with the one running.
  Every current desktop distribution has it.
- A glibc no older than the one the archive was built against, which is the
  current Ubuntu LTS. An older distribution needs a build from source.

Run `./obs-rs` from the unpacked directory. It finds its own copy of FFmpeg
beside it and needs no `LD_LIBRARY_PATH`.

For a name and an icon in the launcher, run `./install-desktop-entry.sh`. It
writes a desktop entry and an icon under `~/.local/share` and needs no root.
On Wayland this is the only way the window gets an icon at all — the protocol
has no call for a client to set its own, so the compositor looks for an
installed entry instead. Moving the folder afterwards breaks the entry, since
it records where the executable is; run the script again, or
`--uninstall` to remove it.

## Building

```bash
cargo run --release
```

Beyond the Rust toolchain you need FFmpeg 8.0 or newer development headers.
On Linux, desktop capture also needs PipeWire development files.

On Windows the default build also fetches the browser engine a Browser
Source draws with — Chromium, through CEF — which is a few hundred megabytes
downloaded once, and builds CEF's C++ wrapper with CMake and **Ninja**. Ninja
specifically: the `cef` crate names that generator itself, so `CMAKE_GENERATOR`
in the environment does not stand in for it. Visual Studio ships one, and
putting it on `PATH` for the build is enough:

```powershell
$env:PATH += ";C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja"
cargo run --release
```

Or install Ninja properly — `winget install Ninja-build.Ninja` — and it is on
`PATH` for every shell.

On Linux the default build fetches the same engine and builds nothing of it:
the `cef` crate links against the library it downloaded, so there is no CMake
or Ninja to install. The engine is put beside the executable, which finds it
there. To skip all of it, on either platform, while working on something else:

```bash
cargo run --release --no-default-features
```

**Linux needs an NVIDIA GPU.** Not for a faster path — for the only one. The
Linux backend composites on CUDA and there is no fallback, so obs-rs will not
start on an AMD or Intel machine. The driver alone is enough; no CUDA toolkit,
since it ships both the library and the PTX compiler. Windows uses D3D11 and
has no such requirement.

## Where it is up to

This records and it broadcasts, but it is not everything OBS is. Worth
knowing before you try it:

- **One transition for the project**, rather than one per Scene, and three
  to choose from. A stinger — a clip played over the switch — is not one of
  them.
- **Eleven source kinds.** Display Capture, Window Capture, Video Capture,
  Media File, Network Stream, Image, Drawing, Color, Text, Browser and
  Scene.
- **No Studio Mode**, so there is no separate preview of what you are about
  to cut to.
- **No groups.** A Scene can be placed inside another, which is how an
  overlay is shared, but the sources of one Scene cannot be folded into a
  collapsible bundle the way OBS groups them.
- **No multiview**, so there is no grid of every Scene to switch from. The
  projector shows the Canvas, which is one Scene at a time.
- **No virtual camera**, so nothing here appears as a webcam in a meeting.
- **Application audio on Windows only.** PipeWire can capture one
  application's stream too, and the Linux half is not written yet — the
  mixer simply does not offer the channel there.

## Contributing

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) describes how the application
is put together — the Preview's terminology, the engine's threading and
pipeline, how recordings are wired, and how localization works.
[`AGENTS.md`](AGENTS.md) is the working guide for changing it.

## License

Licensed under either the [Apache License, Version 2.0](LICENSE-APACHE) or the
[MIT License](LICENSE-MIT), at your option.

A released binary bundles FFmpeg's shared libraries, which are LGPL-2.1 and
are built without GPL components. What that means, and where their source is,
is in [`THIRD-PARTY.md`](THIRD-PARTY.md). Building from source links against
whatever FFmpeg your machine has, and none of it applies.
