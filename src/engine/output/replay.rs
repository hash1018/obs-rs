//! The replay buffer: the last stretch of what is composited, encoded and
//! held in memory, saved as a clip when asked.
//!
//! # An output like the other two
//!
//! The same two branches a recording is — the compositor's `Tee` and the
//! mixer's, each through an encoder — ending in `media-pp`'s `ReplayBuffer`
//! where a recording has a muxer. So it is started through [`Output::start`]
//! with that buffer in the muxer's place, encoded with the recording's own
//! settings, and costs what a recording costs for as long as it runs: an
//! encoder of its own, and its window of packets in memory. Nothing is
//! written until a clip is saved.
//!
//! # Saving
//!
//! A save writes the whole clip to disk, which is not something the engine
//! loop can wait for, so it happens on a thread of its own and answers down
//! the loop's channel — the way a screenshot's branch does. The buffer goes
//! on filling meanwhile; a second save while the first is writing takes the
//! window as it then stands.

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::Duration;

use media_pp::elements::{ReplayBuffer, ReplayBufferError, ReplayBufferHandle};

use super::super::backend::{Backend, BackendError};
use super::{Output, OutputKind, OutputState};
use crate::snapshots::ReplayFailure;

/// What one save came to: the clip's path and how long it is, or why there
/// is none.
pub(in crate::engine) type Saved = Result<(PathBuf, Duration), ReplayFailure>;

/// The replay buffer while it runs.
pub(in crate::engine) struct Replay {
    output: Output,
    handle: ReplayBufferHandle,
    /// How much it was started to keep. The settings can have moved since,
    /// and apply to the next start rather than to this one.
    length: Duration,
}

impl Replay {
    /// Starts filling, with the recording's settings as they are now —
    /// including its fallback to an encoder this machine can open.
    pub(in crate::engine) fn start(
        backend: &Backend,
        state: &OutputState,
    ) -> Result<Self, BackendError> {
        let audio_codecs = super::available_audio_codecs(state.mix_format());
        let settings = super::session::usable_settings(backend, &audio_codecs, &state.settings);
        let length = settings.replay_length();
        let mut opened = None;
        let output = Output::start(
            backend,
            state.mixer.as_ref(),
            OutputKind::Replay,
            backend.frame_rate(),
            &settings.encoding(backend.size),
            |tracks| {
                let mut buffer = ReplayBuffer::create(length);
                let added = tracks.map(|track| buffer.add_stream(track.name, track.format));
                let (mut sinks, handle) = buffer.open()?;
                opened = Some(handle);
                Ok(added.try_map(|track| sinks.take(track))?)
            },
        )?;
        let handle = opened.ok_or("the replay buffer did not open")?;
        tracing::info!("replay buffer keeping the last {}s", length.as_secs());
        Ok(Self {
            output,
            handle,
            length,
        })
    }

    /// Ends both branches, which lets go of everything held. A save already
    /// writing finishes with what it took.
    pub(in crate::engine) fn stop(self, backend: &Backend) -> Result<(), BackendError> {
        self.output.stop(backend)
    }

    /// The handle that says how much is held, and how much it was started
    /// to keep — what the UI reads.
    pub(in crate::engine) fn fill(&self) -> (ReplayBufferHandle, Duration) {
        (self.handle.clone(), self.length)
    }

    /// How much is held now.
    pub(in crate::engine) fn buffered(&self) -> Duration {
        self.handle.buffered()
    }

    /// Writes what is held to `path` — or beside it, where that name is
    /// taken — on a thread of its own, answering through `reply` once the
    /// file is complete or has failed.
    pub(in crate::engine) fn save(
        &self,
        path: PathBuf,
        reply: impl FnOnce(Saved) + Send + 'static,
    ) -> std::io::Result<JoinHandle<()>> {
        let handle = self.handle.clone();
        std::thread::Builder::new()
            .name("replay-save".to_owned())
            .spawn(move || reply(write(&handle, &path)))
    }
}

/// One save, start to finish.
///
/// The name is claimed before anything is written: `media-pp` writes over
/// whatever is at the path it is given, and two saves inside one second
/// share a stamp. What is left behind by a save that failed is removed,
/// the claim included.
fn write(handle: &ReplayBufferHandle, path: &Path) -> Saved {
    let failed = |error: std::io::Error| ReplayFailure::Save(error.to_string());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    let (claim, path) = super::screenshot::create_unique(path).map_err(failed)?;
    // Closed before the muxer opens it again, which Windows would refuse
    // while this handle is still open.
    drop(claim);
    match handle.save(&path) {
        Ok(length) => Ok((path, length)),
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            // Checked before a save is started, but the window can empty
            // between the two — a timeline that restarted — and that is the
            // same news as having asked too early.
            Err(match error {
                media_pp::Error::ReplayBufferError(ReplayBufferError::Empty) => {
                    ReplayFailure::Empty
                }
                error => ReplayFailure::Save(super::describe(&error)),
            })
        }
    }
}

/// Every save still writing, so the engine can wait for them on the way out
/// rather than leave a clip half written.
#[derive(Default)]
pub(in crate::engine) struct Saves(Vec<JoinHandle<()>>);

impl Saves {
    pub(in crate::engine) fn add(&mut self, save: JoinHandle<()>) {
        self.0.push(save);
    }

    /// Joins those that have answered. Called when one does, so the list
    /// holds only what is still writing.
    pub(in crate::engine) fn collect_finished(&mut self) {
        let (finished, running) = std::mem::take(&mut self.0)
            .into_iter()
            .partition(JoinHandle::is_finished);
        self.0 = running;
        for save in finished {
            join(save);
        }
    }

    /// Waits for every save still writing.
    pub(in crate::engine) fn wait(&mut self) {
        for save in self.0.drain(..) {
            join(save);
        }
    }
}

fn join(save: JoinHandle<()>) {
    if save.join().is_err() {
        tracing::error!("a replay save panicked");
    }
}
