use eframe::egui;

pub(in crate::ui) fn show(ui: &mut egui::Ui) {
    ui.centered_and_justified(|ui| {
        ui.weak("No sources in Scene 1");
    });
}
