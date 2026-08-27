use eframe::egui;

use crate::domain::SceneCanvas;
use crate::engine::EngineManager;
use crate::i18n::{LocalizationManager, install_locale_fonts};
use crate::project::{ProjectCommand, ProjectManager, ProjectUpdate};
use crate::resource_manager::ResourceManager;
use crate::settings::{AppSettings, SettingsStore};
use crate::snapshots::Snapshots;
use crate::ui::{self, UiAction, UiState};

#[cfg(target_os = "linux")]
use crate::capture::linux::{SystemDisplayPicker, SystemDisplayPickerUpdate};

pub struct ObsApp {
    ui_state: UiState,
    snapshots: Snapshots,
    project_manager: Option<ProjectManager>,
    resource_manager: Option<ResourceManager>,
    engine: Option<EngineManager>,
    #[cfg(target_os = "linux")]
    system_display_picker: Option<SystemDisplayPicker>,
    localization: LocalizationManager,
    settings: AppSettings,
    settings_store: SettingsStore,
    ui_actions: Vec<UiAction>,
}

impl ObsApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ui_state = UiState::default();
        cc.egui_ctx.set_theme(ui_state.theme());
        install_locale_fonts(&cc.egui_ctx);
        let settings_store = SettingsStore::for_current_user();
        let settings = settings_store.load().unwrap_or_else(|error| {
            eprintln!("could not load app settings: {error}");
            AppSettings::default()
        });
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

        Self {
            ui_state,
            snapshots: Snapshots::default(),
            project_manager,
            resource_manager: ResourceManager::spawn(move || {
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
                    move || engine_repaint_ctx.request_repaint(),
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
        }
    }

    fn poll_project(&mut self) {
        let Some(manager) = &self.project_manager else {
            return;
        };
        if let Some(update) = manager.latest() {
            match update {
                ProjectUpdate::Snapshot { scenes, sources } => {
                    self.snapshots.scenes = scenes;
                    self.snapshots.sources = sources;
                    if let Some(engine) = &self.engine {
                        engine.apply(&self.snapshots.sources);
                    }
                }
                ProjectUpdate::Error(error) => eprintln!("project database error: {error}"),
            }
        }
    }

    fn poll_engine(&mut self) {
        let Some(engine) = &self.engine else {
            return;
        };
        self.snapshots.status.active_fps = engine.active_fps();
        self.snapshots.status.target_fps = Some(engine.target_fps());
    }

    fn poll_resource_usage(&mut self) {
        let Some(manager) = &self.resource_manager else {
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
                    manager.dispatch(ProjectCommand::Source(
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
            UiAction::SetTheme(theme) => ctx.set_theme(theme),
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
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_project();
        self.poll_engine();
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
