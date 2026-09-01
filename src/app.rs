use std::sync::Arc;
use std::time::Duration;

use eframe::egui;

use crate::capture::AudioDeviceTarget;
use crate::domain::SceneCanvas;
use crate::engine::{AudioManager, EngineManager};
use crate::i18n::{LocalizationManager, install_locale_fonts};
use crate::project::{ProjectManager, ProjectUpdate};
use crate::resources::ResourceManager;
use crate::settings::{AppSettings, SettingsStore, WindowGeometry};
use crate::snapshots::Snapshots;
use crate::ui::{self, UiAction, UiState};

#[cfg(target_os = "linux")]
use crate::capture::linux::{SystemDisplayPicker, SystemDisplayPickerUpdate};

pub struct ObsApp {
    ui_state: UiState,
    snapshots: Snapshots,
    project_manager: Option<ProjectManager>,
    resources: Option<ResourceManager>,
    engine: Option<EngineManager>,
    /// The audio graph. Separate from the engine because it neither needs a
    /// GPU nor should be lost when one is missing — see `engine::audio`.
    audio: Option<AudioManager>,
    #[cfg(target_os = "linux")]
    system_display_picker: Option<SystemDisplayPicker>,
    localization: LocalizationManager,
    settings: AppSettings,
    settings_store: SettingsStore,
    ui_actions: Vec<UiAction>,
    /// Set once the user has answered the closing question, so the close it
    /// sends is let through.
    exiting: bool,
    /// The window as it last was when it was neither maximized nor
    /// minimized, updated every pass and written on the way out.
    ///
    /// Tracked rather than read at exit because by then there is no `Context`
    /// to read it from — `eframe::App::on_exit` is handed nothing.
    window: Option<WindowGeometry>,
    window_maximized: bool,
    /// What the engine was last told about whether anyone can see the
    /// Preview — see [`ObsApp::poll_engine`].
    preview_visible: bool,
    /// Every audio endpoint the mixer can offer.
    ///
    /// Taken once at startup rather than per frame: enumerating opens the
    /// audio subsystem, and a list that is a device or two out of date until
    /// the next launch is a smaller cost than doing that sixty times a
    /// second. Refreshing on device change is what a hotplug notification
    /// would be for, and neither backend offers one here yet.
    audio_devices: Arc<Vec<AudioDeviceTarget>>,
}

/// What the engine's wake asks egui to wait before repainting — which is
/// as good as nothing, and deliberately not nothing.
///
/// `Context::request_repaint` is `request_repaint_after(ZERO)`, and egui
/// answers a zero delay with *two* passes rather than one, "to give some
/// things time to settle". That is a reasonable default for a repaint caused
/// by an interaction whose response is only known a pass later. It is not one
/// for this: a composited frame has arrived, and drawing it twice is drawing
/// the whole UI a second time for nothing. Doing that thirty times a second
/// was the application's single largest cost — 64% of a core against 7%.
///
/// Any non-zero delay takes egui's single-pass path, and it then subtracts a
/// predicted frame time from whatever it was given, so this one arrives as
/// "repaint now". The Preview is no slower for it; it is drawn once.
const REPAINT_NOW: Duration = Duration::from_nanos(1);

impl ObsApp {
    /// Both the store and what was in it come from `main`, which has already
    /// read the file: the window has to be placed before there is one to put
    /// the settings in, so reading it again here would be reading it twice.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings_store: SettingsStore,
        settings: AppSettings,
    ) -> Self {
        // Before the first pass draws, so the window never appears in one
        // palette and switches to another.
        cc.egui_ctx.set_theme(settings.theme);
        install_locale_fonts(&cc.egui_ctx);
        // The docks and the Preview zoom come back as they were left; nothing
        // else about the session does.
        let ui_state = UiState::restored(&settings.workspace.docks, &settings.workspace.preview);
        if let Err(error) = settings_store.save(&settings) {
            eprintln!("could not save app settings: {error}");
        }
        let localization = LocalizationManager::new(settings.locale);
        let engine_repaint_ctx = cc.egui_ctx.clone();
        let resource_repaint_ctx = cc.egui_ctx.clone();
        let project_repaint_ctx = cc.egui_ctx.clone();
        let audio_repaint_ctx = cc.egui_ctx.clone();
        #[cfg(target_os = "linux")]
        let picker_repaint_ctx = cc.egui_ctx.clone();

        // Built before the struct so the engine can be handed a dispatcher:
        // opening a capture Source can produce a fresher restore token, and
        // that belongs in the project rather than in this run's memory.
        let project_manager =
            ProjectManager::spawn(move || project_repaint_ctx.request_repaint()).ok();
        let project_dispatcher = project_manager.as_ref().map(ProjectManager::dispatcher);
        let recording_settings = settings.recording.clone();
        // Taken before the struct owns them, since the fields below are
        // where they end up.
        let saved_window = settings.workspace.window;

        // Before the engine, because the engine is handed where a recording's
        // audio track attaches and the mixer is what owns it. Without it the
        // mixer draws what the project holds and nothing is captured, which
        // is what this application did until now.
        let audio = AudioManager::spawn(mix_format(&settings), move || {
            audio_repaint_ctx.request_repaint()
        })
        .inspect_err(|error| eprintln!("could not start audio: {error}"))
        .ok();
        let mixer = audio.as_ref().and_then(AudioManager::mixer);

        Self {
            ui_state,
            snapshots: Snapshots::default(),
            project_manager,
            resources: ResourceManager::spawn(move || {
                resource_repaint_ctx.request_repaint();
            })
            .ok(),
            // Without the wgpu render state there is no device to composite
            // onto, so the preview stays empty rather than the app refusing
            // to start.
            engine: cc.wgpu_render_state.clone().and_then(|render_state| {
                EngineManager::spawn(
                    render_state,
                    SceneCanvas::DEFAULT,
                    project_dispatcher,
                    // Handed over at construction, not sent afterwards: the
                    // engine would otherwise hold its defaults until the
                    // Settings dialog was opened and applied, and a recording
                    // started before that would ignore everything the user had
                    // saved.
                    recording_settings,
                    mixer,
                    move || engine_repaint_ctx.request_repaint_after(REPAINT_NOW),
                )
                .inspect_err(|error| eprintln!("could not start the engine: {error}"))
                .ok()
            }),
            #[cfg(target_os = "linux")]
            system_display_picker: SystemDisplayPicker::spawn(move || {
                picker_repaint_ctx.request_repaint();
            })
            .ok(),
            localization,
            settings,
            settings_store,
            ui_actions: Vec::new(),
            audio_devices: Arc::new(crate::capture::audio_devices()),
            audio,
            exiting: false,
            // Filled in on the first pass, which happens before anything can
            // close the window.
            window: saved_window,
            window_maximized: saved_window.is_some_and(|window| window.maximized),
            // What the engine starts believing, so the first pass says
            // something only if the window came up minimised.
            preview_visible: true,
        }
    }

    fn poll_project(&mut self) {
        let Some(manager) = &self.project_manager else {
            return;
        };
        if let Some(update) = manager.latest() {
            match update {
                ProjectUpdate::Snapshot {
                    scenes,
                    sources,
                    audio,
                } => {
                    self.snapshots.scenes = scenes;
                    self.snapshots.sources = *sources;
                    self.snapshots.audio = audio;
                    if let Some(manager) = &self.audio {
                        manager.apply(&self.snapshots.audio);
                    }
                    if let Some(engine) = &self.engine {
                        engine.apply(&self.snapshots.sources);
                    }
                }
                ProjectUpdate::Error(error) => eprintln!("project database error: {error}"),
            }
        }
    }

    fn poll_engine(&mut self, ctx: &egui::Context) {
        let Some(engine) = &self.engine else {
            return;
        };
        self.snapshots.status.active_fps = engine.active_fps();
        self.snapshots.status.target_fps = Some(engine.target_fps(&self.settings.recording));
        // Read every pass rather than tracked here: the engine is what knows
        // whether a recording actually started, and the Controls dock reads
        // the same answer the status bar's clock does.
        self.snapshots.status.recording_elapsed = engine.recording();
        self.snapshots.status.recording_paused = engine.recording_paused();
        self.snapshots.status.recording_error = engine.recording_error();
        self.snapshots.status.source_status = engine.source_status();
        if self.snapshots.status.encoders.is_empty()
            && let Some(encoders) = engine.encoders()
        {
            self.snapshots.status.encoders = encoders.as_ref().clone();
        }
        if self.snapshots.status.audio_codecs.is_empty()
            && let Some(codecs) = engine.audio_codecs()
        {
            self.snapshots.status.audio_codecs = codecs.as_ref().clone();
        }

        // A minimised window is nobody looking at the Preview, and the engine
        // can stop putting frames where nobody will sample them. Only the
        // change is sent: this runs every pass, and the engine's queue is not
        // the place to say the same thing sixty times a second.
        //
        // This never reads `None`, and the `unwrap_or` is defensive rather
        // than a case that arrives: `egui_winit` already writes
        // `Some(window.is_minimized().unwrap_or(false))`, so a platform that
        // cannot answer reaches here as "not minimised" — which is the right
        // answer anyway, since hiding the Preview from someone who can see it
        // is the worse mistake.
        //
        // On Wayland that is every window: winit answers `None` there
        // ("clients don't know whether they are minimized or not" — the
        // protocol does not say), so this saving is unreachable on a Wayland
        // session and the Preview keeps drawing while minimised. X11 —
        // including XWayland — reports it and takes the saving. Verified by
        // running both: under XWayland a minimised window skipped 84
        // consecutive presents over eight seconds and drew none, and restoring
        // it copied the picture held from while it was down before resuming.
        let visible = !ctx.input(|input| input.viewport().minimized.unwrap_or(false));
        if self.preview_visible != visible {
            self.preview_visible = visible;
            engine.set_preview_visible(visible);
        }
    }

    /// Commits the Settings dialog's draft.
    ///
    /// Saved after it is applied, and a failed write does not undo any of it:
    /// the user asked for these settings now, and refusing them because they
    /// could not also be remembered would be the worse of the two failures.
    fn apply_settings(&mut self, ctx: &egui::Context, settings: AppSettings) {
        if settings.locale != self.settings.locale {
            self.localization.set_locale(settings.locale);
        }
        if settings.theme != self.settings.theme {
            ctx.set_theme(settings.theme);
        }
        // Read when a recording starts, so this reaches the next one rather
        // than any that is running.
        if let Some(engine) = &self.engine {
            engine.set_recording_settings(settings.recording.clone());
        }
        // The mixer's own format, which takes immediately — so it is refused
        // while a recording runs, the same as the frame rate and for the same
        // reason: the running file's audio encoder was opened for the old one.
        if settings.audio != self.settings.audio
            && self.snapshots.status.recording_elapsed.is_none()
            && let Some(audio) = &self.audio
        {
            audio.set_mix_format(mix_format(&settings));
        }
        self.settings = settings;
        if let Err(error) = self.settings_store.save(&self.settings) {
            eprintln!("could not save app settings: {error}");
        }
        ctx.request_repaint();
    }

    /// Holds the window open when closing it would end a recording the user
    /// may not have realised was running.
    ///
    /// Only a question, not a rule: the answer is always available and the
    /// second attempt goes straight through. `close_requested` is asked every
    /// pass because it is how egui reports the window's own close button, the
    /// window manager's, and `UiAction::Exit` alike — intercepting one of
    /// those and not the others would leave a way out that skipped the
    /// question.
    /// Notes where the window is, so closing can write it down.
    ///
    /// Only while it is neither maximized nor minimized. A maximized window's
    /// outer rect is the screen, and restoring *that* as a normal window
    /// would lose the size the user actually chose — so the flag is kept
    /// beside the last ordinary rect rather than instead of it. A minimized
    /// one reports a position that is not where it will reappear.
    fn remember_window(&mut self, ctx: &egui::Context) {
        ctx.input(|input| {
            let viewport = input.viewport();
            self.window_maximized = viewport.maximized.unwrap_or(false);
            if self.window_maximized || viewport.minimized.unwrap_or(false) {
                return;
            }
            // The size from what egui is drawing into rather than from the
            // viewport's rect. Both of that rect's corners come from a window
            // position, which Wayland will not report — so `inner_rect` is
            // `None` there and a rect-only reading remembered nothing at all,
            // not even a size the platform is perfectly able to restore.
            let Some(size) = input.raw.screen_rect.map(|rect| rect.size()) else {
                return;
            };
            if size.x <= 0.0 || size.y <= 0.0 {
                return;
            }
            // Outer for the position, because that is what a window manager is
            // asked to place. Absent where the platform will not say — see
            // `WindowGeometry`.
            let position = viewport.outer_rect.map(|outer| outer.min);
            self.window = Some(WindowGeometry {
                x: position.map(|position| position.x),
                y: position.map(|position| position.y),
                width: size.x,
                height: size.y,
                maximized: false,
            });
        });
    }

    /// Writes down where the window was and how the docks were arranged.
    ///
    /// Once, on the way out. Doing it as either changes would rewrite the
    /// file on every frame of a drag, for a value only the next startup
    /// reads.
    fn save_workspace(&mut self) {
        // A position this session could not observe is left as it was found,
        // not cleared. Wayland never reports one, and wiping the file there
        // would lose a position an X11 session had set — where keeping it
        // costs nothing, since a session that *can* place windows overwrites
        // it every time anyway.
        let saved = self.settings.workspace.window;
        self.settings.workspace.window = self.window.map(|window| WindowGeometry {
            maximized: self.window_maximized,
            x: window.x.or_else(|| saved.and_then(|saved| saved.x)),
            y: window.y.or_else(|| saved.and_then(|saved| saved.y)),
            ..window
        });
        self.settings.workspace.docks = self.ui_state.docks();
        self.settings.workspace.preview = self.ui_state.preview_zoom();
        if let Err(error) = self.settings_store.save(&self.settings) {
            eprintln!("could not save the workspace layout: {error}");
        }
    }

    fn intercept_close(&mut self, ctx: &egui::Context) {
        if !ctx.input(|input| input.viewport().close_requested()) {
            return;
        }
        // Paused counts: the file is open either way, and someone who paused
        // to deal with something else is exactly who would forget.
        let recording = self.snapshots.status.recording_elapsed.is_some();
        if !recording || self.exiting {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.ui_state.confirm_exit();
    }

    /// Fills in each mixer channel's level, and whether it has anything
    /// behind it at all.
    ///
    /// Read every pass rather than pushed with the snapshot: a peak changes
    /// with the audio, not with the project, and the project publishes only
    /// when something is edited. Whether a source is running changes with
    /// neither — it changes when a device is plugged in — so it arrives the
    /// same way.
    fn poll_audio_levels(&mut self) {
        let Some(manager) = &self.audio else {
            return;
        };
        for source in &mut self.snapshots.audio.items {
            source.peak_db = manager.peak_db(source.id);
            // Left as it was until something has actually been published, so
            // the docks do not blink empty on the first pass.
            if let Some(running) = manager.is_running(source.id) {
                source.running = running;
            }
        }
        if let Some(devices) = manager.devices() {
            self.audio_devices = devices;
        }
    }

    fn poll_resource_usage(&mut self) {
        let Some(manager) = &self.resources else {
            return;
        };
        if let Some(usage) = manager.latest() {
            self.snapshots.status.cpu_percent = usage.cpu_percent;
            self.snapshots.status.gpu = usage.gpu;
            self.snapshots.status.memory = usage.memory;
        }
    }

    #[cfg(target_os = "linux")]
    fn poll_system_display_picker(&mut self) {
        let Some(picker) = &self.system_display_picker else {
            return;
        };
        match picker.latest() {
            Some(SystemDisplayPickerUpdate::Selected { scene_id, settings }) => {
                if let Some(manager) = &self.project_manager {
                    manager.dispatch(crate::project::ProjectCommand::Source(
                        crate::project::SourceCommand::AddDisplayCapture { scene_id, settings },
                    ));
                }
            }
            Some(SystemDisplayPickerUpdate::Error(error)) => {
                eprintln!("system display picker error: {error}");
            }
            Some(SystemDisplayPickerUpdate::Cancelled) | None => {}
        }
    }

    fn handle_ui_action(&mut self, ctx: &egui::Context, action: UiAction) {
        match action {
            UiAction::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            UiAction::StopRecordingAndExit => {
                if let Some(engine) = &self.engine {
                    // Through the ordinary stop, so the muxer sees an `Eos`
                    // and the encoder flushes what it was holding. Tearing the
                    // pipeline down would finalize the file too — on
                    // `ControlMsg::Stop`, which abandons rather than drains,
                    // and the last frames go with it.
                    engine.stop_recording();
                }
                // So the close this sends is not intercepted again.
                self.exiting = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            UiAction::Project(command) => {
                if let Some(manager) = &self.project_manager {
                    manager.dispatch(command);
                }
            }
            UiAction::OpenSystemDisplayPicker(scene_id) => {
                #[cfg(target_os = "linux")]
                if let Some(picker) = &self.system_display_picker {
                    picker.open(scene_id);
                }
                #[cfg(not(target_os = "linux"))]
                let _ = scene_id;
            }
            UiAction::DragAudioGain(id, gain_db) => {
                if let Some(audio) = &self.audio {
                    audio.set_gain_db(id, gain_db);
                }
                // And into the snapshot, which is what the dock's readout
                // reads. Nothing else will put it there mid-gesture: the
                // project is not told until the fader is let go, so a readout
                // waiting for that would sit at the old number while the
                // level under it moved.
                //
                // Safe to write over: `poll_project` replaces this only when
                // the project actually changes, which during a drag it does
                // not — and the edit that lands on release carries this same
                // value.
                if let Some(source) = self
                    .snapshots
                    .audio
                    .items
                    .iter_mut()
                    .find(|source| source.id == id)
                {
                    source.gain_db = gain_db;
                }
            }
            UiAction::DrawStrokes(item_id, strokes) => {
                if let Some(engine) = &self.engine {
                    engine.set_drawing_strokes(item_id, strokes);
                }
            }
            UiAction::DragSourceColour(item_id, rgba) => {
                if let Some(engine) = &self.engine {
                    engine.set_source_colour(item_id, rgba);
                }
            }
            UiAction::DragMediaGain(item_id, gain_db) => {
                if let Some(engine) = &self.engine {
                    engine.set_media_gain_db(item_id, gain_db);
                }
                // And into the snapshot the dock's readout reads, for the
                // same reason `DragAudioGain` does it: the project is not
                // told until the fader is let go, and a readout waiting for
                // that would sit at the old number while the level moved.
                if let Some(item) = self
                    .snapshots
                    .sources
                    .items
                    .iter_mut()
                    .find(|item| item.id == item_id)
                    && let crate::domain::SourceSettings::MediaFile(settings) = &mut item.settings
                {
                    settings.gain_db = gain_db;
                }
            }
            UiAction::DragSceneItem(item_id, transform) => {
                if let Some(engine) = &self.engine {
                    engine.set_dragging_transform(item_id, transform);
                }
            }
            UiAction::SetFullscreen(fullscreen) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
            }
            UiAction::ReopenSource(item_id) => {
                if let Some(engine) = &self.engine {
                    engine.reopen_source(item_id);
                }
            }
            UiAction::StartRecording => {
                if let Some(engine) = &self.engine {
                    engine.start_recording();
                }
            }
            UiAction::SetRecordingPaused(paused) => {
                if let Some(engine) = &self.engine {
                    engine.set_recording_paused(paused);
                }
            }
            UiAction::StopRecording => {
                if let Some(engine) = &self.engine {
                    engine.stop_recording();
                }
            }
            UiAction::ShowRecordings => {
                // The configured folder rather than the default one: what the
                // user is looking for is where their recordings actually go.
                let directory = self.settings.recording.directory_or_default();
                if let Err(error) = crate::paths::show_in_file_manager(&directory) {
                    eprintln!("could not show {}: {error}", directory.display());
                }
            }
            UiAction::OpenSettings => {
                // Seeded here rather than in the dialog: this is what holds
                // the live settings, and a draft taken from anywhere else
                // could be stale.
                self.ui_state.open_settings(&self.settings);
            }
            UiAction::ApplySettings(settings) => self.apply_settings(ctx, *settings),
            UiAction::SetTheme(theme) => {
                // Through the same path the dialog takes, so the menu's
                // immediate change is also the one that gets remembered.
                let mut settings = self.settings.clone();
                settings.theme = theme;
                self.apply_settings(ctx, settings);
            }
            UiAction::SetLocale(locale) => {
                self.localization.set_locale(locale);
                self.settings.locale = locale;
                if let Err(error) = self.settings_store.save(&self.settings) {
                    eprintln!("could not save app settings: {error}");
                }
                ctx.request_repaint();
            }
        }
    }
}

impl eframe::App for ObsApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.intercept_close(ctx);
        self.remember_window(ctx);
        self.poll_project();
        self.poll_engine(ctx);
        self.poll_resource_usage();
        self.poll_audio_levels();
        #[cfg(target_os = "linux")]
        self.poll_system_display_picker();
    }

    fn on_exit(&mut self) {
        self.save_workspace();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_actions.clear();
        // Held for the whole draw so the texture cannot be swapped out from
        // under the painter mid-frame.
        let composite_frame = self.engine.as_ref().and_then(EngineManager::frame);
        let resources = ui::UiResources::new(
            &self.snapshots,
            &self.settings,
            &self.audio_devices,
            &self.localization,
            composite_frame.as_deref(),
        );
        ui::show(ui, &mut self.ui_state, &resources, &mut self.ui_actions);

        let ctx = ui.ctx().clone();
        for index in 0..self.ui_actions.len() {
            let action = self.ui_actions[index].clone();
            self.handle_ui_action(&ctx, action);
        }
        self.ui_actions.clear();
    }
}

/// The mix format a settings file asks for.
///
/// Here rather than a `From` on `AudioSettings`, because the conversion is
/// one way and only this crate's boundary with `media-pp` needs it.
fn mix_format(settings: &AppSettings) -> media_pp::elements::MixFormat {
    media_pp::elements::MixFormat {
        sample_rate: settings.audio.sample_rate.max(1),
        channels: settings.audio.channels.max(1),
    }
}
