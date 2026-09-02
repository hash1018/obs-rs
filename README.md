# obs-rs

An OBS-style scene compositor and screen recorder, written in Rust on top of
[`media-pp`](https://github.com/hash1018/media-pp). Captures your desktop,
composites it on the GPU, and records it — with an annotation layer you can
draw on while it runs.

![The obs-rs window: the Scenes, Sources and Controls docks on the left, the
Preview in the middle with a Drawing selected and its pen toolbar under it, and
the Audio Mixer along the bottom.](docs/screenshot.png)

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
- **Crop.** Alt-drag a source's handle to cut into its picture instead of
  resizing it: the dragged edge moves, the opposite one stays, and what is
  being cut off shows faintly behind while you aim. Alt+double-click puts an
  edge back, and the Properties dock takes the four numbers exactly.
- **Recording** to MP4, Matroska or HLS, with hardware encoding where the
  machine has it. One recording can be split into several files by elapsed
  time or by size.
- **Audio.** Desktop and microphone channels with faders, mute, and level
  meters that read after the fader. A fader can boost past unity, and a lamp
  reports a channel that clipped. The Scene's own media files get a channel
  each — fader, mute and meter alike — so what a clip is playing can be set
  against everything else.
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
and its place in the desktop, or the program and title of a window. Most of it
reports rather than asks; a Color source's colour is the one thing edited
there.

**Start Recording** writes to your Videos folder unless Settings says
otherwise. What it records is the canvas, not the window — the selection
outlines and the pen toolbar are editor-only and never appear in the file.

Keys, while the window has focus: `Ctrl+R` starts and stops recording,
`Ctrl+P` pauses and resumes one, `Ctrl+1` … `Ctrl+9` switch to that Scene,
`F11` goes fullscreen, and `Ctrl+,` opens Settings. None of them fire while
you are typing a name. All but the Scene keys can be changed in
**Settings → Hotkeys**: click a binding, press the key you want, or Backspace
to clear it.

**File → Show Recordings** opens that folder in your file manager, and
**File → Settings** is the same dialog the Controls dock's button opens —
there because that dock can be closed.

Settings has four pages: General, Video (output resolution and frame rate),
Audio (sample rate and channels), and Recording (where files go, format,
encoder, bit rates, splitting).

## Building

```bash
cargo run --release
```

Beyond the Rust toolchain you need FFmpeg 8.0 or newer development headers.
On Linux, desktop capture also needs PipeWire development files, and the CUDA
path needs an NVIDIA driver at run time — no CUDA toolkit, since the driver
ships both the library and the PTX compiler.

## Where it is up to

This is a working recorder, not a finished broadcaster. Worth knowing before
you try it:

- **No streaming.** There is no RTMP output, so nothing goes to Twitch or
  YouTube yet. Recording is the whole of it.
- **Seven source kinds.** Display Capture, Window Capture, Media File,
  Network Stream, Image, Drawing and Color. Cameras are not there.

- **No filters or transitions.** Switching Scenes is a cut.

## Contributing

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) describes how the application
is put together — the Preview's terminology, the engine's threading and
pipeline, how recordings are wired, and how localization works.
[`AGENTS.md`](AGENTS.md) is the working guide for changing it.
