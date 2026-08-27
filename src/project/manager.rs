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
                settings,
                visible,
                locked,
                transform,
                crop,
            }
        })
        .collect();

    Ok(SourcesSnapshot {
        canvas: crate::domain::SceneCanvas::DEFAULT,
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
                let source = SourceStore::create_for_test(
                    transaction,
                    "Display Capture",
                    crate::domain::SourceKind::DisplayCapture,
                )?;
                SourceStore::add_to_scene_for_test(transaction, first_scene, source)?;
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
}
