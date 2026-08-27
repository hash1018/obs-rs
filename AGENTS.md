# AGENTS.md

This file defines repository-specific guidance for coding agents working on `obs-rs`.

## Project goal

`obs-rs` is an OBS-style live capture, compositing, and recording application written in Rust with `eframe`/`egui` and the `wgpu` renderer. The project is being built incrementally: preserve the existing architecture and avoid implementing speculative engine behavior in the UI.

## Architecture boundaries

- `src/app.rs` is the application orchestrator. It polls managers, owns snapshots and application services, renders the UI, and handles `UiAction` values.
- `src/ui` contains immediate-mode presentation and editor interaction. UI code may emit actions but must not access SQLite or perform engine work directly.
- `src/project` owns project commands and the project worker. Scene and Source mutations flow through `ProjectCommand`.
- `src/persistence` owns SQLite access, stores, and migrations. Schema changes require a migration and persistence tests.
- `src/capture` decides what the user can pick: enumerating targets, or handing that job to a system-owned picker. It must not depend on `media-pp` — a picker runs before any pipeline exists.
- `src/engine` owns the compositor, capture Sources, and the frame handed to the Preview. It runs on its own thread and reconciles against the project snapshot; it never reads SQLite directly and changes the project only through `ProjectDispatcher`.
- `src/paths` answers where this user's files live. It is the only platform knowledge with no subject of its own; everything else platform-specific belongs beside what it implements.
- `src/resources` samples this process's CPU and GPU usage independently from the UI.
- `src/snapshots` contains read-only data presented to the UI.
- `src/domain` contains project concepts and must not depend on UI or localization details.
- `src/i18n` owns locales, translation keys, Fluent bundles, and UI font fallback configuration.
- `src/settings.rs` owns application preferences such as locale. Application preferences are not project database records.

When additional read-only UI inputs are required, extend `UiResources` instead of growing every `show` function's parameter list. Keep panel-specific mutable state in its dedicated UI state type.

## Adding a platform

Capture is deliberately split in two, and both halves need writing:

- `src/capture/<os>.rs` — what the user can pick. Enumerate targets, or return `SourcePicker::SystemDialog` where the system owns that choice. No `media-pp` here.
- `src/engine/source/<os>.rs` — how a stored target is opened, which needs `media-pp` and a live compositor handle. Selected by `#[cfg_attr(path)]` in `engine/source/mod.rs`; `unsupported.rs` is what a platform gets until it has one.

Keep a platform's code beside the thing it implements rather than gathering it into one tree by virtue of being platform-specific. `src/paths` is the exception, and only because it implements nothing else.

## UI action flow

The expected flow is:

```text
egui widget interaction
  → UiAction
  → ObsApp::handle_ui_action
  → ProjectManager, application service, or viewport command
  → updated Snapshot
  → next egui frame
```

Reuse the `ObsApp::ui_actions` buffer by clearing it. Do not allocate a new action vector on every frame. Transient drag state belongs in the UI; persist the final value once when an interaction finishes.

## Preview terminology and behavior

Use the terminology defined in `README.md` consistently:

- Preview Workspace: the complete central editor area.
- Preview Viewport: the rectangle that displays the final Composite Frame.
- Scene Canvas: the fixed logical output coordinate space, currently 1920×1080.
- Composite Frame: the compositor's output, resolved into one texture the Preview samples.
- Editor Overlay and Transform Gizmo: editor-only visuals that never enter output.

Do not composite Sources in UI code. The Viewport draws the Composite Frame the engine produced, and above it only editor-only overflow and gizmos. Window and dock resizing may change Viewport mapping but must not mutate SceneItem Canvas coordinates.

A drag moves the layer directly and writes the Transform to the project once, when the pointer is released. Do not route per-frame gesture values through `ProjectCommand`: a drag is one edit, not sixty.

## Docking and panels

- Each dock is represented by `DockPanel` and has its own panel module and state where needed.
- Multiple panels may share a region and must remain reorderable and resizable.
- Respect each panel's minimum size and existing drop-target geometry.
- Dock toolbar controls remain left-aligned and vertically centered.
- Preview itself is the fixed central area; docks resize the available Preview Workspace rather than becoming part of it.

## Localization

- Do not add hard-coded user-facing UI strings.
- Add a `TextKey` entry and translations to both `assets/locales/en-US/app.ftl` and `assets/locales/ko-KR/app.ftl`.
- English is the required fallback locale.
- Persisted user-provided names such as Scene and Source names are data and must not be translated during rendering.
- Built-in FTL resources are embedded with `include_str!`. Do not add `build.rs` asset copying unless the runtime resource model is intentionally changed.

## Rust and dependencies

- Keep the `wgpu` eframe renderer. The engine shares eframe's device rather than opening a second one, and the NV12 resolve pass runs on it.
- Compositing belongs on the GPU. `media-pp`'s CPU compositor is single-threaded and cannot hold the output rate for more than one layer.
- Prefer standard library facilities and existing dependencies when they are sufficient.
- Keep new dependencies narrowly scoped and compatible with the repository toolchain.
- Follow existing module visibility conventions (`pub(super)` and `pub(in crate::ui)`) rather than widening visibility unnecessarily.

## Verification

Run these checks after code changes:

`media-pp` is taken by path from the sibling checkout, and building needs FFmpeg 8.0+ development headers plus PipeWire development files on Linux.

```text
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
git diff --check
```

A platform file that no build here compiles is a platform build that breaks on someone else's machine. When changing `src/engine/source/unsupported.rs` or a `<os>.rs` this host does not build, point `engine/source/mod.rs`'s `#[cfg_attr(path)]` at it once and check that it compiles before putting it back.

The Windows capture enumeration test requires an interactive capturable window and can fail in a headless or restricted session. Report that environmental failure explicitly; do not weaken the test to hide it. Run focused tests for changed modules when the environment prevents the full suite from completing.
