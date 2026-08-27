use eframe::egui;

use crate::ui::editor::ResizeHandle;

const HANDLE_RADIUS: f32 = 4.0;
const HANDLE_HIT_RADIUS: f32 = 8.0;
const MIN_ITEM_SIZE: f32 = 16.0;

pub(super) fn paint(painter: &egui::Painter, selection: egui::Color32, rect: egui::Rect) {
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(2.0, selection),
        egui::StrokeKind::Outside,
    );
    for (_, center) in handle_centers(rect) {
        painter.circle_filled(center, HANDLE_RADIUS, egui::Color32::WHITE);
        painter.circle_stroke(center, HANDLE_RADIUS, egui::Stroke::new(1.0, selection));
    }
}

pub(super) fn hit_test(rect: egui::Rect, pointer: egui::Pos2) -> Option<ResizeHandle> {
    handle_centers(rect)
        .into_iter()
        .find_map(|(handle, center)| {
            (center.distance(pointer) <= HANDLE_HIT_RADIUS).then_some(handle)
        })
}

pub(super) fn cursor(handle: ResizeHandle) -> egui::CursorIcon {
    match handle {
        ResizeHandle::TopLeft | ResizeHandle::BottomRight => egui::CursorIcon::ResizeNwSe,
        ResizeHandle::TopRight | ResizeHandle::BottomLeft => egui::CursorIcon::ResizeNeSw,
        ResizeHandle::Top | ResizeHandle::Bottom => egui::CursorIcon::ResizeVertical,
        ResizeHandle::Left | ResizeHandle::Right => egui::CursorIcon::ResizeHorizontal,
    }
}

pub(super) fn resize_rect(
    original: egui::Rect,
    handle: ResizeHandle,
    delta: egui::Vec2,
) -> egui::Rect {
    match handle {
        ResizeHandle::Left => egui::Rect::from_min_max(
            egui::pos2(
                (original.left() + delta.x).min(original.right() - MIN_ITEM_SIZE),
                original.top(),
            ),
            original.max,
        ),
        ResizeHandle::Right => egui::Rect::from_min_max(
            original.min,
            egui::pos2(
                (original.right() + delta.x).max(original.left() + MIN_ITEM_SIZE),
                original.bottom(),
            ),
        ),
        ResizeHandle::Top => egui::Rect::from_min_max(
            egui::pos2(
                original.left(),
                (original.top() + delta.y).min(original.bottom() - MIN_ITEM_SIZE),
            ),
            original.max,
        ),
        ResizeHandle::Bottom => egui::Rect::from_min_max(
            original.min,
            egui::pos2(
                original.right(),
                (original.bottom() + delta.y).max(original.top() + MIN_ITEM_SIZE),
            ),
        ),
        ResizeHandle::TopLeft => resize_corner(original, handle, original.min + delta),
        ResizeHandle::TopRight => resize_corner(original, handle, original.right_top() + delta),
        ResizeHandle::BottomRight => resize_corner(original, handle, original.max + delta),
        ResizeHandle::BottomLeft => resize_corner(original, handle, original.left_bottom() + delta),
    }
}

fn resize_corner(original: egui::Rect, handle: ResizeHandle, dragged: egui::Pos2) -> egui::Rect {
    let fixed = match handle {
        ResizeHandle::TopLeft => original.max,
        ResizeHandle::TopRight => original.left_bottom(),
        ResizeHandle::BottomRight => original.min,
        ResizeHandle::BottomLeft => original.right_top(),
        _ => unreachable!("only corner handles use resize_corner"),
    };
    let raw = dragged - fixed;
    let x_sign = if raw.x < 0.0 { -1.0 } else { 1.0 };
    let y_sign = if raw.y < 0.0 { -1.0 } else { 1.0 };
    let aspect = original.width() / original.height();
    let width_change = (raw.x.abs() / original.width() - 1.0).abs();
    let height_change = (raw.y.abs() / original.height() - 1.0).abs();
    let (width, height) = if width_change >= height_change {
        let width = raw.x.abs().max(MIN_ITEM_SIZE);
        (width, (width / aspect).max(MIN_ITEM_SIZE))
    } else {
        let height = raw.y.abs().max(MIN_ITEM_SIZE);
        ((height * aspect).max(MIN_ITEM_SIZE), height)
    };
    let resized = fixed + egui::vec2(width * x_sign, height * y_sign);
    egui::Rect::from_two_pos(fixed, resized)
}

fn handle_centers(rect: egui::Rect) -> [(ResizeHandle, egui::Pos2); 8] {
    [
        (ResizeHandle::TopLeft, rect.min),
        (ResizeHandle::Top, rect.center_top()),
        (ResizeHandle::TopRight, rect.right_top()),
        (ResizeHandle::Right, rect.right_center()),
        (ResizeHandle::BottomRight, rect.max),
        (ResizeHandle::Bottom, rect.center_bottom()),
        (ResizeHandle::BottomLeft, rect.left_bottom()),
        (ResizeHandle::Left, rect.left_center()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_resize_keeps_aspect_ratio() {
        let original = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(160.0, 90.0));
        let resized = resize_rect(original, ResizeHandle::BottomRight, egui::vec2(160.0, 20.0));
        assert!((resized.width() / resized.height() - 16.0 / 9.0).abs() < 0.001);
    }
}
