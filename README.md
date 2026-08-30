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
  canvas. Move, resize, reorder, hide and lock them in the Preview. A source
  can appear in more than one Scene, with its own position in each.
- **Display capture** on Windows and Linux. Frames go straight into GPU
  memory and stay there through compositing and encoding — the desktop never
  passes through system memory on its way to the recording.
- **Drawing.** A source you draw on rather than one that captures something:
  pen, highlighter and eraser, with undo and clear. It is a real layer, so
  what you draw is in the recording, not just on your screen.
- **Recording** to MP4, Matroska or HLS, with hardware encoding where the
  machine has it. One recording can be split into several files by elapsed
  time or by size.
- **Audio.** Desktop and microphone channels with faders, mute, and level
  meters that read after the fader. A fader can boost past unity, and a lamp
  reports a channel that clipped.
- **A workspace that stays put.** Docks can be moved, resized and closed, and
  where they were is remembered along with the window and the Preview's zoom.
- **English and Korean**, chosen from `View → Language`.

## Using it

Add a source from the **+** under the Sources dock. A Display Capture asks
which monitor; on Wayland the system's own picker opens instead, and the
choice is remembered so later runs do not ask again.

Drag a source in the Preview to move it and its handles to resize it. Sources
higher in the list are drawn in front of lower ones.

Select a **Drawing** and the Preview's toolbar grows a pen. It stays in Select
until you pick one, so a stray click cannot leave a mark. The eraser takes
whole strokes rather than rubbing at pixels, and undo is the same thing aimed
at the last one.

**Start Recording** writes to your Videos folder unless Settings says
otherwise. What it records is the canvas, not the window — the selection
outlines and the pen toolbar are editor-only and never appear in the file.

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
- **Two source kinds.** Display Capture and Drawing, plus a Color source whose
  colour is not yet choosable. Window capture, cameras, images and media files
  are not there.
- **No filters, transitions or hotkeys.** Switching Scenes is a cut.

## Contributing

[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) describes how the application
is put together — the Preview's terminology, the engine's threading and
pipeline, how recordings are wired, and how localization works.
[`AGENTS.md`](AGENTS.md) is the working guide for changing it.
