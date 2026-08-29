# obs-rs

An OBS-style live broadcasting and recording application written in Rust.

## Preview terminology

The following terms are kept distinct so that `Preview` does not ambiguously refer to the UI workspace, logical coordinate system, and composited output.

| Term | Meaning | Representative code name |
|---|---|---|
| Preview Workspace | The entire central editing area excluding docks | `CentralPanel` in `ui::preview::show` |
| Preview Viewport | The on-screen 16:9 rectangle that displays the composited output | `viewport_rect` |
| Scene Canvas | The fixed logical coordinate space in which SceneItems are placed | `SceneCanvas` |
| Composite Frame | The final frame produced by compositing Sources | `CompositeFrame` |
| Editor Overlay | UI-only elements such as selection borders and guides | `paint_editor_overlay` |
| Transform Gizmo | Handles used to move, resize, and rotate an item | `ui::preview::gizmo` |
| Viewport Transform | Conversion between Canvas coordinates and screen coordinates | `ViewportTransform` |

```text
PreviewWorkspace
┌────────────────────────────────────────┐
│ Central editing workspace background   │
│                                        │
│   PreviewViewport                      │
│   ┌────────────────────────────────┐   │
│   │ CompositeFrame                 │   │
│   │                                │   │
│   │ EditorOverlay                  │   │
│   │  └─ TransformGizmo             │   │
│   └────────────────────────────────┘   │
│                                        │
└────────────────────────────────────────┘
```

The Scene Canvas is currently `1920×1080`. Resizing the window or a dock only refits the Preview Viewport; it does not change SceneItem Canvas coordinates. Preview Workspace margins and the Editor Overlay are not included in broadcast or recording output.

By default, the Preview Viewport preserves the Canvas aspect ratio within 75% of the available Workspace size. PreviewToolbar provides `−`, a percentage editor, `+`, and `Fit` controls for a 40–100% range. The Fit menu includes Fit to Workspace, 50/75/100%, and Reset View presets. This scale affects only the UI representation and does not modify SceneItem Canvas coordinates or the output resolution.

When a SceneItem extends outside the Canvas, only the portion intersecting the Canvas belongs to the Composite Frame. The outside portion of a selected SceneItem is rendered dimly in the Preview Workspace margin so that it remains editable, but it is not part of the output.

## Source and SceneItem

A `Source` is a global resource such as a capture device, image, or color. A `SceneItem` is an instance that places a Source in a particular Scene. A Source may be reused by multiple Scenes, while position, scale, visibility, lock state, crop, and compositing order remain specific to each SceneItem.

```text
Select Scene
  → SceneItem list
  → Compositor
  → CompositeFrame
  → PreviewViewport
       + EditorOverlay
```

Items near the top of the Sources dock are composited in front of lower items. While a Source is moved or resized in Preview, the UI updates a temporary Transform. The final Transform is written to the project database once when the pointer is released.

The Compositor is `media-pp`'s `CudaVideoCompositor`, and the Composite Frame is the texture the Preview Viewport samples. The UI still composites nothing itself: it draws that one texture, then the selected SceneItem's Canvas overflow and Transform Gizmo as editor overlays on top.

While a Source is moved or resized, the layer follows the pointer directly and the project database learns the Transform once, when the pointer is released. A drag is one edit, not sixty, but the picture has to move with the gizmo rather than after it.

## Current source support

- Color Source is composited, and can be moved, resized, reordered, hidden, and locked.
- Display Capture is composited on Linux. Capture lands directly in CUDA surfaces, so the desktop never passes through system memory on its way to the compositor.
- Every SceneItem is selectable, movable, and resizable in the editor regardless of whether its Source can produce a frame yet. The editor works on the item's Canvas rectangle, not on the Source's content.
- A Display Capture source stores the pixel size its picker reported, so a new SceneItem starts at the display's own shape rather than being squared off to the Canvas. The size is a hint, not a fact: the display layout can change between runs and a compositor may scale a Wayland stream to a size the portal never named, so the capture layer replaces it with the stream's negotiated size once the Source opens. A Source with no reported size stands in at Canvas size, because an item with no rectangle cannot be selected or dragged at all.
- Display Capture can enumerate monitors on Windows and Linux/X11, persist the selected monitor name, and create a SceneItem. On Wayland, source creation opens the system-owned `xdg-desktop-portal` picker and persists the restore token it issues, and a later run reopens the same display from that token without showing the picker again. Windows has no capture element wired up yet, so a Display Capture there is stored but produces no pixels.

A Display Capture source stores one of two targets, and neither platform can produce the other:

| Platform | Stored target | Reproduced by |
|---|---|---|
| Windows, Linux/X11 | Monitor name (`\\.\DISPLAY1`, `DP-1`) | Resolving the name against the live display layout |
| Wayland | `xdg-desktop-portal` restore token | Reopening a portal session with the token |

Wayland never names a display: the portal owns the picker and returns only what the user chose. A stream id belongs to the session that produced it, so the restore token is the only value that reproduces a selection in a later run. The compositor may decline to issue one, which is not an error — capture then shows the picker again instead of restoring silently.

Opening a Source can hand back a fresher restore token than the one it was given, and that replaces the stored one: a compositor is free to issue a new token on every restore, and keeping the old one would mean prompting on every launch.

Monitor *position* in the virtual desktop is intentionally not persisted — it changes whenever displays are rearranged, and nothing resolves against it. The capture layer resolves the saved monitor name against the current display layout; on Windows it will open `DxgiCaptureSource` in GPU mode. GPU mode is an application invariant rather than a user setting and does not currently support cursor inclusion.

## The engine

`src/engine` owns everything that produces Preview pixels, on its own thread. It reconciles running Sources against the project snapshot rather than reacting to the actions that changed it: a restart replays no actions but must still open everything the project holds, and selecting a Scene replaces the whole set at once.

```text
capture / colour  →  CudaConverter  →  CudaVideoCompositor  (NV12, on the GPU)
                                             ↓
                                       CudaDownload
                                             ↓
                              two plane textures  →  resolve pass  →  CompositeFrame
```

The compositor's NV12 output is resolved into one RGBA texture by a small render pass, because egui draws one texture and NV12 is two planes in a colour space that is not RGB. Doing that on the CPU would undo the reason compositing is on the GPU.

The Preview is redrawn at half the compositor's rate. It is a few hundred pixels wide and watched by one person, and halving it took the application from 10% of a twelve-core machine to 2.5% — almost none of which is pixels, since downloading and resolving at 720p instead of 1080p was measured and changed nothing. The cost is per-frame overhead, most of it the whole-UI repaint each drawn frame asks egui for. The compositor keeps its own rate, because that is what a recording will be made of.

The Preview branch is not allowed to set the compositor's pace. It sits behind a dropping queue, so a Preview that cannot keep up drops frames rather than slowing the output every other branch will be built from. It also sleeps entirely when no shown Source is running — an empty Scene, or one whose Sources are all in other Scenes, costs nothing.

Switching Scenes stops a Source rather than closing it, so returning is a resume and not another portal round trip. Only a SceneItem the project no longer holds anywhere is closed, which is why the snapshot carries every item and not just the shown Scene's.

## Status bar

CPU is this process's share of total machine capacity. GPU prefers a per-process figure and falls back to whole-adapter usage, marked with a trailing `*`, when the platform offers no per-process counter — NVIDIA's Linux driver exposes neither `drm-engine-*` fdinfo nor working per-process NVML samples on GeForce parts, so a device figure is all that exists there. Hovering the reading names its scope.

Per-process NVML is trusted only once obs-rs's own process has appeared in a sample. A driver that answers a poll with other processes' entries and never ours is indistinguishable from obs-rs using no GPU, and treating that as zero would alternate a false `0.0%` with the real device figure.

## Building

Beyond the Rust toolchain, `media-pp` needs FFmpeg 8.0 or newer development headers. On Linux it also needs PipeWire development files for desktop capture, and the CUDA path needs an NVIDIA driver at run time — no CUDA toolkit, since the driver ships both the library and the PTX compiler.

## Localization

UI translations use Fluent FTL language packs. `en-US` is the default and fallback locale, and `ko-KR` is currently included.

```text
assets/locales/
├─ en-US/app.ftl
└─ ko-KR/app.ftl
```

UI code requests translations from `LocalizationManager` through `TextKey` instead of containing user-facing strings directly. Selecting a language from `View → Language` emits `UiAction::SetLocale`, and the selected locale is applied on the next UI frame. A missing translation falls back to English.

The selected locale is stored as an application preference rather than project data. On Windows, the settings file is `%APPDATA%/obs-rs/settings.toml`:

```toml
locale = "ko-KR"
```

The same file also remembers the workspace: where the window was and how large, how wide each dock region was dragged, which docks were open, which region each one was in, and the Preview's zoom (including whether it was set to Fit). It is written once when the application closes, and read before the window opens. A remembered position is used again only if it still lands on one of this session's displays, so a window left on a monitor that is now gone comes back on screen rather than off it — and where displays cannot be enumerated at all (Wayland), the saved position is trusted rather than discarded. A dock the file does not mention keeps its default place, so an arrangement written before a dock existed still shows it.

The built-in FTL files are embedded in the executable with `include_str!`; they are not required beside the executable at runtime. Korean glyphs use a CJK system font registered as an egui fallback. If distribution must not depend on system fonts, add a suitably licensed font to the application assets.
