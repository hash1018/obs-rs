use std::collections::HashSet;

use crate::domain::{
    Crop, SceneCanvas, SceneId, SceneItemId, SourceKind, SourceSettings, Transform,
};

#[derive(Clone)]
pub struct SourcesSnapshot {
    pub canvas: SceneCanvas,
    pub scene_id: Option<SceneId>,
    pub scene_name: Option<String>,
    /// Front-most item first, matching the order shown in the Sources dock.
    pub items: Vec<SceneItemSnapshot>,
    /// Every SceneItem in the project, not only the selected Scene's.
    ///
    /// The engine keeps a Source open when its item merely leaves the visible
    /// Scene, so returning to that Scene costs nothing; it has to close one
    /// whose item is gone for good. The selected Scene alone cannot tell those
    /// two apart.
    pub live_items: HashSet<SceneItemId>,
    /// Every Source's name in the project, not only the selected Scene's.
    ///
    /// A name is unique across the project, so renaming one to a name already
    /// taken is a refused write rather than an edit. The Sources dock has to
    /// be able to say that before sending the rename, and the items above are
    /// only some of the names there are.
    pub names: HashSet<String>,
}

impl Default for SourcesSnapshot {
    fn default() -> Self {
        Self {
            canvas: SceneCanvas::DEFAULT,
            scene_id: None,
            scene_name: None,
            items: Vec::new(),
            live_items: HashSet::new(),
            names: HashSet::new(),
        }
    }
}

impl SceneItemSnapshot {
    /// The item's rectangle in Canvas coordinates, as `[x, y, width, height]`.
    ///
    /// The Preview draws it and the compositor places a layer at it, so it has
    /// to be one calculation rather than two that agree by inspection.
    ///
    /// `transform` is a parameter instead of `self.transform` because the
    /// editor asks for the value it is part-way through dragging, which is not
    /// what the project database holds yet.
    pub fn canvas_rect(&self, transform: Transform) -> [f32; 4] {
        self.canvas_rect_cropped(transform, self.crop)
    }

    /// The same, for a crop this item does not have yet.
    ///
    /// What a crop drag needs: the rectangle follows the edge being dragged
    /// while the pointer is still down, and the crop it is following has not
    /// been recorded — the same split every other gesture here makes.
    pub fn canvas_rect_cropped(&self, transform: Transform, crop: Crop) -> [f32; 4] {
        let width =
            (self.source_size[0] - crop.left - crop.right).max(1.0) * transform.scale[0].max(0.001);
        let height =
            (self.source_size[1] - crop.top - crop.bottom).max(1.0) * transform.scale[1].max(0.001);
        [
            transform.position[0] - width * transform.anchor[0],
            transform.position[1] - height * transform.anchor[1],
            width,
            height,
        ]
    }

    /// Where a Canvas point falls inside the Source's own picture, in that
    /// picture's own coordinates.
    ///
    /// The inverse of [`SceneItemSnapshot::canvas_rect`], which is what
    /// drawing needs: the pointer names a place on the Canvas, and a stroke
    /// has to be recorded where it lands on the Drawing so that moving or
    /// resizing the item afterwards carries its marks along.
    ///
    /// Crop is undone as well as scale, so a point lands in the picture
    /// rather than in the visible part of it. A zero-sized rectangle — which
    /// `canvas_rect` cannot produce, since both extents are clamped — would
    /// have no inverse, so this answers `None` rather than dividing by it.
    pub fn canvas_point_to_source(
        &self,
        transform: Transform,
        point: [f32; 2],
    ) -> Option<[f32; 2]> {
        let [x, y, width, height] = self.canvas_rect(transform);
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let visible_width = (self.source_size[0] - self.crop.left - self.crop.right).max(1.0);
        let visible_height = (self.source_size[1] - self.crop.top - self.crop.bottom).max(1.0);
        Some([
            self.crop.left + (point[0] - x) / width * visible_width,
            self.crop.top + (point[1] - y) / height * visible_height,
        ])
    }
}

#[derive(Clone)]
pub struct SceneItemSnapshot {
    pub id: SceneItemId,
    pub name: String,
    pub kind: SourceKind,
    pub settings: SourceSettings,
    /// The Source's own size in Canvas units, before `transform` scales it.
    pub source_size: [f32; 2],
    pub visible: bool,
    pub locked: bool,
    pub transform: Transform,
    pub crop: Crop,
    /// The loudest sample this Source's own audio has reached since the last
    /// update, in decibels relative to full scale.
    ///
    /// `None` for everything but a media file that is playing one: no other
    /// kind here has sound of its own. Filled from the engine rather than
    /// from the project — it is a measurement, not something stored — which
    /// is why it is on the snapshot rather than in `settings`.
    pub peak_db: Option<f32>,
    /// Where a media file Source has reached in its own file.
    ///
    /// `None` for every other kind, and for one that has not produced a frame
    /// yet. Filled from the engine like `peak_db` and for the same reason: it
    /// is a measurement, not something the project stores.
    pub position: Option<std::time::Duration>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Crop, SourceKind, SourceSettings};

    fn item(source_size: [f32; 2], crop: Crop) -> SceneItemSnapshot {
        SceneItemSnapshot {
            id: SceneItemId(1),
            name: "Drawing".into(),
            kind: SourceKind::Drawing,
            settings: SourceSettings::Drawing(crate::domain::DrawingSourceSettings {
                size: source_size,
                strokes: Vec::new(),
            }),
            source_size,
            transform: Transform::default(),
            crop,

            peak_db: None,

            position: None,
            visible: true,
            locked: false,
        }
    }

    /// Drawing needs the exact inverse of the placement, not something close
    /// to it: a stroke recorded through a mismatched mapping lands somewhere
    /// other than the pointer, and the further from the middle the worse.
    #[test]
    fn a_canvas_point_maps_back_to_where_the_source_put_it() {
        let item = item([1920.0, 1080.0], Crop::default());
        for transform in [
            Transform::default(),
            Transform {
                position: [300.0, 200.0],
                scale: [0.5, 0.25],
                ..Transform::default()
            },
            Transform {
                position: [50.0, 900.0],
                scale: [2.0, 1.5],
                anchor: [0.0, 1.0],
                ..Transform::default()
            },
        ] {
            let [x, y, width, height] = item.canvas_rect(transform);
            // The picture's own corners and middle, put on the Canvas by the
            // placement and asked for back.
            for (source, canvas) in [
                ([0.0, 0.0], [x, y]),
                ([1920.0, 1080.0], [x + width, y + height]),
                ([960.0, 540.0], [x + width / 2.0, y + height / 2.0]),
            ] {
                let back = item
                    .canvas_point_to_source(transform, canvas)
                    .expect("a placed item has an inverse");
                assert!(
                    (back[0] - source[0]).abs() < 0.01 && (back[1] - source[1]).abs() < 0.01,
                    "{canvas:?} should be {source:?} in the source, got {back:?}"
                );
            }
        }
    }

    /// Crop shifts the picture under the rectangle, so undoing the placement
    /// has to undo it too — otherwise drawing on a cropped source lands by
    /// however much was cropped away.
    #[test]
    fn cropping_moves_where_a_canvas_point_lands() {
        let crop = Crop {
            left: 100.0,
            top: 50.0,
            right: 0.0,
            bottom: 0.0,
        };
        let item = item([1920.0, 1080.0], crop);
        let [x, y, ..] = item.canvas_rect(Transform::default());
        let back = item
            .canvas_point_to_source(Transform::default(), [x, y])
            .expect("a placed item has an inverse");
        assert!(
            (back[0] - 100.0).abs() < 0.01 && (back[1] - 50.0).abs() < 0.01,
            "the rectangle's corner is the first pixel left after cropping, got {back:?}"
        );
    }
}
