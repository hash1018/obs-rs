# Building obs-rs

From source, on each platform. Running a released build instead is in
[install.md](install.md).

```bash
cargo run --release
```

Beyond the Rust toolchain you need FFmpeg 8.0 or newer development headers.
On Linux, desktop capture also needs PipeWire development files, FFmpeg has
to be built with Vulkan, and the Vulkan headers have to be installed
(`libvulkan-dev`); the bindings to FFmpeg's Vulkan context are generated
from them with libclang.

## The browser engine

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
there.

On macOS it needs CMake and Ninja, as on Windows, and the engine is a
framework only an application bundle can carry, so `cargo run` runs obs-rs
from one: `.cargo/config.toml` hands the executable to
`assets/macos/run-app.sh`, which puts it in `target/<profile>/obs-rs.app`
beside the framework and runs it there. It is still this terminal's process
— its output is here and Ctrl+C stops it — and the terminal is what the
screen-recording and camera permissions are given to, which a rebuild does
not undo.

Wherever it is fetched to is kept only as long as the build directory is,
unless `CEF_PATH` names somewhere else; `cargo clean` then leaves it alone,
where otherwise it is fetched again. Setting it for every build is one line
in `~/.cargo/config.toml`:

```toml
[env]
CEF_PATH = "/home/you/.local/cef"
```

To skip all of it, on any platform, while working on something else:

```bash
cargo run --release --no-default-features
```

## How each platform composites

**Linux composites on the GPU, CUDA or Vulkan.** CUDA where NVIDIA's driver
is there — it ships both the library and the PTX compiler, so no CUDA
toolkit — and Vulkan on any other GPU, AMD's or Intel's through Mesa. The
choice is made at startup; `OBSRS_GPU=vulkan` (or `cuda`) insists on one,
which is how the Vulkan path is tried on an NVIDIA machine. What Vulkan does
differently: a screen capture comes through system memory, and the Preview is
a readback.
Windows uses D3D11 and has neither.

**macOS composites on Metal**, over VideoToolbox frames, and records with
VideoToolbox: every Source works — the display, window and camera captures
through ScreenCaptureKit and AVFoundation among them, which need the
screen-recording and camera permissions — and the mixer records Core
Audio's inputs, outputs and applications; see [`macos.md`](macos.md). FFmpeg for it
is set up as
[media-pp's macOS notes](https://github.com/hash1018/media-pp/blob/main/docs/building/macos.md)
describe.
