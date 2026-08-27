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

#[derive(Default)]
pub(super) struct SceneEditorState {
    scene_id: Option<SceneId>,
    selected_item_id: Option<SceneItemId>,
    pub transform_override: Option<(SceneItemId, Transform)>,
    pub drag: Option<TransformDrag>,
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
        }
        self.selected_item_id = Some(item_id);
    }

    pub fn clear_selection(&mut self) {
        self.selected_item_id = None;
        self.transform_override = None;
        self.drag = None;
    }

    pub fn effective_transform(&self, item_id: SceneItemId, stored: Transform) -> Transform {
        self.transform_override
            .filter(|(overridden_id, _)| *overridden_id == item_id)
            .map_or(stored, |(_, transform)| transform)
    }
}
