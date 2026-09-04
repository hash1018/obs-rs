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
- Image is a still picture, decoded once and pushed once — a Color Source with a file behind it rather than a colour. It is decoded through FFmpeg rather than an image crate: a PNG is a one-frame container as far as that library is concerned, so the demuxer and decoder that open a video open a picture, and there is one decoder in the tree instead of two. A file that is not there leaves the Source `Missing` and looked for again, the same as a media file's.
- Every SceneItem is selectable, movable, and resizable in the editor regardless of whether its Source can produce a frame yet. The editor works on the item's Canvas rectangle, not on the Source's content.
- A Display Capture source stores the pixel size its picker reported, so a new SceneItem starts at the display's own shape rather than being squared off to the Canvas. The size is a hint, not a fact: the display layout can change between runs and a compositor may scale a Wayland stream to a size the portal never named, so the capture layer replaces it with the stream's negotiated size once the Source opens. A Source with no reported size stands in at Canvas size, because an item with no rectangle cannot be selected or dragged at all.
- Display Capture can enumerate monitors on Windows and Linux/X11, persist the selected monitor name, and create a SceneItem. On Wayland, source creation opens the system-owned `xdg-desktop-portal` picker and persists the restore token it issues, and a later run reopens the same display from that token without showing the picker again.

Network Stream pulls a live RTSP session — an IP camera, most often — and is built like a Media File with a different source at the top: the same two branches, the same hardware decode, the same mixer input for its sound. See "A stream that stopped arriving" below for the half a file does not have.

Window Capture is composited on Windows and written for Linux, where it narrows the same portal source to windows rather than monitors. It is the one source whose target is expected to come and go — see "A window that is not there" below.

Video Capture is a camera. It is the shortest pipeline here — `MfCaptureSource ─ Queue ─ D3d11Upload ─ compositor input` on Windows, `V4l2CaptureSource ─ Queue ─ CudaUpload ─ compositor input` on Linux — because both capture elements hand over NV12 in system memory whatever the device natively speaks, and both uploads take NV12 directly. The `Queue` is the thread boundary, two frames deep: a camera has no timeline to replay, so anything deeper would be latency rather than smoothing. There is no `Pacer` for the same reason.

The conversion is the element's on both platforms, which is what makes the two pipelines the same shape. Media Foundation does it in the driver stack; V4L2 does not convert at all, so `V4l2CaptureSource` decodes and converts itself — a USB camera speaks YUYV at low resolutions and Motion JPEG at high ones, and 720p is commonly offered *only* compressed. One geometry there is not one mode: the same size can be offered by two pixel formats, so the format that carries the chosen mode is named when the device is opened rather than left to the demuxer, which would otherwise pick one that does not have the size and be answered with a different one.

A camera states its own modes, and which one to ask for is stored per Source and picked in the Properties dock. "Automatic" means the camera's own first offered mode, which is its stated preference. Reading the list means opening the device, so it is read once per session when the list is first opened rather than each frame. A mode carries a size, so choosing one moves the item's size hint with it — otherwise a 640x480 picture would be drawn stretched into the rectangle a 1280x720 one had.

Nothing in obs-rs opens a camera's *microphone*: a webcam that has one appears in the Audio Mixer as an ordinary input device, because that is what both platforms make it.

## Media File

The one source that is not a capture, and the only one with sound of its own. It is one pipeline with two branches off one demuxer:

```text
FileDemuxer ┬ video ─ hardware decoder ─ Queue ─ Pacer ─ compositor input
            └ audio ─ SwDecoder        ─ Queue ─ Pacer ─ mixer input
```

Both `Pacer`s wait against the *same* clock, the pipeline's own, which is what keeps the picture and the sound together: each branch is released at its own media timestamp measured from one shared origin. Two pipelines would each anchor a t=0 at whenever they happened to start, which is A/V drift built in.

Video decodes on the GPU straight into the surfaces the compositor draws from. Both compositors take NV12 device frames directly, so nothing converts between the decoder and the layer and the picture never reaches system memory. Audio has no such path and does not want one — it is decoded in software and registered with the mixer as one more input, alongside the desktop and microphone channels. That is where its sound goes and therefore where a recording picks it up — and, if it is being monitored, where it is played back from; see "Audio monitoring" below.

The audio has a channel in the Audio Mixer dock, with a fader and a mute button of its own. That widened what the dock is drawn from, and the widening is the point: "global" was always a claim about the *devices*, not about the dock. A microphone must not stop when the Scene changes; a file's sound has no business playing from a Scene nobody is looking at. So the dock draws the global devices, plus the audio-bearing Sources of the Scene being shown — which is what OBS does, and for the same reason.

The two lists are brought together in `ui::panels::audio_mixer`, where they are drawn, rather than being made one thing in the project. They are two different things there and the audio thread must not see the second: it opens a capture for every item in the `AudioSnapshot` it is handed, and a media file has no device to open.

The fader itself lives in the media file's own pipeline as an `AudioVolume`, between its `Pacer` and the mixer input. Not on the audio thread, because the sound is this Source's rather than a device's — which also means a live drag reaches it through the *engine* (`UiAction::DragMediaGain`) where a device's reaches the audio graph, though both then record one edit when the gesture ends.

A channel is drawn only where there is sound to fade, and there are three ways for that not to be so: the file had no audio stream when it was picked, the SceneItem is hidden, or its Source is not running. Hiding is the interesting one — it silences the Source as well as taking it out of the picture. One state with two effects rather than two states to keep in step: unhiding must not have to remember what the mute button was before, and a Source that is not in the picture has no channel to unmute it from either.

The meter is measured the same way a device's is — an `AppSink` on a `Tee` off the fader, writing one `AtomicU32` the UI thread reads without a lock — but it is *published* separately, and that is the part worth knowing. The audio thread owns `Levels` for the devices and replaces it wholesale whenever it rebuilds its graph; a media file's counters live on the video thread. Had they shared one map, changing a microphone would have wiped every file's meter as a side effect. So there are two publishing points, `AudioManager::peak_db` and `EngineManager::media_peak_db`, and the dock reads both — the same split, for the same reason, as the two snapshots its channel list comes from.

Both meters are post-fader: what they show is what the fader let through rather than what arrived at it, so pulling one down empties its meter and so does muting.

A file that is not looping plays once, sends EOS, and its layer ends with it — the compositor input detaches, so what shows through is whatever was behind it rather than a frozen last frame. Looping is what makes it not finish, and it is switched through `media-pp`'s `FileDemuxerHandle` rather than by reopening — so turning it on or off does not restart what is playing, and turning it off part way through lets the current pass reach the end of the file. Timestamps climb across a loop's join rather than restarting at zero, which is what keeps the `Pacer` pacing the second pass instead of dumping it as fast as it can be read.

Where it has reached is measured on the video branch: a `Tee` after the `Pacer` hands each frame to the compositor and to an `AppSink` that records its timestamp. The loop's accumulated offset is taken off there rather than by the reader, because the two are only comparable at the instant a frame is stamped — `FileDemuxerHandle::lap_offset` is what makes that subtraction possible, and it is why a second lap reads from the start again instead of from the end of the first.

The transport takes two lines: the play button and the clock on one, the bar on its own beneath them. The first two are the same width whatever the dock is, so sharing a line left the bar with whatever remained — nothing, at the dock's minimum width, and a bar squeezed to its floor is drawn past the panel's edge where it cannot be reached. The scrub bar writes nothing to the project. Where a clip is playing from is not something to record the way a Transform is, so it goes straight to the engine, and only when the drag ends — a seek is a `Pause -> Flush -> Seek -> Preroll` cascade rather than something to do on every frame of a gesture. Seeking a paused Source leaves it paused and still updates the picture: the preroll waits for every terminal to report its first new-timeline sample, and the pipeline then restores the state the caller asked for.

Pausing *is* written down, because it has to survive the Scene changing and the application being restarted — a paused clip that started itself again on the next launch would be a surprise. That makes running and showing two questions where they used to be one: leaving the Scene still stops a Source, but a media file can also be paused while its item is right there, so `reconcile` decides what should be running from both together and what should be hidden from the Scene alone.

That end is noticed. `notice_ended_media` asks the running media file Sources whether their pipeline has finished, the same way `notice_closed_windows` asks about windows, and puts the ones that have into `SourceState::Ended` — which the Sources dock reports beside the item. Two things make this worth a state of its own rather than leaving it `Open`. The Source has to be *stopped*: `Stop` is what takes its input off the audio mixer, which an `Eos` alone leaves registered and silent, so its channel would otherwise sit in the dock for as long as the SceneItem existed. And the badge is not the disconnected one — nothing went wrong, so it neither warns nor offers to reopen. Playing once is what a file that is not looping was asked to do.

The engine not reopening it is not the same as it being unplayable. Pressing play in the Properties dock, or dragging its bar, asks for `ReopenSource` — the same request the Sources dock makes for a disconnected capture, and the difference between the engine deciding to start something again and someone asking it to. The pause flag is cleared with it, because a reopened Source reads that and would otherwise come back stopped.

A path that is not there right now — a moved file, an unmounted drive — is the same kind of state a closed window is: the Source is left `Missing` and looked for again on the idle tick, rather than failed. A file that *is* there and will not demux is a real failure.

A file with no video stream is refused. This is a Scene Source and occupies a rectangle on the Canvas, so there is nothing for a sound-only file to be.

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
│   ├─ window_capture/             (a search here, the same portal there)
│   └─ video_capture/              (Windows only: Media Foundation)
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

### Opening a Source happens off the engine loop

Opening one is neither quick nor bounded. A portal capture waits on a dialog the user may leave standing for a minute; a file comes off a disk that may have spun down; a network stream waits out a connect timeout, which is five seconds of nothing for a camera that is switched off. On the engine loop each of those is the whole engine stopped for as long as it takes — no layer moves, no recording starts, no command is read — and the user sees an application that has hung, with no way to tell which Source did it.

So `SourceOpener` holds a thread, and the loop asks it. The reply comes back as `EngineCommand::Opened` through the channel the loop already reads, so the answer is applied where every other change is and the state machine stays in one place. One thread rather than one per request: opening was sequential before, and two portal captures asked for at once would otherwise put two pickers on the screen together.

`SourceState::Opening` is what the wait looks like from the engine's side. It stops the same Source being asked for twice while the first attempt is still connecting, and it gives what comes back somewhere to land. It is *not* published to the UI — a badge beside every item for as long as its capture takes to start would report trouble on the way to everything working.

The wait is not held open. A Scene can change, an item can be deleted, and the same Source can be asked for again, all while one is still opening; so a reply is installed only where the slot still says `Opening`, and a Source that arrives with nowhere to go is stopped where it lands rather than left running with nothing holding it. One that does land is placed where its item stands *now* rather than where it stood when it was asked for.

The backend is shared with that thread, which is why `Backend` has to be `Send + Sync` on every platform — asserted by a test in `engine::backend` so a field that is not says so at compile time rather than by the engine mysteriously blocking again.

### A stream that stopped arriving

`RtspSource` does not reconnect, deliberately: a read that fails ends it with an error and the pipeline finishes, which — a pipeline being one-shot — means coming back is a *new* one. That is the engine's job rather than the element's, and it is the same shape as a window that closed: `notice_dropped_streams` asks the running stream Sources whether their pipeline has ended, and puts the ones that have back to `Missing`.

What is different is how long the wait is. A window's search costs nothing and runs on the idle tick; a reconnect is a request to somebody else's machine, and a camera that is rebooting wants to be left alone for a moment. So the interval is stored per Source and `retry_after` reads it, with the tick as the granularity rather than the answer. Off is a value too: a stream told not to reconnect is `Disconnected` rather than `Missing` — `needs_asking` covers both it and a portal window — so the Sources dock offers it and nothing goes back to that address until someone asks.

The transport is stored beside it because there is no default that works everywhere. TCP interleaves the media in the control connection and crosses a firewall; UDP is lower latency on a network you control and nothing at all where those ports do not get through. Changing it reopens the Source, since a transport is negotiated when the session opens.

An address that cannot be reached *while it is being added* is refused rather than accepted — somebody is standing at the dialog, and a typed address that is wrong is likelier than a camera that happens to be off at that exact second. After that, not answering is an ordinary state the Source waits out.

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

## Crop

A SceneItem stores how much of its Source to leave out, on each of the four edges, in the Source's own pixels. Its own rather than the Canvas's, so a crop survives the item being resized afterwards — the same reason a Drawing's strokes are stored in the Drawing's coordinates.

`media-pp`'s `VideoLayer::source` is what makes it real: the compositor draws that region and fits *it* into the item's rectangle, so cropping a 16:9 capture square and containing it in a square rectangle fills the square. Until that existed the crop was stored, honoured by the editor's geometry, and ignored by the compositor — a number describing something the recording did not do.

Alt held on a resize handle crops instead of resizing, which is the convention every editor with both gestures uses. The edge under the pointer moves and the opposite one stays put: cropping from the left while the whole item slid left would be aiming at a moving target, and the undragged edges are what a person lines the picture up against. The scale comes out of the gesture unchanged, because the rectangle and the region behind it shrank by the same amount.

While the gesture runs, what is being cut is drawn faintly behind the item rather than simply removed. A crop with nothing showing behind it is one you cannot judge — the edge being lined up is exactly what has just been hidden. It goes when the pointer comes up, because after that the crop *is* the picture.

Alt+double-click puts a crop back: on a handle, the edges that handle holds; inside the item, all four. The Properties dock carries the same four numbers, editable, because "a hundred pixels off the left" is not something a drag can be trusted with. Both paths go straight to the compositor while they move and to the project once, when they settle — the split every gesture in this application makes.

## The Properties dock

What the selected SceneItem is, as it currently stands: name and kind, its place and size on the Canvas, whether it is visible and locked, and then whatever its kind has to say — the monitor and its rectangle in the virtual desktop, a window's program and title, a Drawing's stroke count, a Color's colour, a media file's path and whether it loops.

It is a dock and not a dialog because the values move while they are read: dragging in the Preview changes the numbers here, and a dialog would have to be reopened to see it while covering the picture the numbers are about.

Most of it reports rather than asks. Everything shown is already settable somewhere — a Transform by dragging, visibility and lock by the Sources dock's icons — and this says what those came out as, in numbers a drag cannot be precise about. What does ask falls into two shapes. A value with a gesture behind it is applied live and recorded once: `UiAction::DragSourceColour` goes straight to the engine so a Color's layer changes under the pointer, and one `SourceCommand::SetColor` is written when the picker is let go. A live gesture is one edit in the project, not sixty. A value with no gesture to wait out is written where it is touched — a media file's loop switch through the demuxer's own handle rather than by reopening the Source, a stream's transport by reopening it, since a transport is negotiated when the session opens.

Crop is the first shape again, and the reason it is here at all is that "a hundred pixels off the left" is not something a drag can be trusted with. Four spinners, kept inside the picture by the same `clamp_crop` the Preview's gesture uses, so a number typed here and an edge dragged there cannot disagree about what is legal. The picture follows every keystroke through `DragSceneItem`, and one `SourceCommand::SetCrop` is recorded when a field is let go. It was absent until `media-pp` grew `VideoLayer::source`, because until then a crop shown here would have described something the recording did not do.

## Audio monitoring

Playing the mix back to the person running obs-rs, which the mixer did not do at all until it did. Two settings make it, and neither is much use without the other.

**Where it goes** is one endpoint, chosen in Settings → Audio, and it starts as *none* rather than as the system default. That is the whole point of the setting. Desktop Audio is captured by listening to a playback endpoint — WASAPI loopback, a PipeWire monitor — so monitoring into that same endpoint hands the capture its own output, and every pass round adds delay and gain until it howls. The default output is almost always the endpoint Desktop Audio is on, so defaulting to it would ship the loop switched on. Somebody has to say "these headphones", and `engine::audio::device::open_renderer` therefore names its endpoint exactly and does not fall back the way `open_capture` does: a monitoring device that was unplugged stops monitoring, which is quiet, instead of falling back to the speakers, which is not.

The one mistake left is choosing that endpoint anyway, and the page says so where it is being made rather than letting it be discovered by ear. A source that stored no endpoint follows the system default, so the check counts that too — which is exactly the state somebody who never opened the picker is in.

**What is monitored** is per source, and it is one flag rather than the three states OBS has. Heard and recorded look like independent questions, and OBS answers them separately — off, heard, heard-and-recorded. Here they collapse, because obs-rs monitors by *playing* and captures the desktop by *listening to what is played*: a source kept out of the recording and sent to the speakers arrives back through Desktop Audio, late and having been through a speaker. "Heard but not recorded" is only true where the monitoring endpoint is one nothing captures, which is not the machine most people have — so it is not offered. Everything monitored is recorded; the flag says whether you also hear it.

The exclusion could be made real by capturing the desktop with the process loopback Windows 10 2004 added, excluding obs-rs's own process tree. That would also retire the feedback warning above. It has not been done, and until it is, the third state would be a promise this cannot keep.

There are still two mixers, because the two sums still differ: what is monitored is a *subset* of what is recorded. A branch off the recording's mix would play all of it — the desktop, and your own voice back at you. Both run from startup whether or not anything is set: summing nothing is cheap, and a mixer built later is one every source would have to be registered with again.

A source's `Tee` therefore has up to three branches — the meter, the recording's mix, the monitor mix. The monitor branch registers under a name of its own (`audio-1-monitor`), which buys nothing at run time and everything in a log: two branches into two elements with one name between them cannot be told apart in a topology diagram.

**A device channel settles its branches when the `Tee` is built**, so changing a mode reopens that one capture — the same cost and the same shape as changing its device, and a capture that restarts is a few milliseconds nobody hears.

**A Source that carries its own sound cannot afford that.** Reopening a media file restarts it from the beginning, which is a large thing to do to somebody who only asked to hear it. So its `Tee` is dynamic and `sound::SoundRouting` puts the monitor's branch on and takes it off while the clip plays. The meter's branch and the recording's are permanent, which is what makes this one branch rather than two: a Source is recorded whether or not anybody is listening to it.

That is also what a live stream's sound goes through, though nothing yet gives one the flag: it answers `false`, and its branch on the recording's mix is where it has always been.

**Which channels offer the control** is not all of them. An output — Desktop Audio — is captured by listening to what is already being played on it, so it is audible before obs-rs touches it; monitoring one would be hearing the same sound twice, once late, and what that leaves of the three modes is "recorded or not", which the mute button already is. `AudioSourceKind::can_be_monitored` says so, and the dock draws no button where it answers `false`. A media file is the opposite and the reason the control exists: its sound is made inside obs-rs and reaches no speaker any other way.

Between the monitor mix and the endpoint sit a `Queue` and an `AudioResampler`. The resampler is not optional — both renderers require the endpoint's own format and convert nothing — and the queue's policy is `DropNewest` where the recording branch's is `Block`. That inversion is the point of having a second one: a playback device that stalls must not stall the mix a file is being written from. A buffer too late to play has no use left; a recording has to be complete.

With no monitoring device set, every mode wires as `Off`. A mode chosen in one dialog must not silently drop a channel from the file because of a setting in another, with nothing on screen connecting the two.

## Recording output

A recording is two tracks: the compositor's frames encoded as H.264, and the audio mixer's output as AAC or Opus. Neither half owns the file — `engine::recording` does — because an mp4's tracks are fixed before its header is written, so both encoders have to exist before a frame is written to either, and the trailer is written only once *every* track has reported done. Ending one and not the other leaves a file exactly as long as it is unplayable.

A machine whose mixer never started records video alone rather than refusing, so the track list is decided per recording from what is actually running.

Each track is zeroed by its own `TimestampOrigin`, because there is no clock the two share: the compositor counts composed frames and the mixer counts emitted samples, both since launch and on unrelated counters. Measured against a clip whose flash and beep are simultaneous, the two land within one video frame of each other, and a two-minute recording drifts 29 ms end to end — so nothing here needs a shared clock.

The container is chosen on the Recording page and is very nearly a choice of extension: `media-pp`'s `FileMuxer` asks FFmpeg to guess a muxer from the file name, so MP4 and Matroska are the same element told a different path. Matroska is worth choosing for a long recording — MP4 is finalized in its trailer, so a session that dies with the application leaves an unplayable file, where a `.mkv` plays up to where it stopped.

HLS is the third option and a different element. It writes a VOD playlist and fMP4 segments into a directory named for the recording, so a session is servable as it stands and every completed segment is on disk before the recording ends. fMP4 rather than MPEG-TS because Opus is one of the audio encoders offered here, and TS carries it badly.

One recording can also be cut into several files, by elapsed time or by bytes written. Either way the cut lands on the next keyframe rather than on the figure itself, so every file opens on its own — which means a file runs past what was asked for by up to one GOP. Splitting is disabled for HLS, which is already segmenting on its own target duration.

## Status bar

CPU is this process's share of total machine capacity. GPU prefers a per-process figure and falls back to whole-adapter usage, marked with a trailing `*`, when the platform offers no per-process counter — NVIDIA's Linux driver exposes neither `drm-engine-*` fdinfo nor working per-process NVML samples on GeForce parts, so a device figure is all that exists there. Hovering the reading names its scope.

MEM is what is in memory for this process alone — the private working set on Windows, `RssAnon` on Linux. That is the figure a task manager shows, which is why it is the one on the bar: a reading nobody can check against anything is worse than no reading. Hovering it adds what the process has *claimed*, which on this machine is three times larger (171 MB against 507 MB in a release build) and is the steadier of the two, since an operating system trims a working set whenever it wants the pages back.

Windows has no plain call for the private working set — `GetProcessMemoryInfo` reports the whole one, shared pages included, about a third high. It comes from the `Process V2` performance counter instead, whose instance names carry the process id the way the GPU counters do; a Windows without that counter set falls back to the whole working set.

The GPU is in neither figure, which is worth knowing before reading these as the application's whole appetite. Measured here: removing two 1080p Display Captures freed 18 MB of dedicated video memory and moved the private figures by one megabyte. Dedicated VRAM is simply not charged to the process — only whatever system memory the driver uses alongside it, which was 95 MB of the 507. What the rest of that claim is has not been accounted for, and this bar does not pretend to say.

The recording clock is the one reading with a colour. It is red while a recording is running, the convention every recorder shares and the thing that makes a running clock findable in a row of grey figures; the warning colour while one is paused, because a still figure and a stalled application look alike for the first few seconds; and unmarked while nothing is recording, when `REC --:--:--` is a placeholder holding its own width rather than a state. The hover says which of the three it is, so the colour is not carrying the meaning alone.

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
