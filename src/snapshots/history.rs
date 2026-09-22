//! What Edit → Undo and Redo would do, as the menu names them.

/// The two menu items, and what the last one pressed did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HistorySnapshot {
    /// What Undo would take back, or `None` where there is nothing to.
    pub undo: Option<EditLabel>,
    /// What Redo would put back again.
    pub redo: Option<EditLabel>,
    /// The step Undo or Redo just moved, for the status bar to say.
    ///
    /// An undo that changes something in a Scene nobody is looking at would
    /// otherwise change nothing anyone can see — and it does not switch to
    /// that Scene, because selecting a Scene is what is on air. So this is
    /// how anyone finds out what was undone, and where.
    pub moved: Option<HistoryMove>,
}

/// One step Undo or Redo moved.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryMove {
    /// Whether it was taken back, rather than put back.
    pub undone: bool,
    pub label: EditLabel,
    /// Counts every move, so the status bar can tell a new one from the one
    /// it is already showing — two undos of steps with the same label are
    /// two moves.
    pub serial: u64,
}

/// One step, as a person would describe it: what was done, and to what.
///
/// The words are the interface's, in whichever language it is set to; the
/// names are the project's own and go through untranslated.
#[derive(Clone, Debug, PartialEq)]
pub struct EditLabel {
    pub verb: EditVerb,
    /// `Scene 1 › Webcam`, `Scene 2`, `Microphone` — whatever the step
    /// changed, named the way the rest of the interface names it. Empty for
    /// the one step that is about no one thing, the transition.
    pub target: String,
}

/// What kind of change a step was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditVerb {
    AddScene,
    DeleteScene,
    DuplicateScene,
    AddSource,
    Delete,
    Rename,
    Reorder,
    Transform,
    Crop,
    Opacity,
    Visibility,
    /// How long showing and hiding take — see
    /// [`crate::domain::VisibilityFades`].
    Fade,
    Lock,
    Properties,
    AddFilter,
    RemoveFilter,
    ReorderFilter,
    FilterSettings,
    Draw,
    Erase,
    Transition,
    AddChannel,
    RemoveChannel,
    ChannelDevice,
}
