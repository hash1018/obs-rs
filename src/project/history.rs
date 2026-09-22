//! Edit → Undo and Redo.
//!
//! # Built on what SQLite already records
//!
//! Every edit runs as one transaction, and SQLite's session extension can
//! hand back exactly the rows a transaction changed — inserted, updated,
//! deleted, and those a foreign key's cascade took along — as a changeset
//! that can be inverted. So a step here is that changeset and nothing more:
//! undo applies its inverse, redo applies it again. No command needs an
//! inverse written by hand, which is what would otherwise make eighty of
//! them eighty chances to get one wrong, and deleting a Source comes back
//! with its filters, strokes and placements because the rows do.
//!
//! # What is a step, and what is not
//!
//! An edit is a step. Three kinds of change are not:
//!
//! - **What the engine writes.** A portal hands back a fresher token and a
//!   capture reports the size it opened at; those arrive through the engine's
//!   own dispatcher and are never recorded — an undo taking one back would
//!   be undoing something nobody did.
//! - **What is operated rather than edited.** A fader, a mute button, a
//!   monitor switch, a clip's play and pause, a timer's start and stop. These
//!   are worked during a show; an undo meant for a moved Source that also
//!   unmuted a microphone would be an accident on air.
//! - **Which Scene is selected.** That is what is being broadcast. An undo
//!   never changes it, not even to show where the step it took back was —
//!   the status bar says that instead (see [`HistoryMove`]).

use rusqlite::Connection;
use rusqlite::session::Changeset;

use crate::domain::{AudioFilterId, AudioSourceId, FilterId, SceneId, SceneItemId};
use crate::persistence::{PersistenceResult, ProjectDatabase, SceneStore};
use crate::snapshots::{EditLabel, EditVerb, HistoryMove, HistorySnapshot};

use super::{AudioCommand, ProjectCommand, SceneCommand, SourceCommand};

/// How many steps Undo can reach back.
///
/// Each is only the rows it changed, so a hundred is small; what bounds it is
/// that a history nobody could scroll back through is not one anybody uses.
const MAX_STEPS: usize = 100;

/// What a step changed, before it is put into words.
enum Target {
    Item(SceneItemId),
    Scene(SceneId),
    Channel(AudioSourceId),
    Filter(FilterId),
    AudioFilter(AudioFilterId),
    /// The one setting that is about the project rather than one thing in
    /// it: the transition.
    Project,
}

/// Whether `command` is an edit, and if it is, how to describe it.
///
/// Every command answers, with no wildcard: a command added later has to be
/// decided on here rather than being recorded, or left out, by default.
fn classify(command: &ProjectCommand) -> Option<(EditVerb, Target)> {
    use EditVerb as V;
    Some(match command {
        ProjectCommand::Scene(command) => match command {
            SceneCommand::Add => (V::AddScene, Target::Project),
            SceneCommand::Delete(scene) => (V::DeleteScene, Target::Scene(*scene)),
            SceneCommand::Duplicate(scene) => (V::DuplicateScene, Target::Scene(*scene)),
            SceneCommand::MoveUp(scene) | SceneCommand::MoveDown(scene) => {
                (V::Reorder, Target::Scene(*scene))
            }
            SceneCommand::Rename(scene, _) => (V::Rename, Target::Scene(*scene)),
            // What is on air — see this module's docs.
            SceneCommand::Select(_) => return None,
            SceneCommand::SetTransition(_) => (V::Transition, Target::Project),
        },
        ProjectCommand::Source(command) => match command {
            SourceCommand::AddColor(scene)
            | SourceCommand::AddDrawing(scene)
            | SourceCommand::AddText(scene)
            | SourceCommand::AddBrowser(scene)
            | SourceCommand::AddDisplayCapture {
                scene_id: scene, ..
            }
            | SourceCommand::AddWindowCapture {
                scene_id: scene, ..
            }
            | SourceCommand::AddMediaFile {
                scene_id: scene, ..
            }
            | SourceCommand::AddImage {
                scene_id: scene, ..
            }
            | SourceCommand::AddRtsp {
                scene_id: scene, ..
            }
            | SourceCommand::AddVideoCapture {
                scene_id: scene, ..
            }
            | SourceCommand::AddScene(scene, _) => (V::AddSource, Target::Scene(*scene)),
            SourceCommand::Delete(item) => (V::Delete, Target::Item(*item)),
            SourceCommand::MoveUp(item) | SourceCommand::MoveDown(item) => {
                (V::Reorder, Target::Item(*item))
            }
            SourceCommand::Rename(item, _) => (V::Rename, Target::Item(*item)),
            SourceCommand::SetLocked(item, _) => (V::Lock, Target::Item(*item)),
            SourceCommand::SetVisible(item, _) | SourceCommand::ToggleVisible(item) => {
                (V::Visibility, Target::Item(*item))
            }
            SourceCommand::SetTransform(item, _) => (V::Transform, Target::Item(*item)),
            SourceCommand::SetCrop(item, _) => (V::Crop, Target::Item(*item)),
            SourceCommand::SetOpacity(item, _) => (V::Opacity, Target::Item(*item)),
            SourceCommand::AddFilter { scene_item_id, .. }
            | SourceCommand::AddAudioFilter { scene_item_id, .. } => {
                (V::AddFilter, Target::Item(*scene_item_id))
            }
            SourceCommand::RemoveFilter(filter) => (V::RemoveFilter, Target::Filter(*filter)),
            SourceCommand::MoveFilterEarlier(filter) | SourceCommand::MoveFilterLater(filter) => {
                (V::ReorderFilter, Target::Filter(*filter))
            }
            SourceCommand::SetFilterEnabled(filter, _)
            | SourceCommand::SetFilterSettings(filter, _) => {
                (V::FilterSettings, Target::Filter(*filter))
            }
            SourceCommand::AddStroke(item, _) => (V::Draw, Target::Item(*item)),
            SourceCommand::RemoveStrokes(item, _) | SourceCommand::ClearStrokes(item) => {
                (V::Erase, Target::Item(*item))
            }
            SourceCommand::SetColor(item, _)
            | SourceCommand::SetSourceSize(item, ..)
            | SourceCommand::SetMediaLooping(item, _)
            | SourceCommand::SetRtspTransport(item, _)
            | SourceCommand::SetRtspReconnect(item, _)
            | SourceCommand::SetVideoCaptureMode(item, _)
            | SourceCommand::SetText(item, _)
            | SourceCommand::SetBrowserUrl(item, _)
            | SourceCommand::SetBrowserSize(item, _)
            | SourceCommand::SetBrowserFps(item, _)
            | SourceCommand::SetBrowserShutDownWhenHidden(item, _)
            | SourceCommand::SetBrowserRefreshWhenShown(item, _)
            | SourceCommand::SetSceneSource(item, _)
            | SourceCommand::SetTextFont(item, _)
            | SourceCommand::SetTextFontSize(item, _)
            | SourceCommand::SetTextColour(item, _)
            | SourceCommand::SetTextAlignment(item, _)
            | SourceCommand::SetTextMode(item, _)
            | SourceCommand::SetClockFormat(item, _)
            | SourceCommand::SetTimerFormat(item, _)
            | SourceCommand::SetTextSize(item, _) => (V::Properties, Target::Item(*item)),
            // Operated, not edited, and the engine's own — see this module's
            // docs. The token is only ever the engine's, but it is decided
            // here too so that no path could make it a step.
            SourceCommand::SetMediaGain(..)
            | SourceCommand::SetMediaMuted(..)
            | SourceCommand::SetMediaMonitored(..)
            | SourceCommand::SetMediaPaused(..)
            | SourceCommand::StartTextTimer(_)
            | SourceCommand::StopTextTimer(_)
            | SourceCommand::ResetTextTimer(_)
            | SourceCommand::SetRestoreToken(..) => return None,
        },
        ProjectCommand::Audio(command) => match command {
            AudioCommand::AddApplication => (V::AddChannel, Target::Project),
            AudioCommand::Remove(channel) => (V::RemoveChannel, Target::Channel(*channel)),
            AudioCommand::SetDevice(channel, _) => (V::ChannelDevice, Target::Channel(*channel)),
            AudioCommand::AddFilter {
                audio_source_id, ..
            } => (V::AddFilter, Target::Channel(*audio_source_id)),
            AudioCommand::RemoveFilter(filter) => (V::RemoveFilter, Target::AudioFilter(*filter)),
            AudioCommand::MoveFilterEarlier(filter) | AudioCommand::MoveFilterLater(filter) => {
                (V::ReorderFilter, Target::AudioFilter(*filter))
            }
            AudioCommand::SetFilterEnabled(filter, _)
            | AudioCommand::SetFilterSettings(filter, _) => {
                (V::FilterSettings, Target::AudioFilter(*filter))
            }
            // Operated — see this module's docs.
            AudioCommand::SetGainDb(..)
            | AudioCommand::SetMuted(..)
            | AudioCommand::SetMonitored(..) => return None,
        },
        // Not edits either: they are how the history is walked.
        ProjectCommand::Undo | ProjectCommand::Redo => return None,
    })
}

/// Puts a target into the words the rest of the interface uses for it.
///
/// Asked *before* the step runs, so a deleted thing is still there to be
/// named. What cannot be found is named by nothing rather than failing the
/// edit: the label is for a menu, and the edit is what matters.
fn describe(connection: &Connection, target: &Target) -> String {
    let item = |id: i64| {
        connection
            .query_row(
                "SELECT scenes.name, sources.name
                 FROM scene_items
                 JOIN scenes ON scenes.id = scene_items.scene_id
                 JOIN sources ON sources.id = scene_items.source_id
                 WHERE scene_items.id = ?1",
                [id],
                |row| {
                    Ok(format!(
                        "{} › {}",
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?
                    ))
                },
            )
            .unwrap_or_default()
    };
    let one = |sql: &str, id: i64| {
        connection
            .query_row(sql, [id], |row| row.get::<_, String>(0))
            .unwrap_or_default()
    };
    match target {
        Target::Item(item_id) => item(item_id.0),
        Target::Scene(scene) => one("SELECT name FROM scenes WHERE id = ?1", scene.0),
        Target::Channel(channel) => one("SELECT name FROM audio_sources WHERE id = ?1", channel.0),
        // A filter is the Source's, not one placement's, so it is named by
        // the Source alone.
        Target::Filter(filter) => one(
            "SELECT sources.name FROM source_filters
             JOIN sources ON sources.id = source_filters.source_id
             WHERE source_filters.id = ?1",
            filter.0,
        ),
        // A mixer channel's, or a media Source's own sound.
        Target::AudioFilter(filter) => one(
            "SELECT COALESCE(audio_sources.name, sources.name)
             FROM audio_source_filters
             LEFT JOIN audio_sources ON audio_sources.id = audio_source_filters.audio_source_id
             LEFT JOIN sources ON sources.id = audio_source_filters.source_id
             WHERE audio_source_filters.id = ?1",
            filter.0,
        ),
        Target::Project => String::new(),
    }
}

/// One step: the rows an edit changed, and what to call it.
struct Step {
    label: EditLabel,
    changes: Changeset,
}

/// The steps that can be undone, and those that were and can be redone.
///
/// Lives on the project thread beside the database it is a history of, for
/// the length of a run: a history kept across restarts would be a history of
/// a project somebody may since have edited by hand.
#[derive(Default)]
pub(super) struct History {
    /// Oldest first.
    done: Vec<Step>,
    /// The most recently undone last, so redo takes from the end.
    undone: Vec<Step>,
    moved: Option<HistoryMove>,
    serial: u64,
}

impl History {
    /// Runs one command, recording it as a step where it is an edit.
    ///
    /// `apply` is the command itself, run inside whichever transaction this
    /// chooses. A new step drops everything that had been undone — the usual
    /// rule, and the only one that keeps redo meaning "put back what I took
    /// back" rather than "apply an edit to a project that has moved on".
    pub(super) fn run(
        &mut self,
        database: &mut ProjectDatabase,
        command: ProjectCommand,
        apply: impl FnOnce(&rusqlite::Transaction<'_>, ProjectCommand) -> PersistenceResult<()>,
    ) -> PersistenceResult<()> {
        let Some((verb, target)) = classify(&command) else {
            return database.transaction(|transaction| apply(transaction, command));
        };
        let label = EditLabel {
            verb,
            target: describe(database.connection(), &target),
        };
        let ((), changes) =
            database.recorded_transaction(|transaction| apply(transaction, command))?;
        // An edit that changed nothing — moving the top Source up — is not a
        // step anyone would want to undo, and not one to cost them what they
        // could redo.
        if let Some(changes) = changes {
            self.undone.clear();
            self.done.push(Step { label, changes });
            if self.done.len() > MAX_STEPS {
                self.done.remove(0);
            }
            self.moved = None;
        }
        Ok(())
    }

    /// Takes the last step back. Nothing to undo is not an error.
    pub(super) fn undo(&mut self, database: &mut ProjectDatabase) -> PersistenceResult<()> {
        let Some(step) = self.done.pop() else {
            return Ok(());
        };
        match step.changes.invert() {
            Ok(inverse) => {
                if let Err(error) = apply_keeping_selection(database, &inverse) {
                    // Left where it was, so a failed undo can be tried again
                    // rather than quietly losing the step.
                    self.done.push(step);
                    return Err(error);
                }
            }
            Err(error) => {
                self.done.push(step);
                return Err(error.into());
            }
        }
        self.moved(true, &step.label);
        self.undone.push(step);
        Ok(())
    }

    /// Puts the last step undone back.
    pub(super) fn redo(&mut self, database: &mut ProjectDatabase) -> PersistenceResult<()> {
        let Some(step) = self.undone.pop() else {
            return Ok(());
        };
        if let Err(error) = apply_keeping_selection(database, &step.changes) {
            self.undone.push(step);
            return Err(error);
        }
        self.moved(false, &step.label);
        self.done.push(step);
        Ok(())
    }

    pub(super) fn snapshot(&self) -> HistorySnapshot {
        HistorySnapshot {
            undo: self.done.last().map(|step| step.label.clone()),
            redo: self.undone.last().map(|step| step.label.clone()),
            moved: self.moved.clone(),
        }
    }

    fn moved(&mut self, undone: bool, label: &EditLabel) {
        self.serial += 1;
        self.moved = Some(HistoryMove {
            undone,
            label: label.clone(),
            serial: self.serial,
        });
    }
}

/// Applies a step, and then puts the selected Scene back where it was.
///
/// A step can carry a change of selection with it — adding a Scene selects
/// the new one, and deleting the selected one moves the selection on — and
/// taking such a step back or putting it back would change what is on air.
/// So whatever was selected stays selected, wherever it still exists. Where
/// the step took that very Scene away, the selection the step itself holds
/// is the only one there is.
fn apply_keeping_selection(
    database: &mut ProjectDatabase,
    changes: &Changeset,
) -> PersistenceResult<()> {
    let selected = SceneStore::selected_scene_id(database.connection())?;
    database.apply_recorded(changes, |transaction| {
        let Some(selected) = selected else {
            return Ok(());
        };
        let exists: bool = transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM scenes WHERE id = ?1)",
            [selected.0],
            |row| row.get(0),
        )?;
        if exists && SceneStore::selected_scene_id(transaction)? != Some(selected) {
            SceneStore::select(transaction, selected)?;
        }
        Ok(())
    })
}
