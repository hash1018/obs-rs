use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use crate::domain::Scene;
use crate::persistence::{PersistenceResult, ProjectDatabase, SceneStore};
use crate::snapshots::{SceneSnapshot, ScenesSnapshot};

use super::{ProjectCommand, SceneCommand};

enum ManagerMessage {
    Execute(ProjectCommand),
    Shutdown,
}

pub enum ProjectUpdate {
    Scenes(ScenesSnapshot),
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
    }
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

fn publish_snapshot(
    database: &ProjectDatabase,
    update_tx: &Sender<ProjectUpdate>,
    wake_ui: &impl Fn(),
) {
    let update = match scene_snapshot(database) {
        Ok(snapshot) => ProjectUpdate::Scenes(snapshot),
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

        handle_scene_command(&mut database, SceneCommand::Delete(second)).unwrap();
        let snapshot = scene_snapshot(&database).unwrap();
        assert_eq!(snapshot.items.len(), 2);
    }
}
