use crate::i18n::Locale;
use crate::{
    domain::{AudioSourceId, Crop, SceneId, SceneItemId, Transform},
    project::ProjectCommand,
};

#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    Exit,
    Project(ProjectCommand),
    OpenSystemDisplayPicker(SceneId),
    /// One item's Transform while the pointer is still down. Goes to the
    /// compositor and not to the project: a drag is one edit, recorded when it
    /// ends, but the picture has to follow the pointer meanwhile.
    DragSceneItem(SceneItemId, Transform, Crop),
    /// One filter's settings while its slider is still held, for the reason
    /// `DragSceneItem` exists: keying is tuned by watching the picture, and
    /// the project hears one edit when the pointer comes up rather than one
    /// per frame of the drag.
    DragFilterSettings(
        SceneItemId,
        crate::domain::FilterId,
        crate::domain::FilterSettings,
    ),
    /// A Drawing's strokes while the pointer is still down, for the same
    /// reason `DragSceneItem` exists: the mark has to be under the pointer,
    /// and the stroke is recorded once when the gesture ends.
    DrawStrokes(SceneItemId, Vec<crate::domain::Stroke>),
    /// A Color Source.s colour while the picker is still held, for the same
    /// reason `DragSceneItem` exists: the picture has to follow the pointer,
    /// and the repaint is recorded once when it is let go.
    DragSourceColour(SceneItemId, [u8; 4]),
    /// A Text Source as it is being edited, for the same reason
    /// `DragSourceColour` exists: a caption is written while watching it, so
    /// the picture follows every keystroke and the project hears one edit
    /// when the field is let go.
    ///
    /// The whole settings rather than the one field that changed, because
    /// drawing is done from all of them either way — the same reason
    /// `DrawStrokes` carries the whole list.
    DragSourceText(SceneItemId, crate::domain::TextSourceSettings),
    /// Ask for a font file for one Text Source. The dialog is the system's
    /// and outlives this pass, so it is run where the state to hold it is —
    /// see `ObsApp::open_font_picker`.
    PickTextFont(SceneItemId),
    /// One source's gain while the fader is still held. Goes to the audio
    /// graph and not to the project, for the same reason `DragSceneItem`
    /// does: what is heard has to follow the pointer, and the edit is
    /// recorded once when the gesture ends.
    DragAudioGain(AudioSourceId, f32),
    /// One mixer channel's filter while its slider is still held — to the
    /// audio graph, for the reason `DragAudioGain` goes there: a gate is set
    /// by listening to it.
    DragAudioFilterSettings(
        AudioSourceId,
        crate::domain::AudioFilterId,
        crate::domain::AudioFilterSettings,
    ),
    /// Open the Filters dock on one mixer channel's filters. A channel is in
    /// no Scene, so nothing the editor can select stands for it — the
    /// channel's own menu is the way in.
    ShowAudioFilters(AudioSourceId),
    /// One media file Source's gain while the fader is still held. Goes to
    /// the engine rather than the audio graph: a file's fader lives in its
    /// own pipeline, which the video engine owns.
    DragMediaGain(SceneItemId, f32),
    /// Move one media file Source to a position in its own file.
    ///
    /// Straight to the engine and not to the project: where a clip is playing
    /// from is not something to record, the way a Transform is. Sent when the
    /// scrub bar is let go rather than while it is dragged — a seek is a
    /// flush and a preroll, not something to do sixty times a second.
    SeekMediaFile(SceneItemId, std::time::Duration),
    SetFullscreen(bool),
    SetTheme(crate::settings::Theme),
    SetLocale(Locale),
    /// Begin recording the composited frames. Where the file goes is the
    /// engine's to decide, so this carries nothing.
    StartRecording,
    /// Stop or resume writing to the running recording, leaving its file
    /// open. The paused span does not end up in it.
    SetRecordingPaused(bool),
    /// Finish the running recording and close its file.
    StopRecording,
    /// Begin publishing to the configured server. Where it goes is the
    /// settings' to decide, so this carries nothing — as `StartRecording`
    /// does.
    StartStreaming,
    /// End the running broadcast.
    StopStreaming,
    /// End the recording properly and then quit — what the closing question
    /// asks about.
    StopRecordingAndExit,
    /// Open one Source again after it was disconnected — on Linux this puts
    /// the portal's window picker on screen, which is why nothing does it
    /// without being asked.
    ReopenSource(SceneItemId),
    /// Show the Settings dialog, seeded from what is currently set.
    OpenSettings,
    /// Shows the folder recordings are written to in the system's own file
    /// manager, creating it first if nothing has recorded yet.
    ShowRecordings,
    /// Commit the dialog's draft: persist it, and hand the engine the part it
    /// needs. Boxed because it is much the largest variant here and every
    /// other one would otherwise be padded to its size.
    ApplySettings(Box<crate::settings::AppSettings>),
}
