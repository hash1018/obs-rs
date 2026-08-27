use eframe::egui;

use crate::domain::SceneCanvas;

#[derive(Clone, Copy)]
/// Maps persistent SceneCanvas coordinates to the temporary on-screen PreviewViewport.
pub(super) struct ViewportTransform {
    viewport: egui::Rect,
    scale: f32,
}

impl ViewportTransform {
    pub fn new(viewport: egui::Rect, canvas: SceneCanvas) -> Self {
        Self {
            viewport,
            scale: viewport.width() / canvas.width,
        }
    }

    pub fn canvas_to_screen(self, point: egui::Pos2) -> egui::Pos2 {
        self.viewport.min + point.to_vec2() * self.scale
    }

    pub fn canvas_rect_to_screen(self, rect: egui::Rect) -> egui::Rect {
        egui::Rect::from_min_max(
            self.canvas_to_screen(rect.min),
            self.canvas_to_screen(rect.max),
        )
    }

    pub fn screen_delta_to_canvas(self, delta: egui::Vec2) -> egui::Vec2 {
        delta / self.scale
    }

    pub fn viewport(self) -> egui::Rect {
        self.viewport
    }
}

pub(super) fn fit_aspect_ratio(available: egui::Vec2, aspect_ratio: f32) -> egui::Vec2 {
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
    fn screen_delta_is_converted_to_canvas_coordinates() {
        let viewport = ViewportTransform::new(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 540.0)),
            SceneCanvas::DEFAULT,
        );
        assert_eq!(
            viewport.screen_delta_to_canvas(egui::vec2(100.0, 50.0)),
            egui::vec2(200.0, 100.0)
        );
    }
}
