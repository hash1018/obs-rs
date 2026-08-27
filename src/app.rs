use eframe::egui;

use crate::resource_manager::ResourceManager;
use crate::snapshots::Snapshots;
use crate::ui::{self, UiAction, UiState};

pub struct ObsApp {
    ui_state: UiState,
    snapshots: Snapshots,
    resource_manager: Option<ResourceManager>,
    ui_actions: Vec<UiAction>,
}

impl ObsApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ui_state = UiState::default();
        cc.egui_ctx.set_theme(ui_state.theme());
        let repaint_ctx = cc.egui_ctx.clone();

        Self {
            ui_state,
            snapshots: Snapshots::default(),
            resource_manager: ResourceManager::spawn(move || repaint_ctx.request_repaint()).ok(),
            ui_actions: Vec::new(),
        }
    }

    fn poll_resource_usage(&mut self) {
        let Some(manager) = &self.resource_manager else {
            return;
        };
        if let Some(usage) = manager.latest() {
            self.snapshots.status.cpu_percent = usage.cpu_percent;
            self.snapshots.status.gpu_percent = usage.gpu_percent;
        }
    }

    fn handle_ui_action(&mut self, ctx: &egui::Context, action: UiAction) {
        match action {
            UiAction::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            UiAction::SetFullscreen(fullscreen) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
            }
            UiAction::SetTheme(theme) => ctx.set_theme(theme),
        }
    }
}

impl eframe::App for ObsApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_resource_usage();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_actions.clear();
        ui::show(
            ui,
            &mut self.ui_state,
            &self.snapshots,
            &mut self.ui_actions,
        );

        let ctx = ui.ctx().clone();
        for index in 0..self.ui_actions.len() {
            let action = self.ui_actions[index];
            self.handle_ui_action(&ctx, action);
        }
        self.ui_actions.clear();
    }
}
