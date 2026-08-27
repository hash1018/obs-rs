use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use crate::persistence::{PersistenceResult, ProjectDatabase, SceneStore};
use crate::scene::SceneAction;
use crate::snapshots::ScenesSnapshot;

enum ProjectCommand {
    Scene(SceneAction),
    Shutdown,
}

pub enum ProjectUpdate {
    Scenes(ScenesSnapshot),
    Error(String),
}

pub struct ProjectManager {
    command_tx: Sender<ProjectCommand>,
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

    pub fn dispatch(&self, action: SceneAction) {
        let _ = self.command_tx.send(ProjectCommand::Scene(action));
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
        let _ = self.command_tx.send(ProjectCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run(
    mut database: ProjectDatabase,
    command_rx: Receiver<ProjectCommand>,
    update_tx: &Sender<ProjectUpdate>,
    wake_ui: &impl Fn(),
) {
    publish_snapshot(&database, update_tx, wake_ui);

    while let Ok(command) = command_rx.recv() {
        match command {
            ProjectCommand::Scene(action) => {
                let result = handle_scene_action(&mut database, action);
                match result {
                    Ok(()) => publish_snapshot(&database, update_tx, wake_ui),
                    Err(error) => {
                        let _ = update_tx.send(ProjectUpdate::Error(error.to_string()));
                        wake_ui();
                    }
                }
            }
            ProjectCommand::Shutdown => break,
        }
    }
}

fn handle_scene_action(
    database: &mut ProjectDatabase,
    action: SceneAction,
) -> PersistenceResult<()> {
    database.transaction(|transaction| match action {
        SceneAction::Add => {
            SceneStore::add(transaction)?;
            Ok(())
        }
        SceneAction::Delete(scene_id) => SceneStore::delete(transaction, scene_id),
        SceneAction::Duplicate(scene_id) => SceneStore::duplicate(transaction, scene_id),
        SceneAction::MoveUp(scene_id) => SceneStore::move_up(transaction, scene_id),
        SceneAction::MoveDown(scene_id) => SceneStore::move_down(transaction, scene_id),
        SceneAction::Select(scene_id) => SceneStore::select(transaction, scene_id),
    })
}

fn publish_snapshot(
    database: &ProjectDatabase,
    update_tx: &Sender<ProjectUpdate>,
    wake_ui: &impl Fn(),
) {
    let update = match SceneStore::snapshot(database.connection()) {
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
    fn scene_actions_are_persisted_and_ordered() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();

        handle_scene_action(&mut database, SceneAction::Add).unwrap();
        let snapshot = SceneStore::snapshot(database.connection()).unwrap();
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.items[1].name, "Scene 2");

        let second = snapshot.items[1].id;
        handle_scene_action(&mut database, SceneAction::MoveUp(second)).unwrap();
        let snapshot = SceneStore::snapshot(database.connection()).unwrap();
        assert_eq!(snapshot.items[0].id, second);

        handle_scene_action(&mut database, SceneAction::Duplicate(second)).unwrap();
        let snapshot = SceneStore::snapshot(database.connection()).unwrap();
        assert_eq!(snapshot.items.len(), 3);

        handle_scene_action(&mut database, SceneAction::Delete(second)).unwrap();
        let snapshot = SceneStore::snapshot(database.connection()).unwrap();
        assert_eq!(snapshot.items.len(), 2);
    }
}
