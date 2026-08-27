use eframe::egui;

use super::state::{PreviewScaleMode, PreviewViewState};

pub(super) const TOOLBAR_HEIGHT: f32 = 26.0;
pub(super) const TOOLBAR_WIDTH: f32 = 210.0;
pub(super) const TOOLBAR_GAP: f32 = 6.0;

pub(super) fn show(ui: &mut egui::Ui, state: &mut PreviewViewState) {
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.horizontal_centered(|ui| {
        if ui
            .add_enabled(state.can_decrease(), egui::Button::new("−"))
            .on_hover_text("Decrease viewport scale")
            .clicked()
        {
            state.decrease();
        }

        let mut percentage = state.percentage();
        let response = ui.add(
            egui::DragValue::new(&mut percentage)
                .range(40.0..=100.0)
                .speed(1.0)
                .suffix("%")
                .max_decimals(0)
                .update_while_editing(false),
        );
        if response.changed() {
            state.set_percentage(percentage);
        }

        if ui
            .add_enabled(state.can_increase(), egui::Button::new("+"))
            .on_hover_text("Increase viewport scale")
            .clicked()
        {
            state.increase();
        }

        let mut fit_text = egui::RichText::new("Fit");
        if state.mode() == PreviewScaleMode::FitToWorkspace {
            fit_text = fit_text.strong();
        }
        ui.menu_button(fit_text, |ui| {
            if ui
                .selectable_label(
                    state.mode() == PreviewScaleMode::FitToWorkspace,
                    "Fit to Workspace",
                )
                .clicked()
            {
                state.fit_to_workspace();
                ui.close();
            }
            ui.separator();
            for percentage in [50.0, 75.0, 100.0] {
                if ui.button(format!("{percentage:.0}%")).clicked() {
                    state.set_percentage(percentage);
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Reset View").clicked() {
                state.reset();
                ui.close();
            }
        })
        .response
        .on_hover_text("Preview viewport scale options");
    });
}
