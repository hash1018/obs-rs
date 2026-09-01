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
        /// Boxed because it is much the largest thing sent here — every
        /// SceneItem in the selected Scene, and every Source name in the
        /// project — and an `Error` would otherwise be padded to its size.
        sources: Box<SourcesSnapshot>,
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
        SourceCommand::AddWindowCapture { scene_id, settings } => {
            SourceStore::add_window_capture(transaction, scene_id, &settings)?;
            Ok(())
        }
        SourceCommand::AddMediaFile { scene_id, settings } => {
            SourceStore::add_media_file(transaction, scene_id, &settings)?;
            Ok(())
        }
        SourceCommand::AddImage { scene_id, settings } => {
            SourceStore::add_image(transaction, scene_id, &settings)?;
            Ok(())
        }
        SourceCommand::SetMediaLooping(scene_item_id, looping) => {
            SourceStore::set_media_looping(transaction, scene_item_id, looping)
        }
        SourceCommand::SetMediaGain(scene_item_id, gain_db) => {
            SourceStore::set_media_gain_db(transaction, scene_item_id, gain_db)
        }
        SourceCommand::SetMediaMuted(scene_item_id, muted) => {
            SourceStore::set_media_muted(transaction, scene_item_id, muted)
        }
        SourceCommand::SetMediaPaused(scene_item_id, paused) => {
            SourceStore::set_media_paused(transaction, scene_item_id, paused)
        }
        SourceCommand::Delete(scene_item_id) => {
            SourceStore::delete_scene_item(transaction, scene_item_id)
        }
        SourceCommand::MoveUp(scene_item_id) => SourceStore::move_up(transaction, scene_item_id),
        SourceCommand::MoveDown(scene_item_id) => {
            SourceStore::move_down(transaction, scene_item_id)
        }
        SourceCommand::Rename(scene_item_id, name) => {
            SourceStore::rename(transaction, scene_item_id, &name)
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
    let names = SourceStore::names(database.connection())?;
    let Some(scene_id) = scenes.selected_scene_id else {
        return Ok(SourcesSnapshot {
            live_items,
            names,
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
                // Filled in later, from the engine — see `ObsApp::poll_media_levels`.
                peak_db: None,
                position: None,
            }
        })
        .collect();

    Ok(SourcesSnapshot {
        canvas,
        scene_id: Some(scene_id),
        scene_name,
        items,
        live_items,
        names,
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
            sources: Box::new(sources),
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
    fn window_capture_source_persists_the_pair_that_finds_its_window() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();

        handle_source_command(
            &mut database,
            SourceCommand::AddWindowCapture {
                scene_id,
                settings: crate::domain::WindowCaptureSettings {
                    target: crate::domain::WindowCaptureTarget::Window {
                        process: "notepad.exe".into(),
                        title: "Untitled - Notepad".into(),
                    },
                    size_hint: Some([1280, 720]),
                },
            },
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(sources.items.len(), 1);
        assert_eq!(sources.items[0].name, "Window Capture");
        assert_eq!(sources.items[0].source_size, [1280.0, 720.0]);
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::WindowCapture(settings)
                if settings.target
                    == crate::domain::WindowCaptureTarget::Window {
                        process: "notepad.exe".into(),
                        title: "Untitled - Notepad".into(),
                    }
                    && settings.size_hint == Some([1280, 720])
        ));
    }

    #[test]
    fn image_source_is_named_after_its_file_and_keeps_the_path() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        let path = std::path::PathBuf::from("/pictures/logo.png");

        handle_source_command(
            &mut database,
            SourceCommand::AddImage {
                scene_id,
                settings: crate::domain::ImageSourceSettings {
                    path: path.clone(),
                    size_hint: Some([512, 128]),
                },
            },
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(sources.items.len(), 1);
        assert_eq!(sources.items[0].name, "logo");
        // Its own shape, not the Canvas's: a logo is not 1920 wide.
        assert_eq!(sources.items[0].source_size, [512.0, 128.0]);
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::Image(settings)
                if settings.path == path && settings.size_hint == Some([512, 128])
        ));
    }

    #[test]
    fn media_file_source_is_named_after_its_file_and_keeps_the_path() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        let path = std::path::PathBuf::from("/media/holiday clip.mp4");

        handle_source_command(
            &mut database,
            SourceCommand::AddMediaFile {
                scene_id,
                settings: crate::domain::MediaFileSettings {
                    path: path.clone(),
                    looping: false,
                    size_hint: Some([1280, 720]),
                    has_audio: true,
                    gain_db: 0.0,
                    muted: false,
                    duration: None,
                    paused: false,
                },
            },
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(sources.items.len(), 1);
        // The file's own name rather than "Media Source": what someone
        // reading the Sources list is looking for is which file this is.
        assert_eq!(sources.items[0].name, "holiday clip");
        assert_eq!(sources.items[0].source_size, [1280.0, 720.0]);
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::MediaFile(settings)
                if settings.path == path
                    && !settings.looping
                    && settings.size_hint == Some([1280, 720])
        ));
    }

    /// Two files can share a name and live in different folders, which is
    /// exactly when naming a Source after its file needs the same
    /// disambiguation every other kind gets.
    #[test]
    fn two_media_files_with_one_name_are_still_two_sources() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();

        for folder in ["/a", "/b"] {
            handle_source_command(
                &mut database,
                SourceCommand::AddMediaFile {
                    scene_id,
                    settings: crate::domain::MediaFileSettings {
                        path: std::path::PathBuf::from(format!("{folder}/clip.mp4")),
                        looping: false,
                        size_hint: None,
                        has_audio: false,
                        gain_db: 0.0,
                        muted: false,
                        duration: None,
                        paused: false,
                    },
                },
            )
            .unwrap();
        }

        let (_, sources, _) = project_snapshot(&database).unwrap();
        let mut names: Vec<&str> = sources
            .items
            .iter()
            .map(|item| item.name.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["clip", "clip 2"]);
        // No size to start at, so both stand in at Canvas size the way every
        // other Source with no hint does.
        assert_eq!(sources.items[0].source_size, [1920.0, 1080.0]);
    }

    /// The fader and the mute button are the Source's own, the way a device
    /// channel's are the device's — and a file with no sound still carries
    /// them, because nothing here knows that and the columns cost nothing.
    #[test]
    fn media_file_keeps_its_own_fader_and_mute() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();

        handle_source_command(
            &mut database,
            SourceCommand::AddMediaFile {
                scene_id,
                settings: crate::domain::MediaFileSettings {
                    path: std::path::PathBuf::from("/media/clip.mp4"),
                    looping: false,
                    size_hint: None,
                    has_audio: true,
                    gain_db: 0.0,
                    muted: false,
                    duration: None,
                    paused: false,
                },
            },
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        let item_id = sources.items[0].id;
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::MediaFile(settings)
                if settings.has_audio && settings.gain_db == 0.0 && !settings.muted
        ));

        handle_source_command(&mut database, SourceCommand::SetMediaGain(item_id, -12.0)).unwrap();
        handle_source_command(&mut database, SourceCommand::SetMediaMuted(item_id, true)).unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::MediaFile(settings)
                if settings.gain_db == -12.0 && settings.muted
        ));
    }

    /// A fader cannot be pushed past the ends of its own scale, whatever
    /// asks: the dock clamps what it draws, and this is what a command
    /// arriving from anywhere else meets.
    #[test]
    fn a_media_file_gain_is_held_to_the_faders_range() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();

        handle_source_command(
            &mut database,
            SourceCommand::AddMediaFile {
                scene_id,
                settings: crate::domain::MediaFileSettings {
                    path: std::path::PathBuf::from("/media/clip.mp4"),
                    looping: false,
                    size_hint: None,
                    has_audio: true,
                    gain_db: 0.0,
                    muted: false,
                    duration: None,
                    paused: false,
                },
            },
        )
        .unwrap();
        let (_, sources, _) = project_snapshot(&database).unwrap();
        let item_id = sources.items[0].id;

        handle_source_command(&mut database, SourceCommand::SetMediaGain(item_id, -400.0)).unwrap();
        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::MediaFile(settings)
                if settings.gain_db == crate::domain::MIN_GAIN_DB
        ));

        handle_source_command(&mut database, SourceCommand::SetMediaGain(item_id, 400.0)).unwrap();
        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::MediaFile(settings)
                if settings.gain_db == crate::domain::MAX_GAIN_DB
        ));
    }

    /// The two things a paused clip has to survive: the Scene changing under
    /// it, and the application being restarted. Both are the same test from
    /// the project's side — it either wrote the flag down or it did not.
    #[test]
    fn media_file_pause_and_length_are_written_down() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();

        handle_source_command(
            &mut database,
            SourceCommand::AddMediaFile {
                scene_id,
                settings: crate::domain::MediaFileSettings {
                    path: std::path::PathBuf::from("/media/clip.mp4"),
                    looping: false,
                    size_hint: None,
                    has_audio: false,
                    gain_db: 0.0,
                    muted: false,
                    duration: Some(std::time::Duration::from_millis(40_037)),
                    paused: false,
                },
            },
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        let item_id = sources.items[0].id;
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::MediaFile(settings)
                if settings.duration == Some(std::time::Duration::from_millis(40_037))
                    && !settings.paused
        ));
        // A measurement, not something the project holds — it is filled from
        // the engine on every pass and is absent until one runs.
        assert_eq!(sources.items[0].position, None);

        handle_source_command(&mut database, SourceCommand::SetMediaPaused(item_id, true)).unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::MediaFile(settings) if settings.paused
        ));
    }

    #[test]
    fn media_file_looping_is_off_until_it_is_switched_on() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();

        handle_source_command(
            &mut database,
            SourceCommand::AddMediaFile {
                scene_id,
                settings: crate::domain::MediaFileSettings {
                    path: std::path::PathBuf::from("/media/clip.mp4"),
                    looping: false,
                    size_hint: None,
                    has_audio: true,
                    gain_db: 0.0,
                    muted: false,
                    duration: None,
                    paused: false,
                },
            },
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        let item_id = sources.items[0].id;

        handle_source_command(&mut database, SourceCommand::SetMediaLooping(item_id, true))
            .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::MediaFile(settings) if settings.looping
        ));

        handle_source_command(
            &mut database,
            SourceCommand::SetMediaLooping(item_id, false),
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::MediaFile(settings) if !settings.looping
        ));
    }

    #[test]
    fn portal_window_capture_persists_no_target_until_the_portal_answers() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let scene_id = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();

        // How a portal Window Capture is always added: nothing has been
        // chosen yet, because the portal does the choosing when the Source
        // first opens.
        handle_source_command(
            &mut database,
            SourceCommand::AddWindowCapture {
                scene_id,
                settings: crate::domain::WindowCaptureSettings {
                    target: crate::domain::WindowCaptureTarget::Portal {
                        restore_token: None,
                    },
                    size_hint: None,
                },
            },
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        let item_id = sources.items[0].id;
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::WindowCapture(settings)
                if settings.target
                    == crate::domain::WindowCaptureTarget::Portal { restore_token: None }
        ));

        // And what the first open produces is stored under the same item, so
        // the second run reopens the window instead of asking again.
        handle_source_command(
            &mut database,
            SourceCommand::SetRestoreToken(item_id, Some("window-token".into())),
        )
        .unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert!(matches!(
            &sources.items[0].settings,
            crate::domain::SourceSettings::WindowCapture(settings)
                if settings.target
                    == crate::domain::WindowCaptureTarget::Portal {
                        restore_token: Some("window-token".into()),
                    }
        ));
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

    /// The Sources dock refuses a name before sending it, and what it checks
    /// against is this set. A name held by a Source in another Scene is one
    /// the database would refuse and the dock would otherwise have offered.
    #[test]
    fn the_snapshot_carries_every_source_name_in_the_project() {
        let mut database = ProjectDatabase::open_in_memory().unwrap();
        let first_scene = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        handle_source_command(&mut database, SourceCommand::AddColor(first_scene)).unwrap();

        handle_scene_command(&mut database, SceneCommand::Add).unwrap();
        let second_scene = scene_snapshot(&database)
            .unwrap()
            .selected_scene_id
            .unwrap();
        handle_source_command(&mut database, SourceCommand::AddDrawing(second_scene)).unwrap();

        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(
            sources.items.len(),
            1,
            "only the selected Scene's items are shown"
        );
        assert!(sources.names.contains("Color Source"));
        assert!(sources.names.contains("Drawing"));

        // And a rename replaces the old name rather than adding to the set,
        // so the name just given up can be taken by something else.
        let item_id = sources.items[0].id;
        handle_source_command(
            &mut database,
            SourceCommand::Rename(item_id, "Whiteboard".into()),
        )
        .unwrap();
        let (_, sources, _) = project_snapshot(&database).unwrap();
        assert_eq!(sources.items[0].name, "Whiteboard");
        assert!(sources.names.contains("Whiteboard"));
        assert!(!sources.names.contains("Drawing"));
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
