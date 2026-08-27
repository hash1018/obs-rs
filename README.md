# obs-rs

An OBS-style live broadcasting and recording application written in Rust.

## Preview terminology

The following terms are kept distinct so that `Preview` does not ambiguously refer to the UI workspace, logical coordinate system, and composited output.

| Term | Meaning | Representative code name |
|---|---|---|
| Preview Workspace | The entire central editing area excluding docks | `CentralPanel` in `ui::preview::show` |
| Preview Viewport | The on-screen 16:9 rectangle that displays the composited output | `viewport_rect` |
| Scene Canvas | The fixed logical coordinate space in which SceneItems are placed | `SceneCanvas` |
| Composite Frame | The final frame produced by compositing Sources | Future `CompositeFrame` |
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

The Compositor and Composite Frame are not implemented yet, so the Preview Viewport currently displays an empty frame. The UI does not directly composite Sources inside the Viewport. It only renders the selected SceneItem's Canvas overflow and Transform Gizmo as editor overlays. Once the Compositor produces a GPU texture-backed Composite Frame, Preview will display that texture together with the Editor Overlay.

## Current source support

- Color Source is persisted and can be moved and resized in the editor.
- Display Capture can enumerate monitors on Windows and Linux/X11, persist the selected monitor name, and create a SceneItem. On Wayland, source creation opens the system-owned `xdg-desktop-portal` picker and persists the restore token it issues. Runtime capture is not connected on either platform, so it does not produce Preview pixels.

A Display Capture source stores one of two targets, and neither platform can produce the other:

| Platform | Stored target | Reproduced by |
|---|---|---|
| Windows, Linux/X11 | Monitor name (`\\.\DISPLAY1`, `DP-1`) | Resolving the name against the live display layout |
| Wayland | `xdg-desktop-portal` restore token | Reopening a portal session with the token |

Wayland never names a display: the portal owns the picker and returns only what the user chose. A stream id belongs to the session that produced it, so the restore token is the only value that reproduces a selection in a later run. The compositor may decline to issue one, which is not an error — capture then shows the picker again instead of restoring silently.

Monitor geometry is intentionally not persisted. The runtime capture layer will resolve the saved monitor name against the current display layout; on Windows it will open `DxgiCaptureSource` in GPU mode. GPU mode is an application invariant rather than a user setting and does not currently support cursor inclusion.

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

The built-in FTL files are embedded in the executable with `include_str!`; they are not required beside the executable at runtime. Korean glyphs use a CJK system font registered as an egui fallback. If distribution must not depend on system fonts, add a suitably licensed font to the application assets.
