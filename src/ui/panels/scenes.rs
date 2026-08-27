use eframe::egui;

pub(in crate::ui) fn show(ui: &mut egui::Ui) {
    let _ = ui.selectable_label(true, "Scene 1");

    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        ui.separator();
        ui.horizontal(|ui| {
            let _ = ui.button("+");
            ui.add_enabled(false, egui::Button::new("−"));
        });
    });
}
