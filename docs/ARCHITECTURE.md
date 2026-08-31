# obs-rs architecture

How the application is put together, for anyone changing it. What it is and
how to run it are in the [README](../README.md).

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

The Compositor is `media-pp`'s — `D3d11VideoCompositor` on Windows, `CudaVideoCompositor` on Linux — and the Composite Frame is the texture the Preview Viewport samples. The UI still composites nothing itself: it draws that one texture, then the selected SceneItem's Canvas overflow and Transform Gizmo as editor overlays on top.

While a Source is moved or resized, the layer follows the pointer directly and the project database learns the Transform once, when the pointer is released. A drag is one edit, not sixty, but the picture has to move with the gizmo rather than after it.

## Current source support

- Color Source is composited, and can be moved, resized, reordered, hidden, and locked. Its colour is stored per source and edited in the Properties dock; a new one starts blue. The colour belongs to the Source, so two SceneItems standing for one Color change together.
- Drawing is composited on both backends and is the one source that carries transparency: it reaches the compositor as BGRA rather than through a converter, so what was never drawn on lets the scene beneath it through. See `drawing_bgra` for why a stroke is rasterized in two passes.
- Display Capture is composited on Windows and Linux. Capture lands directly in GPU surfaces — D3D11 textures or CUDA surfaces — so the desktop never passes through system memory on its way to the compositor.
- Every SceneItem is selectable, movable, and resizable in the editor regardless of whether its Source can produce a frame yet. The editor works on the item's Canvas rectangle, not on the Source's content.
- A Display Capture source stores the pixel size its picker reported, so a new SceneItem starts at the display's own shape rather than being squared off to the Canvas. The size is a hint, not a fact: the display layout can change between runs and a compositor may scale a Wayland stream to a size the portal never named, so the capture layer replaces it with the stream's negotiated size once the Source opens. A Source with no reported size stands in at Canvas size, because an item with no rectangle cannot be selected or dragged at all.
- Display Capture can enumerate monitors on Windows and Linux/X11, persist the selected monitor name, and create a SceneItem. On Wayland, source creation opens the system-owned `xdg-desktop-portal` picker and persists the restore token it issues, and a later run reopens the same display from that token without showing the picker again.

Window Capture is composited on Windows and written for Linux, where it narrows the same portal source to windows rather than monitors. It is the one source whose target is expected to come and go — see "A window that is not there" below.

A Display Capture source stores one of two targets, and neither platform can produce the other:

| Platform | Stored target | Reproduced by |
|---|---|---|
| Windows, Linux/X11 | Monitor name (`\\.\DISPLAY1`, `DP-1`) | Resolving the name against the live display layout |
| Wayland | `xdg-desktop-portal` restore token | Reopening a portal session with the token |

Wayland never names a display: the portal owns the picker and returns only what the user chose. A stream id belongs to the session that produced it, so the restore token is the only value that reproduces a selection in a later run. The compositor may decline to issue one, which is not an error — capture then shows the picker again instead of restoring silently.

Opening a Source can hand back a fresher restore token than the one it was given, and that replaces the stored one: a compositor is free to issue a new token on every restore, and keeping the old one would mean prompting on every launch.

A Window Capture stores a pair, and for the same reason: a window handle means something only inside the session that issued it.

| Platform | Stored target | Reproduced by |
|---|---|---|
| Windows, Linux/X11 | `{program, title}` | Searching the live window list |
| Wayland | `xdg-desktop-portal` restore token | Reopening a portal session with the token |

Neither half of that pair identifies a window on its own: two windows of one program are common, and two programs can show the same title. The search takes an exact match on both first, then the same program under any title — titles change constantly (a document name, a tab, an unsaved marker) and a window whose title moved on is still the window the user picked. Where several windows of that program are open the first is taken, because nothing stored tells them apart.

A portal Window Capture stores nothing at all when it is added. There is no list to pick from, so the item is created pointing at the portal and the portal asks which window the first time the Source opens; the token it hands back is what reopens it after that.

Monitor *position* in the virtual desktop is intentionally not persisted — it changes whenever displays are rearranged, and nothing resolves against it. The capture layer resolves the saved monitor name against the current display layout; on Windows it will open `DxgiCaptureSource` in GPU mode. GPU mode is an application invariant rather than a user setting and does not currently support cursor inclusion.

## The engine

`src/engine` owns everything that produces Preview pixels, on its own thread. It reconciles running Sources against the project snapshot rather than reacting to the actions that changed it: a restart replays no actions but must still open everything the project holds, and selecting a Scene replaces the whole set at once.

It is laid out by what a part does, with the platform split inside each rather than around it — a `linux.rs` and a `windows.rs` beside a shared `mod.rs` that holds whatever both agree on:

```text
engine/
├─ backend/      the compositor, and what a platform's Backend is
├─ source/       opening one SceneItem's Source
│   ├─ color.rs, drawing.rs        (no platform half: both push pixels)
│   ├─ display_capture/            (a portal here, a registry on Windows)
│   └─ window_capture/             (a search here, the same portal there)
├─ preview/      compositor output → egui, and nothing else
├─ recording/    the encode chain and the muxer's tracks
└─ audio/        capture, per-source gain, one mix
```

The CUDA path, which is the one with a conversion in it:

```text
capture / colour  →  CudaConverter  ─┐
                                     ├→  CudaVideoCompositor  (NV12, on the GPU)
drawing (BGRA, keeps its alpha)  ────┘             ↓
                                        memory Vulkan allocated
                                        and CUDA imported
                                                   ↓
                              two plane textures  →  resolve pass  →  CompositeFrame
```

Nothing on that path crosses the bus. The composited frame is copied into memory wgpu already owns — Vulkan allocates it and exports a file descriptor, `cuImportExternalMemory` takes exactly that — so the two plane uploads and the resolve are one GPU submission. Reading the frame back to system memory and pushing it up again is what this replaced.

Windows is the same shape with `D3d11VideoCompositor` and no converter at all: capture, compositor and encoder are BGRA end to end, so there is nothing between them to convert, and the resolve pass is a CUDA concern only — Windows shares one texture and hands wgpu a view of it.

The compositor's NV12 output is resolved into one RGBA texture by a small render pass, because egui draws one texture and NV12 is two planes in a colour space that is not RGB. Doing that on the CPU would undo the reason compositing is on the GPU.

The Preview is redrawn at 30 fps, or at the compositor's own rate when that is lower. It is a few hundred pixels wide and watched by one person, and halving it took the application from 10% of a twelve-core machine to 2.5% — almost none of which is pixels, since downloading and resolving at 720p instead of 1080p was measured and changed nothing. The cost is per-frame overhead, most of it the whole-UI repaint each drawn frame asks egui for. The compositor keeps its own rate, because that is what a recording will be made of.

The Preview branch is not allowed to set the compositor's pace. It sits behind a dropping queue, so a Preview that cannot keep up drops frames rather than slowing the output every other branch will be built from. It also sleeps entirely when no shown Source is running — an empty Scene, or one whose Sources are all in other Scenes, costs nothing.

### A window that is not there

Opening a Source answers with four outcomes rather than two. A window that is not on screen is neither open nor failed: `open_source` returns `Ok(None)` and the engine holds the SceneItem as `Missing` — nothing was opened, so nothing holds a device or a dialog while it waits. `Failed` stays terminal, because a source that could not open will not open by being asked again. `Disconnected` is the fourth, and it exists because on one platform looking again is itself an interruption.

Two things then have to be noticed rather than waited for, and both happen on the engine loop's idle tick, once a second:

- a window that comes back, which reopens the Source where it stood — where the engine may go looking at all, see below;
- a window that closes while its capture is running. That ends the capture — the compositor sees the input finish and drops the layer — but nothing tells the engine, so it asks the pipeline through `Pipeline::ended`. The endings are not alike on the bus: a closed window ends `WgcCaptureSource` as an *error*, where a file source ends with `Eos`, and whoever might reopen it has no reason to tell those apart.

#### Who pays for looking again

Which of `Missing` and `Disconnected` a closed window lands in follows from the stored target, and so from the platform:

| Stored target | Looking again costs | State | What brings it back |
|---|---|---|---|
| `{program, title}` | enumerating windows | `Missing` | the idle tick, once the window is back |
| portal restore token | a modal picker over whatever the user is doing | `Disconnected` | the user, through the Sources list |

A portal selection cannot be checked. The portal owns the picker, a closed window's restore token is dead, and there is no way to ask whether one is still good without starting the flow that puts a dialog on screen — so the engine never starts it. The Sources dock says **끊김** beside such an item, and clicking that is the ask: `EngineManager::reopen_source` is the only path that reopens one. A picker the user then cancels comes back as an error and is treated the same way, as an answer rather than a fault, leaving the Source disconnected and offered again rather than `Failed`.

The set of Sources producing nothing is published to the UI the way a recording error is, and replaced only when it changes: the Sources list reads it every pass, and it moves about as often as a window closes.

Nothing else is polled this way. A display does not close, and a capture shared between SceneItems belongs to the registry rather than to one item.

Switching Scenes stops a Source rather than closing it, so returning is a resume and not another portal round trip. Only a SceneItem the project no longer holds anywhere is closed, which is why the snapshot carries every item and not just the shown Scene's.

## One instance

Two copies would open the same project database and write the same log, and
each would open its own captures — one desktop duplicated twice, two claims on
the same audio endpoints, two compositors on one GPU. So the first thing
`main` does, before any file of its own is opened, is claim a lock in the data
directory; a launch that cannot have it raises the running window and stops.

The claim is a file *held open* rather than a flag written into one: the
operating system releases a handle however the process ended, a crash
included, where a flag has to be cleared by whoever stopped running. Windows
needs no lock call for this — a handle opened for writing while sharing only
reads refuses every later writer, which is the claim itself — and Linux takes
a `flock`. The file's contents are only the process id, so the launch being
turned away can find the window to raise; a wrong or missing id costs that
raise and nothing else.

Raising is written for Windows and deliberately not guessed at for Linux: it
is `_NET_ACTIVE_WINDOW` under X11 and something a Wayland compositor may
refuse outright, focus-stealing being what that protocol is designed to
prevent. There, a second launch says so on stderr and stops.

## Hotkeys

Keys are dispatched before anything is drawn, in `ui::shell::hotkeys`, and each one produces the `UiAction` its button or menu item would. Nothing is reachable only from a key — what a key adds is reach, since the Controls dock holding Start Recording can be closed.

Two rules carry the layer. A chord is ignored while anything is taking typed input, because there is no chord this could reserve that a Scene's name might not contain. And a held key repeats, where a repeat is not a press: without that distinction leaning on `Ctrl+R` starts and stops a recording sixty times a second. The second rule is not the caller's to enforce — egui rewrites the repeat flag from its own record of which keys are down, so a key repeats only by being sent again without a release, which is what the test holds one across three passes to reproduce.

Modifiers match exactly rather than through egui's `matches_logically`, which treats extra Shift and Alt as noise. `Ctrl+Shift+R` would otherwise start a recording, and could never be bound to anything else.

Four of the bindings come from `settings.toml`, through `crate::hotkey`: a binding is stored as the string a person would write (`Ctrl+R`, `F11`, `Ctrl+,`), read loosely — case and either spelling of a key, since egui answers to both `Comma` and `,` — and written back in the short form, so a file edited by hand comes back tidy. Cleared is a value rather than an absence: an empty string, because a key left out of the file is one the defaults put straight back.

The Scene keys are not among them. `Ctrl+1` through `Ctrl+9` select by *position*, which is a convention rather than a binding — a per-Scene key is the model that survives reordering, and it belongs with per-Source bindings rather than as nine more rows on a page.

The settings page binds by listening: the button shows what is bound and, while it waits, takes the next key. Escape keeps what was there and Backspace clears it, which are the two things somebody in the middle of choosing might want that are not a key to bind. While one is waiting the hotkey layer stands down, or `Ctrl+R` would start a recording on its way to being bound to something. Two actions on one chord is warned about rather than refused: the first listed would take the key and the second would never see it, so the page names the clash beside the row instead of leaving the user holding a key that does nothing.

Global hotkeys — keys that work while another application has focus — are a different mechanism per platform and a separate piece of work.

## The Properties dock

What the selected SceneItem is, as it currently stands: name and kind, its place and size on the Canvas, whether it is visible and locked, and then whatever its kind has to say — the monitor and its rectangle in the virtual desktop, a window's program and title, a Drawing's stroke count, a Color's colour.

It is a dock and not a dialog because the values move while they are read: dragging in the Preview changes the numbers here, and a dialog would have to be reopened to see it while covering the picture the numbers are about.

Almost all of it reports rather than asks. Everything shown is already settable somewhere — a Transform by dragging, visibility and lock by the Sources dock's icons — and this says what those came out as, in numbers a drag cannot be precise about. The one exception is a Color's colour, which follows the same two-part shape a drag does: `UiAction::DragSourceColour` goes straight to the engine so the layer changes under the pointer, and one `SourceCommand::SetColor` is recorded when the picker is let go. A live gesture is one edit in the project, not sixty.

Crop is deliberately absent. `SceneItemSnapshot` carries one and the editor's geometry honours it, but nothing in either backend does, so a crop reported here would describe something the recording does not do.

## Recording output

A recording is two tracks: the compositor's frames encoded as H.264, and the audio mixer's output as AAC or Opus. Neither half owns the file — `engine::recording` does — because an mp4's tracks are fixed before its header is written, so both encoders have to exist before a frame is written to either, and the trailer is written only once *every* track has reported done. Ending one and not the other leaves a file exactly as long as it is unplayable.

A machine whose mixer never started records video alone rather than refusing, so the track list is decided per recording from what is actually running.

Each track is zeroed by its own `TimestampOrigin`, because there is no clock the two share: the compositor counts composed frames and the mixer counts emitted samples, both since launch and on unrelated counters. Measured against a clip whose flash and beep are simultaneous, the two land within one video frame of each other, and a two-minute recording drifts 29 ms end to end — so nothing here needs a shared clock.

The container is chosen on the Recording page and is very nearly a choice of extension: `media-pp`'s `FileMuxer` asks FFmpeg to guess a muxer from the file name, so MP4 and Matroska are the same element told a different path. Matroska is worth choosing for a long recording — MP4 is finalized in its trailer, so a session that dies with the application leaves an unplayable file, where a `.mkv` plays up to where it stopped.

HLS is the third option and a different element. It writes a VOD playlist and fMP4 segments into a directory named for the recording, so a session is servable as it stands and every completed segment is on disk before the recording ends. fMP4 rather than MPEG-TS because Opus is one of the audio encoders offered here, and TS carries it badly.

One recording can also be cut into several files, by elapsed time or by bytes written. Either way the cut lands on the next keyframe rather than on the figure itself, so every file opens on its own — which means a file runs past what was asked for by up to one GOP. Splitting is disabled for HLS, which is already segmenting on its own target duration.

## Status bar

CPU is this process's share of total machine capacity. GPU prefers a per-process figure and falls back to whole-adapter usage, marked with a trailing `*`, when the platform offers no per-process counter — NVIDIA's Linux driver exposes neither `drm-engine-*` fdinfo nor working per-process NVML samples on GeForce parts, so a device figure is all that exists there. Hovering the reading names its scope.

MEM is what this process has to itself in system memory — private commit on Windows, resident set on Linux, the nearest each platform offers to the other. Deliberately not the whole story: a compositor layer, a capture pool and a preview texture live in video memory, which is a separate figure and not one this reading pretends to include. It is here because an application that holds frame buffers for hours is one whose memory is worth being able to watch without a task manager.

Per-process NVML is trusted only once obs-rs's own process has appeared in a sample. A driver that answers a poll with other processes' entries and never ours is indistinguishable from obs-rs using no GPU, and treating that as zero would alternate a false `0.0%` with the real device figure.

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
