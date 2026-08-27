use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use crate::domain::{Scene, SceneItem, Source};
use crate::persistence::{PersistenceResult, ProjectDatabase, SceneStore, SourceStore};
use crate::snapshots::{SceneItemSnapshot, SceneSnapshot, ScenesSnapshot, SourcesSnapshot};

use super::{ProjectCommand, SceneCommand, SourceCommand};

enum ManagerMessage {
    Execute(ProjectCommand),
    Shutdown,
}

pub enum ProjectUpdate {
    Snapshot {
        scenes: ScenesSnapshot,
        sources: SourcesSnapshot,
    },
    Error(String),
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
        let _ = self.command_tx.send(ManagerMessage::Execute(command));
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
    }
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
        SourceCommand::AddDisplayCapture { scene_id, settings } => {
            SourceStore::add_display_capture(transaction, scene_id, &settings)?;
            Ok(())
        }
        SourceCommand::Delete(scene_item_id) => {
            SourceStore::delete_scene_item(transaction, scene_item_id)
        }
        SourceCommand::SetTransform(scene_item_id, transform) => {
            SourceStore::set_transform(transaction, scene_item_id, transform)
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
    let Some(scene_id) = scenes.selected_scene_id else {
        return Ok(SourcesSnapshot::default());
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
    })
}

fn project_snapshot(
    database: &ProjectDatabase,
) -> PersistenceResult<(ScenesSnapshot, SourcesSnapshot)> {
    let scenes = scene_snapshot(database)?;
    let sources = sources_snapshot(database, &scenes)?;
    Ok((scenes, sources))
}

fn publish_snapshot(
    database: &ProjectDatabase,
    update_tx: &Sender<ProjectUpdate>,
    wake_ui: &impl Fn(),
) {
    let update = match project_snapshot(database) {
        Ok((scenes, sources)) => ProjectUpdate::Snapshot { scenes, sources },
        Err(error) => ProjectUpdate::Error(error.to_string()),
    };
    let _ = update_tx.send(update);
    wake_ui();
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let (_, sources) = project_snapshot(&database).unwrap();
        assert_eq!(sources.scene_id, Some(first_scene));
        assert_eq!(sources.items.len(), 1);
        assert_eq!(sources.items[0].name, "Display Capture");

        handle_scene_command(&mut database, SceneCommand::Add).unwrap();
        let (_, sources) = project_snapshot(&database).unwrap();
        assert!(sources.items.is_empty());
        assert_eq!(sources.scene_name.as_deref(), Some("Scene 2"));
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
        let (_, sources) = project_snapshot(&database).unwrap();
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

        let (_, sources) = project_snapshot(&database).unwrap();
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

        let (_, sources) = project_snapshot(&database).unwrap();
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

        let (_, sources) = project_snapshot(&database).unwrap();
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
