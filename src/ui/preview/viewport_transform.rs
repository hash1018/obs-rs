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

    /// The inverse of [`ViewportTransform::canvas_to_screen`], for a gesture
    /// that names a place rather than a movement — drawing, where every point
    /// is its own position, unlike a drag where only the delta matters.
    pub fn screen_to_canvas(self, point: egui::Pos2) -> egui::Pos2 {
        ((point - self.viewport.min) / self.scale).to_pos2()
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
    /// A point converted out and back is the point it started as, which is
    /// what drawing needs: a stroke is a list of positions, and one that
    /// drifted by a scale factor would land somewhere other than the pointer.
    #[test]
    fn a_canvas_point_survives_the_round_trip_to_the_screen() {
        let viewport = ViewportTransform::new(
            egui::Rect::from_min_size(egui::pos2(40.0, 25.0), egui::vec2(960.0, 540.0)),
            SceneCanvas::DEFAULT,
        );
        for point in [
            egui::pos2(0.0, 0.0),
            egui::pos2(1920.0, 1080.0),
            egui::pos2(733.5, 219.25),
        ] {
            let round_trip = viewport.screen_to_canvas(viewport.canvas_to_screen(point));
            assert!(
                (round_trip - point).length() < 0.01,
                "{point:?} came back as {round_trip:?}"
            );
        }
    }

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
