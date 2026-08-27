use eframe::egui;

const PREVIEW_ASPECT_RATIO: f32 = 16.0 / 9.0;

pub fn show(ui: &mut egui::Ui) {
    egui::CentralPanel::default()
        .frame(
            egui::Frame::central_panel(ui.style())
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(egui::Margin::same(18)),
        )
        .show(ui, |ui| {
            let available = ui.available_size();
            let preview_size = fit_aspect_ratio(available, PREVIEW_ASPECT_RATIO);

            ui.vertical_centered(|ui| {
                ui.add_space(((available.y - preview_size.y) * 0.5).max(0.0));
                let (rect, _) = ui.allocate_exact_size(preview_size, egui::Sense::hover());
                let painter = ui.painter();

                painter.rect_filled(rect, 0, egui::Color32::BLACK);
                painter.rect_stroke(
                    rect,
                    0,
                    egui::Stroke::new(1.0, egui::Color32::from_gray(64)),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "No capture source",
                    egui::FontId::proportional(14.0),
                    egui::Color32::from_gray(132),
                );
            });
        });
}

fn fit_aspect_ratio(available: egui::Vec2, aspect_ratio: f32) -> egui::Vec2 {
    let width_from_height = available.y * aspect_ratio;
    if width_from_height <= available.x {
        egui::vec2(width_from_height, available.y)
    } else {
        egui::vec2(available.x, available.x / aspect_ratio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_fits_wide_and_tall_areas() {
        let wide = fit_aspect_ratio(egui::vec2(1600.0, 600.0), PREVIEW_ASPECT_RATIO);
        assert!((wide.x - 1066.6667).abs() < 0.001);
        assert_eq!(wide.y, 600.0);

        assert_eq!(
            fit_aspect_ratio(egui::vec2(800.0, 900.0), PREVIEW_ASPECT_RATIO),
            egui::vec2(800.0, 450.0)
        );
    }
}
