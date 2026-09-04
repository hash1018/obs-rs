# Third-party software in an obs-rs release

obs-rs itself is MIT OR Apache-2.0 — see [`LICENSE-MIT`](LICENSE-MIT) and
[`LICENSE-APACHE`](LICENSE-APACHE). This file is about what a *released
binary* carries alongside it, which is a different question: building from
source links against whatever FFmpeg the machine already has, and nothing here
applies.

## FFmpeg

A release archive bundles FFmpeg's shared libraries, because the application
cannot start without them:

```text
avcodec  avdevice  avfilter  avformat  avutil  swresample  swscale
```

They are **LGPL-2.1-or-later**, and the release is built to keep them that
way:

- **No GPL components.** The build is vcpkg's default `ffmpeg` port with the
  `openh264` feature added and nothing else. GPL-only pieces — `x264`,
  `x265`, and the rest — are not in vcpkg's default feature set, so they are
  not in the build. OpenH264 is BSD-2-Clause and is what obs-rs encodes H.264
  with where no hardware encoder is available.
- **Dynamic linking only.** obs-rs loads these as DLLs or shared objects and
  never links them statically, which is what keeps the LGPL's relinking
  requirement satisfiable.

### Where the source is

The exact FFmpeg these binaries are built from is pinned by this repository
rather than described loosely. The pin lives in the workflow that builds a
release and names an immutable vcpkg tag:

| | |
|---|---|
| FFmpeg version | 8.0.1 |
| vcpkg tag | `2026.01.16` |
| port | `ffmpeg[core,openh264]` |
| triplet | `x64-windows`, `x64-linux-dynamic` |

Building `microsoft/vcpkg` at that tag with those options reproduces the
libraries in the archive, and the port's own portfile records which FFmpeg
tarball it fetches. FFmpeg's sources are at <https://ffmpeg.org/download.html>
and <https://github.com/FFmpeg/FFmpeg>.

### License text

Each release archive carries the license files that ship with the FFmpeg build
it bundles, under `licenses/`, rather than a copy committed here — the terms
that apply are the ones distributed with those exact binaries, and a
hand-maintained second copy could fall out of step with them.

## The Visual C++ runtime

The Windows archive carries `vcruntime140.dll`, `msvcp140.dll` and the rest of
Microsoft's VC++ runtime, because both the executable and the FFmpeg
libraries are built against it and it is not part of a stock Windows install.

Microsoft's redistributable licence permits shipping these files inside an
application's own directory, which is what this does — they sit beside
`obs-rs.exe` and are used by nothing else. The alternative would be telling
every user to install the redistributable first, which is a worse first
minute for no gain to anybody.

## Rust dependencies

Everything else obs-rs uses is a Rust crate compiled into the executable, and
those are permissive (MIT, Apache-2.0, BSD, ISC, Zlib). `cargo tree` lists
them; `cargo about` or `cargo deny` will generate an attribution file from the
lockfile if one is ever wanted.
