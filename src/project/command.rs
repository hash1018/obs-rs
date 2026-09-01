use crate::domain::{
    AudioSourceId, DisplayCaptureSettings, ImageSourceSettings, MediaFileSettings, SceneId,
    SceneItemId, Stroke, Transform, WindowCaptureSettings,
};

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectCommand {
    Scene(SceneCommand),
    Source(SourceCommand),
    Audio(AudioCommand),
}

/// A change to one of the mixer's audio sources.
///
/// Carries no Scene, because audio is not in one — see
/// [`crate::domain::AudioSourceId`].
// Every variant is a `Set` because every one of them is: the mixer has no
// add or remove, only the three values a channel holds. Dropping the verb
// would leave `AudioCommand::Device(..)` reading as a noun where the rest of
// this file reads as instructions.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq)]
pub enum AudioCommand {
    /// Gain in decibels. Clamped where it is stored rather than here, so
    /// every way in lands on the same range.
    SetGainDb(AudioSourceId, f32),
    SetMuted(AudioSourceId, bool),
    /// Which endpoint to listen to, or `None` to follow the system default.
    SetDevice(AudioSourceId, Option<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceCommand {
    AddColor(SceneId),
    AddDrawing(SceneId),
    AddDisplayCapture {
        scene_id: SceneId,
        settings: DisplayCaptureSettings,
    },
    AddWindowCapture {
        scene_id: SceneId,
        settings: WindowCaptureSettings,
    },
    AddMediaFile {
        scene_id: SceneId,
        settings: MediaFileSettings,
    },
    AddImage {
        scene_id: SceneId,
        settings: ImageSourceSettings,
    },
    Delete(SceneItemId),
    MoveUp(SceneItemId),
    MoveDown(SceneItemId),
    /// Renames the Source this item stands for.
    ///
    /// The Source, not the item: a name belongs to what is being captured,
    /// and an item is one placement of it. Renaming through an item is what
    /// every other command here does, and it means the new name shows in
    /// every Scene the Source appears in — which is what sharing one is for.
    Rename(SceneItemId, String),
    SetLocked(SceneItemId, bool),
    /// Replaces the portal token a Display Capture reopens with.
    SetRestoreToken(SceneItemId, Option<String>),
    SetTransform(SceneItemId, Transform),
    SetVisible(SceneItemId, bool),
    /// Puts one finished stroke on a Drawing. Sent when the pointer comes up,
    /// not while it is down — see `UiAction::DragStroke`.
    AddStroke(SceneItemId, Stroke),
    /// Takes strokes off a Drawing by their position in it, which is both
    /// what the eraser does and what undo does.
    RemoveStrokes(SceneItemId, Vec<usize>),
    ClearStrokes(SceneItemId),
    /// Repaints a Color Source. Sent when the picker is let go, not while it
    /// is being dragged — see `UiAction::DragSourceColour`.
    SetColor(SceneItemId, [u8; 4]),
    /// A media file Source's own fader, recorded when the gesture ends — the
    /// same two-part split a Color's picker makes, with
    /// `UiAction::DragMediaGain` carrying the live value.
    SetMediaGain(SceneItemId, f32),
    /// Whether a media file Source's sound is muted.
    SetMediaMuted(SceneItemId, bool),
    /// Whether a media file Source is stopped where it is.
    ///
    /// Stored rather than kept in the engine because it must survive the
    /// Scene changing and the application being restarted.
    SetMediaPaused(SceneItemId, bool),
    /// Whether a media file Source starts again when it reaches its end.
    ///
    /// Takes effect where it is: the running Source is told through its own
    /// handle rather than reopened, so switching this does not restart what
    /// is playing.
    SetMediaLooping(SceneItemId, bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneCommand {
    Add,
    Delete(SceneId),
    Duplicate(SceneId),
    MoveUp(SceneId),
    MoveDown(SceneId),
    Rename(SceneId, String),
    Select(SceneId),
}
