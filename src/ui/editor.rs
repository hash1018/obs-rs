use crate::domain::{SceneId, SceneItemId, Transform};
use crate::snapshots::SourcesSnapshot;

#[derive(Debug, Clone, Copy)]
pub(super) enum ResizeHandle {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

#[derive(Clone, Copy)]
pub(super) enum TransformDragMode {
    Move,
    Resize(ResizeHandle),
}

#[derive(Clone, Copy)]
pub(super) struct TransformDrag {
    pub item_id: SceneItemId,
    pub original: Transform,
    pub mode: TransformDragMode,
}

/// What the pointer does inside the Preview.
///
/// Selecting and drawing want the same gesture, so one of them has to be
/// chosen. The tool is only offered while a Drawing is selected — that is
/// what the Preview's toolbar shows — and it goes back to [`Tool::Select`]
/// whenever the selection moves elsewhere, so the pointer is never quietly
/// left in a mode for an item that is no longer there.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tool {
    /// Pick items up, move them, resize them — what the Preview has always
    /// done, and what a Drawing is left in when it is first selected so that
    /// a stray click cannot draw a dot.
    #[default]
    Select,
    Pen,
    Eraser,
}

/// The pen's own settings, and the stroke it is part-way through.
pub(super) struct PenState {
    pub tool: Tool,
    pub rgba: [u8; 4],
    pub width: f32,
    /// The stroke being drawn, in the Drawing's own coordinates. `None`
    /// between gestures.
    pub stroke: Option<Vec<[f32; 2]>>,
}

impl Default for PenState {
    fn default() -> Self {
        Self {
            tool: Tool::default(),
            // Red, because annotation is nearly always over someone else's
            // picture and this is the one colour that is rarely in it.
            rgba: [220, 40, 40, 255],
            width: 6.0,
            stroke: None,
        }
    }
}

#[derive(Default)]
pub(super) struct SceneEditorState {
    scene_id: Option<SceneId>,
    selected_item_id: Option<SceneItemId>,
    pub transform_override: Option<(SceneItemId, Transform)>,
    pub drag: Option<TransformDrag>,
    pub pen: PenState,
}

impl SceneEditorState {
    pub fn sync(&mut self, snapshot: &SourcesSnapshot) {
        if self.scene_id != snapshot.scene_id {
            self.scene_id = snapshot.scene_id;
            self.clear_selection();
            return;
        }

        if self
            .selected_item_id
            .is_some_and(|selected| !snapshot.items.iter().any(|item| item.id == selected))
        {
            self.clear_selection();
        }

        if let Some((item_id, transform)) = self.transform_override
            && snapshot
                .items
                .iter()
                .find(|item| item.id == item_id)
                .is_some_and(|item| item.transform == transform)
        {
            self.transform_override = None;
        }
    }

    pub fn selected_item_id(&self) -> Option<SceneItemId> {
        self.selected_item_id
    }

    pub fn select(&mut self, item_id: SceneItemId) {
        if self.selected_item_id != Some(item_id) {
            self.transform_override = None;
            self.drag = None;
            // The tool belongs to the item it was chosen for. Carrying a pen
            // across to whatever is selected next would draw on something the
            // user was only pointing at.
            self.pen.tool = Tool::Select;
            self.pen.stroke = None;
        }
        self.selected_item_id = Some(item_id);
    }

    pub fn clear_selection(&mut self) {
        self.selected_item_id = None;
        self.transform_override = None;
        self.drag = None;
        self.pen.tool = Tool::Select;
        self.pen.stroke = None;
    }

    pub fn effective_transform(&self, item_id: SceneItemId, stored: Transform) -> Transform {
        self.transform_override
            .filter(|(overridden_id, _)| *overridden_id == item_id)
            .map_or(stored, |(_, transform)| transform)
    }
}
