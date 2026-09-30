# obs-rs

An OBS-style scene compositor, screen recorder and streamer, written in Rust
on top of [`media-pp`](https://github.com/hash1018/media-pp). Captures your
desktop, composites it on the GPU, and records it or sends it to an RTMP
service — with an annotation layer you can draw on while it runs. Windows,
Linux and macOS.

![The obs-rs window on macOS: the Scenes, Sources and Properties docks down
the left; the Preview in the middle, where a web page — a gradient with a
LIVE badge, a clock and a lower third — a title card, a logo and a looping
test clip are composited together, the selected title card outlined in red
with the distance to each canvas edge written beside it; and the Audio
Mixer, metering the clip's sound, the Filters and the Controls along the
bottom.](docs/screenshot.png)

## What it does

- **Scenes** of sources on a 1920×1080 canvas, with transitions, crop, and
  a Scene placed inside another to share an overlay.
- **Sources:** display and window capture, webcams, media files, RTSP
  streams, web pages, images, text, colour and a layer you draw on.
- **Recording** to MP4, Matroska or HLS with hardware encoding, **streaming**
  to any RTMP service, a **replay buffer** and screenshots.
- **Audio:** a mixer with a channel per device, per media file and per
  application, filters on picture and sound, and monitoring.
- **Hotkeys** that work while another application has focus, undo and redo,
  a projector, and a workspace that remembers itself. English and Korean.

The whole list is in [docs/features.md](docs/features.md), and how to use it
in [docs/usage.md](docs/usage.md).

## Getting it

Each release carries an archive per platform with FFmpeg inside; unpack it
and run what is in it. What each platform needs besides is in
[docs/install.md](docs/install.md).

## Building

```bash
cargo run --release
```

Rust and FFmpeg 8.0 development headers, and on Windows and macOS CMake and
Ninja for the browser engine — the details, per platform, are in
[docs/building.md](docs/building.md).

## Documentation

| | |
|---|---|
| [features.md](docs/features.md) | Everything it does, source by source |
| [usage.md](docs/usage.md) | Adding and arranging sources, the docks, the keys |
| [install.md](docs/install.md) | Running a released build on each platform |
| [building.md](docs/building.md) | Building from source, the browser engine, how each platform composites |
| [status.md](docs/status.md) | What it does not do yet, next to OBS |
| [macos.md](docs/macos.md) | How the macOS build is put together |
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | How the application is put together, for changing it |

[`AGENTS.md`](https://github.com/hash1018/obs-rs/blob/main/AGENTS.md) is the
working guide for changing it.

## License

Licensed under either the [Apache License, Version 2.0](LICENSE-APACHE) or the
[MIT License](LICENSE-MIT), at your option.

A released binary bundles FFmpeg's shared libraries, which are LGPL-2.1 and
are built without GPL components. What that means, and where their source is,
is in [`THIRD-PARTY.md`](THIRD-PARTY.md). Building from source links against
whatever FFmpeg your machine has, and none of it applies.
