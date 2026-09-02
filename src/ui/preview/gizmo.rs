use eframe::egui;

use crate::ui::editor::ResizeHandle;

/// Enough to tie the white dot to the outline's colour without becoming a
/// ring in its own right.
const HANDLE_RING_WIDTH: f32 = 1.5;
const HANDLE_RADIUS: f32 = 4.0;
const HANDLE_HIT_RADIUS: f32 = 8.0;
const MIN_ITEM_SIZE: f32 = 16.0;

/// The grab points, without the outline between them.
///
/// The outline is drawn by the caller, because whether an edge is the
/// picture's own or one a crop cut is not something this module knows — see
/// `preview::paint_outline`.
pub(super) fn paint_handles(painter: &egui::Painter, selection: egui::Color32, rect: egui::Rect) {
    for (_, center) in handle_centers(rect) {
        painter.circle_filled(center, HANDLE_RADIUS, egui::Color32::WHITE);
        painter.circle_stroke(
            center,
            HANDLE_RADIUS,
            egui::Stroke::new(HANDLE_RING_WIDTH, selection),
        );
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
    let original_corner = match handle {
        ResizeHandle::TopLeft => original.min,
        ResizeHandle::TopRight => original.right_top(),
        ResizeHandle::BottomRight => original.max,
        ResizeHandle::BottomLeft => original.left_bottom(),
        _ => unreachable!("only corner handles use resize_corner"),
    };
    let diagonal = original_corner - fixed;
    let dragged_from_fixed = dragged - fixed;
    let diagonal_length_squared = diagonal.length_sq();
    let projected_scale = dragged_from_fixed.dot(diagonal) / diagonal_length_squared;
    let minimum_scale = (MIN_ITEM_SIZE / original.width())
        .max(MIN_ITEM_SIZE / original.height())
        .min(1.0);
    // Project onto the original diagonal instead of switching between the X
    // and Y axes every frame. This keeps the opposite corner fixed, preserves
    // aspect ratio, and prevents the rectangle from flipping across its anchor.
    let resized = fixed + diagonal * projected_scale.max(minimum_scale);
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

    #[test]
    fn every_corner_resize_keeps_its_opposite_corner_fixed() {
        let original = egui::Rect::from_min_size(egui::pos2(20.0, 30.0), egui::vec2(160.0, 90.0));
        let cases = [
            (ResizeHandle::TopLeft, original.max),
            (ResizeHandle::TopRight, original.left_bottom()),
            (ResizeHandle::BottomRight, original.min),
            (ResizeHandle::BottomLeft, original.right_top()),
        ];
        for (handle, fixed) in cases {
            let resized = resize_rect(original, handle, egui::vec2(45.0, -20.0));
            assert!(
                [
                    resized.min,
                    resized.right_top(),
                    resized.max,
                    resized.left_bottom()
                ]
                .into_iter()
                .any(|corner| corner.distance(fixed) < 0.001),
                "{handle:?}: {resized:?}"
            );
        }
    }

    #[test]
    fn corner_cannot_flip_past_the_opposite_corner() {
        let original = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(160.0, 90.0));
        let resized = resize_rect(
            original,
            ResizeHandle::BottomRight,
            egui::vec2(-320.0, -180.0),
        );
        assert_eq!(resized.min, original.min);
        assert!(resized.width() >= MIN_ITEM_SIZE);
        assert!(resized.height() >= MIN_ITEM_SIZE);
        assert!((resized.width() / resized.height() - 16.0 / 9.0).abs() < 0.001);
    }
}
