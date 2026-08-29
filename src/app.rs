use std::time::Duration;

use eframe::egui;

use crate::capture::AudioDeviceTarget;
use crate::domain::SceneCanvas;
use crate::engine::EngineManager;
use crate::i18n::{LocalizationManager, install_locale_fonts};
use crate::project::{ProjectManager, ProjectUpdate};
use crate::resources::ResourceManager;
use crate::settings::{AppSettings, SettingsStore};
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
    #[cfg(target_os = "linux")]
    system_display_picker: Option<SystemDisplayPicker>,
    localization: LocalizationManager,
    settings: AppSettings,
    settings_store: SettingsStore,
    ui_actions: Vec<UiAction>,
    /// Set once the user has answered the closing question, so the close it
    /// sends is let through.
    exiting: bool,
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
    audio_devices: Vec<AudioDeviceTarget>,
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
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings_store = SettingsStore::for_current_user();
        let settings = settings_store.load().unwrap_or_else(|error| {
            eprintln!("could not load app settings: {error}");
            AppSettings::default()
        });
        // Before the first pass draws, so the window never appears in one
        // palette and switches to another.
        cc.egui_ctx.set_theme(settings.theme);
        install_locale_fonts(&cc.egui_ctx);
        let ui_state = UiState::default();
        if let Err(error) = settings_store.save(&settings) {
            eprintln!("could not save app settings: {error}");
        }
        let localization = LocalizationManager::new(settings.locale);
        let engine_repaint_ctx = cc.egui_ctx.clone();
        let resource_repaint_ctx = cc.egui_ctx.clone();
        let project_repaint_ctx = cc.egui_ctx.clone();
        #[cfg(target_os = "linux")]
        let picker_repaint_ctx = cc.egui_ctx.clone();

        // Built before the struct so the engine can be handed a dispatcher:
        // opening a capture Source can produce a fresher restore token, and
        // that belongs in the project rather than in this run's memory.
        let project_manager =
            ProjectManager::spawn(move || project_repaint_ctx.request_repaint()).ok();
        let project_dispatcher = project_manager.as_ref().map(ProjectManager::dispatcher);
        let recording_settings = settings.recording.clone();

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
            audio_devices: crate::capture::audio_devices(),
            exiting: false,
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
                    self.snapshots.sources = sources;
                    // Nothing to reconcile against yet: the mixer is what the
                    // project holds, and no pipeline is built from it.
                    self.snapshots.audio = audio;
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
        self.snapshots.status.target_fps = Some(engine.target_fps());
        // Read every pass rather than tracked here: the engine is what knows
        // whether a recording actually started, and the Controls dock reads
        // the same answer the status bar's clock does.
        self.snapshots.status.recording_elapsed = engine.recording();
        self.snapshots.status.recording_paused = engine.recording_paused();
        self.snapshots.status.recording_error = engine.recording_error();
        if self.snapshots.status.encoders.is_empty()
            && let Some(encoders) = engine.encoders()
        {
            self.snapshots.status.encoders = encoders.as_ref().clone();
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

    fn poll_resource_usage(&mut self) {
        let Some(manager) = &self.resources else {
            return;
        };
        if let Some(usage) = manager.latest() {
            self.snapshots.status.cpu_percent = usage.cpu_percent;
            self.snapshots.status.gpu = usage.gpu;
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
            UiAction::DragSceneItem(item_id, transform) => {
                if let Some(engine) = &self.engine {
                    engine.set_dragging_transform(item_id, transform);
                }
            }
            UiAction::SetFullscreen(fullscreen) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
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
        self.poll_project();
        self.poll_engine(ctx);
        self.poll_resource_usage();
        #[cfg(target_os = "linux")]
        self.poll_system_display_picker();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_actions.clear();
        // Held for the whole draw so the texture cannot be swapped out from
        // under the painter mid-frame.
        let composite_frame = self.engine.as_ref().and_then(EngineManager::frame);
        ui::show(
            ui,
            &mut self.ui_state,
            &self.snapshots,
            &self.audio_devices,
            &self.localization,
            composite_frame.as_deref(),
            &mut self.ui_actions,
        );

        let ctx = ui.ctx().clone();
        for index in 0..self.ui_actions.len() {
            let action = self.ui_actions[index].clone();
            self.handle_ui_action(&ctx, action);
        }
        self.ui_actions.clear();
    }
}
