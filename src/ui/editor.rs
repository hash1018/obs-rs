use crate::domain::{Crop, SceneId, SceneItemId, Transform};
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
    /// The same handles with Alt held: what moves is where the Source's own
    /// picture is cut rather than how large it is drawn. See
    /// `preview::crop_drag`.
    Crop(ResizeHandle),
}

#[derive(Clone, Copy)]
pub(super) struct TransformDrag {
    pub item_id: SceneItemId,
    pub original: Transform,
    /// What the item was cropped to when the gesture began. Held for the
    /// same reason `original` is: every frame's crop is computed from the
    /// whole gesture's delta rather than accumulated from the last one.
    pub crop: Crop,
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
    /// A pen you can read through: the same stroke at an alpha that leaves
    /// whatever it was drawn over legible. Which is why it is a tool rather
    /// than a colour in the palette — its width, its colour, and the fact
    /// that it does not cover are one choice, not three.
    Highlighter,
    Eraser,
}

impl Tool {
    /// How opaque this tool's strokes are.
    ///
    /// A third of the way is enough to read a colour off and little enough to
    /// read through — a highlighter that hides what it marks has marked
    /// nothing.
    fn alpha(self) -> u8 {
        match self {
            Self::Highlighter => 90,
            _ => u8::MAX,
        }
    }
}

/// One tool's colour and width, remembered while another is in use.
///
/// Shared settings would mean picking up the highlighter and finding it thin
/// and red, then picking the pen back up and finding it thick and yellow.
/// They are different implements and each keeps what it was set to.
pub(super) struct Nib {
    /// Without alpha: how much shows through is [`Tool::alpha`]'s to say, and
    /// a palette entry that carried its own would let the two disagree.
    pub rgb: [u8; 3],
    pub width: f32,
}

/// The pen's own settings, and the stroke it is part-way through.
pub(super) struct PenState {
    pub tool: Tool,
    pub pen: Nib,
    pub highlighter: Nib,
    /// The stroke being drawn, in the Drawing's own coordinates. `None`
    /// between gestures.
    pub stroke: Option<Vec<[f32; 2]>>,
}

impl PenState {
    /// The nib in use, which the toolbar edits and a stroke is made with.
    ///
    /// Select and Eraser have no implement of their own and answer with the
    /// pen's — for the eraser that is also how far it reaches, so it sweeps
    /// as wide as the line it is there to take away.
    pub fn nib(&self) -> &Nib {
        match self.tool {
            Tool::Highlighter => &self.highlighter,
            _ => &self.pen,
        }
    }

    pub fn nib_mut(&mut self) -> &mut Nib {
        match self.tool {
            Tool::Highlighter => &mut self.highlighter,
            _ => &mut self.pen,
        }
    }

    /// What a stroke made now is coloured: the nib's, at the tool's own
    /// opacity.
    pub fn rgba(&self) -> [u8; 4] {
        let [red, green, blue] = self.nib().rgb;
        [red, green, blue, self.tool.alpha()]
    }

    pub fn width(&self) -> f32 {
        self.nib().width
    }
}

impl Default for PenState {
    fn default() -> Self {
        Self {
            tool: Tool::default(),
            pen: Nib {
                // Red, because annotation is nearly always over someone else's
                // picture and this is the one colour that is rarely in it.
                rgb: [220, 40, 40],
                width: 6.0,
            },
            highlighter: Nib {
                // Yellow and broad, which is what the implement it is named
                // for looks like — and a highlighter as narrow as a pen would
                // only be a fainter pen.
                rgb: [250, 220, 60],
                width: 24.0,
            },
            stroke: None,
        }
    }
}

#[derive(Default)]
pub(super) struct SceneEditorState {
    scene_id: Option<SceneId>,
    selected_item_id: Option<SceneItemId>,
    pub transform_override: Option<(SceneItemId, Transform)>,
    /// What a crop drag has reached, until the project is told. Separate
    /// from `transform_override` because a crop drag moves both and either
    /// can settle first.
    pub crop_override: Option<(SceneItemId, Crop)>,
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

        if let Some((item_id, crop)) = self.crop_override
            && snapshot
                .items
                .iter()
                .find(|item| item.id == item_id)
                .is_some_and(|item| item.crop == crop)
        {
            self.crop_override = None;
        }
    }

    pub fn selected_item_id(&self) -> Option<SceneItemId> {
        self.selected_item_id
    }

    pub fn select(&mut self, item_id: SceneItemId) {
        if self.selected_item_id != Some(item_id) {
            self.transform_override = None;
            self.crop_override = None;
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

    /// The crop being dragged, or the one the project holds.
    pub fn effective_crop(&self, item_id: SceneItemId, stored: Crop) -> Crop {
        self.crop_override
            .filter(|(overridden_id, _)| *overridden_id == item_id)
            .map_or(stored, |(_, crop)| crop)
    }
}
