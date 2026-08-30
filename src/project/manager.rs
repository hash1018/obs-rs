use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use crate::domain::{Scene, SceneItem, Source};
use crate::persistence::{AudioStore, PersistenceResult, ProjectDatabase, SceneStore, SourceStore};
use crate::snapshots::{
    AudioSnapshot, AudioSourceSnapshot, SceneItemSnapshot, SceneSnapshot, ScenesSnapshot,
    SourcesSnapshot,
};

use super::{AudioCommand, ProjectCommand, SceneCommand, SourceCommand};

enum ManagerMessage {
    Execute(ProjectCommand),
    Shutdown,
}

pub enum ProjectUpdate {
    Snapshot {
        scenes: ScenesSnapshot,
        sources: SourcesSnapshot,
        audio: AudioSnapshot,
    },
    Error(String),
}

/// Lets something other than the UI change the project.
///
/// The engine needs one: opening a capture Source can hand back a fresher
/// restore token than the one it was given, and that belongs in the database
/// rather than in the memory of the run that received it.
#[derive(Clone)]
pub struct ProjectDispatcher {
    command_tx: Sender<ManagerMessage>,
}

impl ProjectDispatcher {
    pub fn dispatch(&self, command: ProjectCommand) {
        let _ = self.command_tx.send(ManagerMessage::Execute(command));
    }
}

pub struct ProjectManager {
    command_tx: Sender<ManagerMessage>,
    update_rx: Receiver<ProjectUpdate>,
    worker: Option<JoinHandle<()>>,
}

impl ProjectManager {
    pub fn spawn(wake_ui: impl Fn() + Send + 'static) -> PersistenceResult<Self> {
        let database = ProjectDatabase::open_default()?;
        let (command_tx, command_rx) = mpsc::channel();
        let (update_tx, update_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("project-manager".into())
            .spawn(move || run(database, command_rx, &update_tx, &wake_ui))?;

        Ok(Self {
            command_tx,
            update_rx,
            worker: Some(worker),
        })
    }

    pub fn dispatch(&self, command: ProjectCommand) {
        self.dispatcher().dispatch(command);
    }

    pub fn dispatcher(&self) -> ProjectDispatcher {
        ProjectDispatcher {
            command_tx: self.command_tx.clone(),
        }
    }

    pub fn latest(&self) -> Option<ProjectUpdate> {
        let mut latest = None;
        loop {
            match self.update_rx.try_recv() {
                Ok(update) => latest = Some(update),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return latest,
            }
        }
    }
}

impl Drop for ProjectManager {
    fn drop(&mut self) {
        let _ = self.command_tx.send(ManagerMessage::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run(
    mut database: ProjectDatabase,
    command_rx: Receiver<ManagerMessage>,
    update_tx: &Sender<ProjectUpdate>,
    wake_ui: &impl Fn(),
) {
    publish_snapshot(&database, update_tx, wake_ui);

    while let Ok(message) = command_rx.recv() {
        match message {
            ManagerMessage::Execute(command) => {
                let result = handle_project_command(&mut database, command);
                match result {
                    Ok(()) => publish_snapshot(&database, update_tx, wake_ui),
                    Err(error) => {
                        let _ = update_tx.send(ProjectUpdate::Error(error.to_string()));
                        wake_ui();
                    }
                }
            }
            ManagerMessage::Shutdown => break,
        }
    }
}

fn handle_project_command(
    database: &mut ProjectDatabase,
    command: ProjectCommand,
) -> PersistenceResult<()> {
    match command {
        ProjectCommand::Scene(command) => handle_scene_command(database, command),
        ProjectCommand::Source(command) => handle_source_command(database, command),
        ProjectCommand::Audio(command) => handle_audio_command(database, command),
    }
}

fn handle_audio_command(
    database: &mut ProjectDatabase,
    command: AudioCommand,
) -> PersistenceResult<()> {
    database.transaction(|transaction| match command {
        AudioCommand::SetGainDb(id, gain_db) => AudioStore::set_gain_db(transaction, id, gain_db),
        AudioCommand::SetMuted(id, muted) => AudioStore::set_muted(transaction, id, muted),
        AudioCommand::SetDevice(id, device) => {
            AudioStore::set_device(transaction, id, device.as_deref())
        }
    })
}

fn handle_source_command(
    database: &mut ProjectDatabase,
    command: SourceCommand,
) -> PersistenceResult<()> {
    database.transaction(|transaction| match command {
        SourceCommand::AddColor(scene_id) => {
            SourceStore::add_color(transaction, scene_id)?;
            Ok(())
        }
        SourceCommand::AddDrawing(scene_id) => {
            SourceStore::add_drawing(transaction, scene_id)?;
            Ok(())
        }
        SourceCommand::AddStroke(item_id, stroke) => {
            SourceStore::add_stroke(transaction, item_id, &stroke)
        }
        SourceCommand::RemoveStrokes(item_id, ordinals) => {
            SourceStore::remove_strokes(transaction, item_id, &ordinals)
        }
        SourceCommand::ClearStrokes(item_id) => SourceStore::clear_strokes(transaction, item_id),
        SourceCommand::AddDisplayCapture { scene_id, settings } => {
            SourceStore::add_display_capture(transaction, scene_id, &settings)?;
            Ok(())
        }
        SourceCommand::Delete(scene_item_id) => {
            SourceStore::delete_scene_item(transaction, scene_item_id)
        }
        SourceCommand::MoveUp(scene_item_id) => SourceStore::move_up(transaction, scene_item_id),
        SourceCommand::MoveDown(scene_item_id) => {
            SourceStore::move_down(transaction, scene_item_id)
        }
        SourceCommand::SetRestoreToken(scene_item_id, token) => {
            SourceStore::set_restore_token(transaction, scene_item_id, token.as_deref())
        }
        SourceCommand::SetLocked(scene_item_id, locked) => {
            SourceStore::set_locked(transaction, scene_item_id, locked)
        }
        SourceCommand::SetVisible(scene_item_id, visible) => {
            SourceStore::set_visible(transaction, scene_item_id, visible)
        }
        SourceCommand::SetTransform(scene_item_id, transform) => {
            SourceStore::set_transform(transaction, scene_item_id, transform)
        }
        SourceCommand::SetColor(scene_item_id, rgba) => {
            SourceStore::set_color(transaction, scene_item_id, rgba)
        }
    })
}

fn handle_scene_command(
    database: &mut ProjectDatabase,
    command: SceneCommand,
) -> PersistenceResult<()> {
    database.transaction(|transaction| match command {
        SceneCommand::Add => {
            SceneStore::add(transaction)?;
            Ok(())
        }
        SceneCommand::Delete(scene_id) => SceneStore::delete(transaction, scene_id),
        SceneCommand::Duplicate(scene_id) => SceneStore::duplicate(transaction, scene_id),
        SceneCommand::MoveUp(scene_id) => SceneStore::move_up(transaction, scene_id),
        SceneCommand::MoveDown(scene_id) => SceneStore::move_down(transaction, scene_id),
        SceneCommand::Rename(scene_id, name) => SceneStore::rename(transaction, scene_id, &name),
        SceneCommand::Select(scene_id) => SceneStore::select(transaction, scene_id),
    })
}

fn scene_snapshot(database: &ProjectDatabase) -> PersistenceResult<ScenesSnapshot> {
    let items = SceneStore::list(database.connection())?
        .into_iter()
        .map(|Scene { id, name, .. }| SceneSnapshot { id, name })
        .collect();
    let selected_scene_id = SceneStore::selected_scene_id(database.connection())?;
    Ok(ScenesSnapshot {
        items,
        selected_scene_id,
    })
}

fn sources_snapshot(
    database: &ProjectDatabase,
    scenes: &ScenesSnapshot,
) -> PersistenceResult<SourcesSnapshot> {
    let live_items = SourceStore::live_item_ids(database.connection())?;
    let Some(scene_id) = scenes.selected_scene_id else {
        return Ok(SourcesSnapshot {
            live_items,
            ..SourcesSnapshot::default()
        });
    };
    let canvas = crate::domain::SceneCanvas::DEFAULT;
    let scene_name = scenes
        .items
        .iter()
        .find(|scene| scene.id == scene_id)
        .map(|scene| scene.name.clone());
    let items = SourceStore::list_for_scene(database.connection(), scene_id)?
        .into_iter()
        .map(|(item, source)| {
            debug_assert_eq!(item.scene_id, scene_id);
            debug_assert_eq!(item.source_id, source.id);
            let SceneItem {
                id,
                visible,
                locked,
                transform,
                crop,
                z_index,
                ..
            } = item;
            let Source {
                id: _,
                name,
                kind,
                settings,
            } = source;
            debug_assert!(z_index >= 0);
            SceneItemSnapshot {
                id,
                name,
                kind,
                source_size: settings.source_size(canvas),
                settings,
                visible,
                locked,
                transform,
                crop,
            }
        })
        .collect();

    Ok(SourcesSnapshot {
        canvas,
        scene_id: Some(scene_id),
        scene_name,
        items,
        live_items,
    })
}

fn project_snapshot(
    database: &ProjectDatabase,
) -> PersistenceResult<(ScenesSnapshot, SourcesSnapshot, AudioSnapshot)> {
    let scenes = scene_snapshot(database)?;
    let sources = sources_snapshot(database, &scenes)?;
    let audio = audio_snapshot(database)?;
    Ok((scenes, sources, audio))
}

/// Read whole rather than diffed against the last one: there are a handful of
/// audio sources, and a fader moving is not a reason to work out which.
///
/// `peak_db` is `None` for every one of them — the meter is drawn from
/// whatever measures the audio, and nothing does yet.
fn audio_snapshot(database: &ProjectDatabase) -> PersistenceResult<AudioSnapshot> {
    let items = AudioStore::list(database.connection())?
        .into_iter()
        .map(|source| AudioSourceSnapshot {
            id: source.id,
            name: source.name,
            kind: source.kind,
            device: source.device,
            gain_db: source.gain_db,
            muted: source.muted,
            peak_db: None,
            running: true,
        })
        .collect();
    Ok(AudioSnapshot { items })
}

fn publish_snapshot(
    database: &ProjectDatabase,
    update_tx: &Sender<ProjectUpdate>,
    wake_ui: &impl Fn(),
) {
    let update = match project_snapshot(database) {
        Ok((scenes, sources, audio)) => ProjectUpdate::Snapshot {
            scenes,
            sources,
            audio,
        },
        Err(error) => ProjectUpdate::Error(error.to_string()),
    };
    let _ = update_tx.send(update);
    wake_ui();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{SourceSettings, Stroke};

    #[test]
    fn scene_commands_are_persisted_and_ordered() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();

        handle_scene_command(&mut database, SceneCommand::Add).unwrap();
        let snapshot = scene_snapshot(&database).unwrap();
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.items[1].name, "Scene 2");

        let second = snapshot.items[1].id;
        handle_scene_command(&mut database, SceneCommand::MoveUp(second)).unwrap();
        let snapshot = scene_snapshot(&database).unwrap();
        assert_eq!(snapshot.items[0].id, second);

        handle_scene_command(&mut database, SceneCommand::Duplicate(second)).unwrap();
        let snapshot = scene_snapshot(&database).unwrap();
        assert_eq!(snapshot.items.len(), 3);

        handle_scene_command(
            &mut database,
            SceneCommand::Rename(second, "Gameplay".into()),
        )
        .unwrap();
        let snapshot = scene_snapshot(&database).unwrap();
        assert_eq!(snapshot.items[0].name, "Gameplay");

        let conflicting = snapshot
            .items
            .iter()
            .find(|scene| scene.id != second)
            .unwrap()
            .id;
        assert!(
            handle_scene_command(
                &mut database,
                SceneCommand::Rename(conflicting, "Gameplay".into()),
            )
            .is_err()
        );

        handle_scene_command(&mut database, SceneCommand::Delete(second)).unwrap();
        let snapshot = scene_snapshot(&database).unwrap();
        assert_eq!(snapshot.items.len(), 2);
    }

    #[test]
    fn sources_snapshot_follows_the_selected_scene() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let first_scene = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        database
            .transaction(|transaction| {
                SourceStore::add_display_capture(
                    transaction,
                    first_scene,
                    &crate::domain::DisplayCaptureSettings {
                        target: crate::domain::DisplayCaptureTarget::MonitorName(
                            r"\\.\DISPLAY1".into(),
                        ),
                        size_hint: None,
                    },
                )?;
                Ok(())
            })
            .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(sources.scene_id, Some(first_scene));
        assert_eq!(sources.items.len(), 1);
        assert_eq!(sources.items[0].name, "Display Capture");

        handle_scene_command(&mut database, SceneCommand::Add).unwrap();
        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert!(sources.items.is_empty());
        assert_eq!(sources.scene_name.as_deref(), Some("Scene 2"));
    }

    /// A Drawing's marks survive a round trip through the database, and the
    /// two ways of taking one off — the eraser and undo — both work by
    /// position rather than by identity.
    #[test]
    fn a_drawings_strokes_survive_the_database_and_come_back_in_order() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        handle_source_command(&mut database, SourceCommand::AddDrawing(scene_id)).unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        let item_id = sources.items[0].id;
        assert_eq!(sources.items[0].name, "Drawing");
        assert!(
            matches!(&sources.items[0].settings, SourceSettings::Drawing(drawing) if drawing.strokes.is_empty()),
            "a new Drawing has nothing on it"
        );

        for (index, rgba) in [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]]
            .into_iter()
            .enumerate()
        {
            handle_source_command(
                &mut database,
                SourceCommand::AddStroke(
                    item_id,
                    Stroke {
                        points: vec![[index as f32, 1.5], [index as f32 + 10.0, 2.5]],
                        rgba,
                        width: 4.0,
                    },
                ),
            )
            .unwrap();
        }

        let strokes = |database: &ProjectDatabase| {
            let (_, sources, _) = project_snapshot(database).unwrap();
            match &sources.items[0].settings {
                SourceSettings::Drawing(drawing) => drawing.strokes.clone(),
                other => panic!("expected a Drawing, got {other:?}"),
            }
        };

        let drawn = strokes(&database);
        assert_eq!(drawn.len(), 3, "every stroke came back");
        assert_eq!(
            drawn[1].points,
            vec![[1.0, 1.5], [11.0, 2.5]],
            "points survive the round trip exactly, in the order drawn"
        );
        assert_eq!(drawn[2].rgba, [0, 0, 255, 255]);
        assert_eq!(drawn[0].width, 4.0);

        // The eraser takes the middle one; the two either side keep their
        // order and their colours.
        handle_source_command(
            &mut database,
            SourceCommand::RemoveStrokes(item_id, vec![1]),
        )
        .unwrap();
        let left = strokes(&database);
        assert_eq!(left.len(), 2);
        assert_eq!(
            [left[0].rgba, left[1].rgba],
            [[255, 0, 0, 255], [0, 0, 255, 255]],
            "removing by position takes the one meant, not the one after it"
        );

        handle_source_command(&mut database, SourceCommand::ClearStrokes(item_id)).unwrap();
        assert!(strokes(&database).is_empty(), "clearing leaves nothing");
    }

    #[test]
    fn color_sources_are_named_and_transformed_persistently() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();

        handle_source_command(&mut database, SourceCommand::AddColor(scene_id)).unwrap();
        handle_source_command(&mut database, SourceCommand::AddColor(scene_id)).unwrap();
        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(sources.items.len(), 2);
        assert_eq!(sources.items[0].name, "Color Source 2");
        assert_eq!(sources.items[1].name, "Color Source");
        assert_eq!(sources.items[0].transform.position, [960.0, 540.0]);
        assert!(matches!(
            sources.items[0].settings,
            crate::domain::SourceSettings::Color(_)
        ));

        let item_id = sources.items[0].id;
        let transform = crate::domain::Transform {
            position: [480.0, 270.0],
            scale: [0.5, 0.5],
            ..crate::domain::Transform::default()
        };
        handle_source_command(
            &mut database,
            SourceCommand::SetTransform(item_id, transform),
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(sources.items[0].transform, transform);
    }

    #[test]
    fn display_capture_source_persists_its_monitor() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();

        handle_source_command(
            &mut database,
            SourceCommand::AddDisplayCapture {
                scene_id,
                settings: crate::domain::DisplayCaptureSettings {
                    target: crate::domain::DisplayCaptureTarget::MonitorName(
                        r"\\.\DISPLAY2".into(),
                    ),
                    size_hint: Some([3440, 1440]),
                },
            },
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(sources.items.len(), 1);
        assert_eq!(sources.items[0].name, "Display Capture");
        // The picker reported an ultrawide, so the item starts at that shape
        // rather than being squared off to the 16:9 Canvas.
        assert_eq!(sources.items[0].source_size, [3440.0, 1440.0]);
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::DisplayCapture(settings)
                if settings.target
                    == crate::domain::DisplayCaptureTarget::MonitorName(r"\\.\DISPLAY2".into())
                    && settings.size_hint == Some([3440, 1440])
        ));
    }

    #[test]
    fn portal_display_capture_persists_its_restore_token() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();

        handle_source_command(
            &mut database,
            SourceCommand::AddDisplayCapture {
                scene_id,
                settings: crate::domain::DisplayCaptureSettings {
                    target: crate::domain::DisplayCaptureTarget::Portal {
                        restore_token: Some("token-1".into()),
                    },
                    size_hint: None,
                },
            },
        )
        .unwrap();
        // A compositor that declines to persist the selection still produces a
        // usable source; it just has to prompt again next time.
        handle_source_command(
            &mut database,
            SourceCommand::AddDisplayCapture {
                scene_id,
                settings: crate::domain::DisplayCaptureSettings {
                    target: crate::domain::DisplayCaptureTarget::Portal {
                        restore_token: None,
                    },
                    size_hint: None,
                },
            },
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        let targets: Vec<_> = sources
            .items
            .iter()
            .map(|item| match &item.settings {
                crate::domain::SourceSettings::DisplayCapture(settings) => settings.target.clone(),
                other => panic!("expected a display capture, got {other:?}"),
            })
            .collect();
        assert_eq!(
            targets,
            vec![
                crate::domain::DisplayCaptureTarget::Portal {
                    restore_token: None
                },
                crate::domain::DisplayCaptureTarget::Portal {
                    restore_token: Some("token-1".into())
                },
            ]
        );
    }

    #[test]
    fn source_visibility_lock_and_compositing_order_are_persisted() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        for _ in 0..3 {
            handle_source_command(&mut database, SourceCommand::AddColor(scene_id)).unwrap();
        }

        let names = |database: &ProjectDatabase| {
            project_snapshot(database)
                .unwrap()
                .1
                .items
                .into_iter()
                .map(|item| item.name)
                .collect::<Vec<_>>()
        };
        // The dock lists front-most first, which is the newest item.
        assert_eq!(
            names(&database),
            ["Color Source 3", "Color Source 2", "Color Source"]
        );

        let (_, sources, _) = project_snapshot(&database).unwrap();
        let back = sources.items[2].id;
        handle_source_command(&mut database, SourceCommand::MoveUp(back)).unwrap();
        assert_eq!(
            names(&database),
            ["Color Source 3", "Color Source", "Color Source 2"]
        );

        handle_source_command(&mut database, SourceCommand::MoveDown(back)).unwrap();
        assert_eq!(
            names(&database),
            ["Color Source 3", "Color Source 2", "Color Source"]
        );

        // Moving past the ends is a no-op rather than an error, so a toolbar
        // that lets it through cannot corrupt the order.
        let front = sources.items[0].id;
        handle_source_command(&mut database, SourceCommand::MoveUp(front)).unwrap();
        handle_source_command(&mut database, SourceCommand::MoveDown(back)).unwrap();
        assert_eq!(
            names(&database),
            ["Color Source 3", "Color Source 2", "Color Source"]
        );

        handle_source_command(&mut database, SourceCommand::SetVisible(front, false)).unwrap();
        handle_source_command(&mut database, SourceCommand::SetLocked(front, true)).unwrap();
        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert!(!sources.items[0].visible);
        assert!(sources.items[0].locked);
    }

    #[test]
    fn reordering_still_works_across_a_gap_left_by_a_deletion() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        for _ in 0..3 {
            handle_source_command(&mut database, SourceCommand::AddColor(scene_id)).unwrap();
        }

        // Removing the middle item leaves its z_index unused, so the two that
        // remain are no longer adjacent numbers. Looking a neighbour up by
        // `z_index + 1` would find nothing and silently do nothing.
        let (_, sources, _) = project_snapshot(&database).unwrap();
        handle_source_command(&mut database, SourceCommand::Delete(sources.items[1].id)).unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(sources.items.len(), 2);
        handle_source_command(&mut database, SourceCommand::MoveUp(sources.items[1].id)).unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(
            sources
                .items
                .into_iter()
                .map(|item| item.name)
                .collect::<Vec<_>>(),
            ["Color Source", "Color Source 3"]
        );
    }

    #[test]
    fn a_refreshed_portal_token_replaces_the_stored_one() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        handle_source_command(
            &mut database,
            SourceCommand::AddDisplayCapture {
                scene_id,
                settings: crate::domain::DisplayCaptureSettings {
                    target: crate::domain::DisplayCaptureTarget::Portal {
                        restore_token: Some("first".into()),
                    },
                    size_hint: None,
                },
            },
        )
        .unwrap();
        // A monitor target has no token and the schema forbids giving it one,
        // so the update has to leave it alone rather than fail the whole
        // transaction that carried it.
        handle_source_command(
            &mut database,
            SourceCommand::AddDisplayCapture {
                scene_id,
                settings: crate::domain::DisplayCaptureSettings {
                    target: crate::domain::DisplayCaptureTarget::MonitorName("DP-1".into()),
                    size_hint: None,
                },
            },
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        let monitor = sources.items[0].id;
        let portal = sources.items[1].id;

        handle_source_command(
            &mut database,
            SourceCommand::SetRestoreToken(portal, Some("second".into())),
        )
        .unwrap();
        handle_source_command(
            &mut database,
            SourceCommand::SetRestoreToken(monitor, Some("nonsense".into())),
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(
            sources.items[1].settings,
            crate::domain::SourceSettings::DisplayCapture(crate::domain::DisplayCaptureSettings {
                target: crate::domain::DisplayCaptureTarget::Portal {
                    restore_token: Some("second".into())
                },
                size_hint: None,
            })
        );
        assert_eq!(
            sources.items[0].settings,
            crate::domain::SourceSettings::DisplayCapture(crate::domain::DisplayCaptureSettings {
                target: crate::domain::DisplayCaptureTarget::MonitorName("DP-1".into()),
                size_hint: None,
            })
        );
    }

    #[test]
    fn the_snapshot_names_every_item_not_just_the_shown_scene() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let first_scene = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        handle_source_command(&mut database, SourceCommand::AddColor(first_scene)).unwrap();
        let hidden = project_snapshot(&database).unwrap().1.items[0].id;

        handle_scene_command(&mut database, SceneCommand::Add).unwrap();
        let second_scene = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        handle_source_command(&mut database, SourceCommand::AddColor(second_scene)).unwrap();

        // The engine keeps a Source open while its item is merely out of view
        // and closes one that is gone, so the snapshot has to distinguish
        // "not in this Scene" from "not in the project".
        let (_, sources, _) = project_snapshot(&database).unwrap();
        let shown = sources.items[0].id;
        assert!(!sources.items.iter().any(|item| item.id == hidden));
        assert!(sources.live_items.contains(&hidden));
        assert!(sources.live_items.contains(&shown));

        handle_source_command(&mut database, SourceCommand::Delete(hidden)).unwrap();
        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert!(!sources.live_items.contains(&hidden));
        assert!(sources.live_items.contains(&shown));
    }

    #[test]
    fn deleting_a_scene_item_removes_only_an_orphaned_source() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let first_scene = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        let (first_item, source_id, second_item) = database
            .transaction(|transaction| {
                let first_item = SourceStore::add_color(transaction, first_scene)?;
                let source_id = transaction.query_row(
                    "SELECT source_id FROM scene_items WHERE id = ?1",
                    [first_item.0],
                    |row| row.get::<_, i64>(0),
                )?;
                let second_scene = SceneStore::add(transaction)?;
                transaction.execute(
                    "INSERT INTO scene_items
                        (scene_id, source_id, position_x, position_y, z_index)
                     VALUES (?1, ?2, 960, 540, 0)",
                    rusqlite::params![second_scene.0, source_id],
                )?;
                Ok((
                    first_item,
                    source_id,
                    crate::domain::SceneItemId(transaction.last_insert_rowid()),
                ))
            })
            .unwrap();

        handle_source_command(&mut database, SourceCommand::Delete(first_item)).unwrap();
        let source_count: i64 = database
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sources WHERE id = ?1",
                [source_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_count, 1);

        handle_source_command(&mut database, SourceCommand::Delete(second_item)).unwrap();
        let source_count: i64 = database
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sources WHERE id = ?1",
                [source_id],
                |row| row.get(0),
            )
            .unwrap();
        let settings_count: i64 = database
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM color_source_settings WHERE source_id = ?1",
                [source_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source_count, 0);
        assert_eq!(settings_count, 0);
    }
}
