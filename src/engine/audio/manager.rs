//! The audio thread, and what the rest of the application says to it.
//!
//! Separate from `EngineManager` rather than folded into it because opening a
//! capture opens the audio subsystem and can block for a moment — not
//! something to do on the UI thread — and the video engine still starts when
//! its backend cannot start, which is no reason to lose the microphone.

use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use arc_swap::ArcSwapOption;

use media_pp::elements::{MixFormat, MixerHandle, TeeHandle};

use crate::capture::AudioDeviceTarget;
use crate::domain::AudioSourceId;
use crate::snapshots::AudioSnapshot;

use super::{AudioEngine, Levels};

/// How often the worker looks at what it has open when nothing has woken it.
///
/// The same second the video engine gives its own Sources, and for the same
/// question — see [`AudioEngine::close_ended`]. Nothing else needs a tick:
/// every other reason to act arrives as a command or an endpoint
/// notification.
const HEALTH_INTERVAL: Duration = Duration::from_secs(1);

/// What reaches the audio thread.
///
/// Two things move the graph, and they are not the same event: the project
/// saying what should be running, and the machine saying what it can be run
/// on. Either alone is not enough to decide anything — a source is open when
/// the project asks for it *and* an endpoint exists to open it on.
enum AudioCommand {
    /// What the project now holds.
    Project(Box<AudioSnapshot>),
    /// One source's gain, mid-gesture.
    ///
    /// Separate from `Project` because a fader being dragged is not an edit
    /// yet: the project hears about it once, when the gesture ends, and this
    /// is what the audio hears in the meantime. Same split the Preview's own
    /// drag makes between the compositor and the project.
    Gain(AudioSourceId, f32),
    /// An endpoint appeared, went, or became the default. Says to look
    /// again, not what changed — see [`crate::capture::watch_audio_devices`].
    DevicesChanged,
    /// What the mix should be summed into from now on.
    MixFormat(MixFormat),
    /// Which endpoint monitoring plays to, or `None` for none.
    MonitorDevice(Option<String>),
    /// Ends the worker's loop.
    ///
    /// Every sender dropping cannot do it: the endpoint watch owns a sender
    /// of its own, and the watch lives on the worker's own stack — so the
    /// channel stays open exactly as long as the loop that would close it,
    /// and `recv` blocks forever. Being told is the way out.
    Shutdown,
}

/// Owns the audio graph on a thread of its own.
///
/// Separate from `EngineManager` rather than folded into it: building a
/// capture opens the audio subsystem and can block for a moment, which is not
/// something to do on the UI thread — and the video engine's thread exits
/// when its backend cannot start, which is no reason to lose the microphone.
pub struct AudioManager {
    /// `Option` only so `Drop` can close the channel by taking it, which is
    /// what ends the worker's loop.
    commands: Option<Sender<AudioCommand>>,
    /// Republished on every rebuild, because the set of sources it covers
    /// changes with them. `None` until the first graph is built.
    levels: Arc<ArcSwapOption<Levels>>,
    /// Every endpoint the machine currently has, for the device pickers.
    ///
    /// Published from here rather than enumerated by the UI because this is
    /// the side that is told when the answer changes. A list taken once at
    /// startup is missing whatever was plugged in since.
    devices: Arc<ArcSwapOption<Vec<AudioDeviceTarget>>>,
    /// The mix that is played back, for a Source that carries its own sound.
    ///
    /// Republished rather than taken once like [`AudioManager::mixer`],
    /// because unlike the recording's mixer this one comes and goes: it is
    /// `None` until a monitoring endpoint is chosen, and `None` again when
    /// one is taken away.
    monitor: Arc<ArcSwapOption<MixerHandle>>,
    /// Where a recording attaches its audio track, taken once here rather
    /// than asked for later: the recording is opened on the *video* thread,
    /// which cannot reach into this one to fetch it. `None` when the mixer
    /// failed to start.
    /// The mixer's fan-out and its control, taken once at startup because the
    /// mixer lives on a thread the engine cannot ask. `None` when it never
    /// started, which records video only.
    mixer: Option<(TeeHandle, MixerHandle)>,
    worker: Option<JoinHandle<()>>,
}

impl AudioManager {
    pub fn spawn(format: MixFormat, wake_ui: impl Fn() + Send + 'static) -> std::io::Result<Self> {
        let (commands, command_rx) = mpsc::channel::<AudioCommand>();
        let levels = Arc::new(ArcSwapOption::empty());
        let devices = Arc::new(ArcSwapOption::empty());
        let monitor: Arc<ArcSwapOption<MixerHandle>> = Arc::new(ArcSwapOption::empty());
        // The engine is built on its own thread and stays there — it holds
        // FFmpeg state that is not `Send`, so it cannot be made here and
        // moved. Only the mixer's `Tee` comes back, over this channel: a
        // recording is opened on the *video* thread, which has no way to
        // reach into this one and ask for it later.
        let (ready, mixer_rx) = mpsc::channel::<Option<(TeeHandle, MixerHandle)>>();
        let worker = thread::Builder::new().name("audio".to_owned()).spawn({
            let levels = Arc::clone(&levels);
            let published_devices = Arc::clone(&devices);
            let published_monitor = Arc::clone(&monitor);
            let watch_commands = commands.clone();
            move || {
                let mut engine = AudioEngine::new(format);
                // The worker's own copy, because the monitoring branch is
                // built against it and a device change arrives without one.
                let mut mix_format = format;
                let _ = ready.send(engine.mixer_access());
                published_monitor.store(engine.monitor_access().map(Arc::new));
                let mut known_devices = crate::capture::audio_devices();
                published_devices.store(Some(Arc::new(known_devices.clone())));
                // Held for the life of the loop: dropping it stops the
                // notifications, and the callback it owns is what sends the
                // command below.
                let _watch = crate::capture::watch_audio_devices(move || {
                    let _ = watch_commands.send(AudioCommand::DevicesChanged);
                });
                // The project's own snapshot, kept because an endpoint
                // change has to be answered with it — what to run is still
                // the project's answer, and the machine only changed what
                // can be run.
                let mut project: Option<AudioSnapshot> = None;

                // Ends when the manager says so — see `AudioCommand::Shutdown`.
                // Dropping the engine on the way out stops every capture and
                // the mixer, and dropping the watch stops the notifications.
                loop {
                    // Timed rather than blocking, for the one thing no
                    // command reports: a capture that stopped by itself. See
                    // `AudioEngine::close_ended` for which failures reach
                    // here and which arrive as a notification instead.
                    let woken = match command_rx.recv_timeout(HEALTH_INTERVAL) {
                        Ok(command) => Some(command),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    let ticked = woken.is_none();
                    // Coalesced before anything is acted on. Windows raises
                    // several notifications for one plug, and a fader being
                    // dragged sends a snapshot per frame; both collapse to
                    // the one state they all describe.
                    let mut devices_changed = false;
                    let mut project_changed = false;
                    let mut ending = false;
                    let mut gains: Vec<(AudioSourceId, f32)> = Vec::new();
                    for command in woken.into_iter().chain(command_rx.try_iter()) {
                        match command {
                            AudioCommand::Project(snapshot) => {
                                project = Some(*snapshot);
                                project_changed = true;
                            }
                            AudioCommand::Gain(id, gain_db) => {
                                // Last one wins per source: a drag sends one
                                // per frame and only the newest is where the
                                // fader is now.
                                gains.retain(|(other, _)| *other != id);
                                gains.push((id, gain_db));
                            }
                            AudioCommand::DevicesChanged => devices_changed = true,
                            AudioCommand::MixFormat(format) => {
                                mix_format = format;
                                engine.set_mix_format(format);
                                // The monitor branch is rebuilt at the new
                                // rate, and can fail to be — which changes
                                // what wiring every source should have.
                                project_changed = true;
                            }
                            AudioCommand::MonitorDevice(device) => {
                                engine.set_monitor_device(device.as_deref(), mix_format);
                                // Published after the attempt rather than
                                // from the request: an endpoint that refused
                                // to open leaves no mix to register with,
                                // and a Source told otherwise would play to
                                // nothing.
                                published_monitor.store(engine.monitor_access().map(Arc::new));
                                // Whether there is anywhere to monitor to
                                // decides how every source is wired, so this
                                // is a reason to reconcile even though the
                                // project did not move.
                                project_changed = true;
                            }
                            AudioCommand::Shutdown => ending = true,
                        }
                    }
                    if ending {
                        break;
                    }
                    // Before the device list is refreshed below, so a source
                    // this closes is reopened in the same pass where its
                    // endpoint is still there — and left closed where it is
                    // not.
                    if engine.close_ended() {
                        // Asked rather than assumed: a capture ending by
                        // itself is evidence the endpoint picture may have
                        // moved without this thread being told, which is the
                        // whole reason the check exists.
                        devices_changed = true;
                        project_changed = true;
                    }
                    if devices_changed {
                        known_devices = crate::capture::audio_devices();
                        published_devices.store(Some(Arc::new(known_devices.clone())));
                    }
                    // Only when something it reads has moved. A fader being
                    // dragged wakes this loop sixty times a second, and
                    // reconciling the whole graph against a project that has
                    // not changed is the work `AudioCommand::Gain` exists to
                    // avoid — it would also re-apply the stored gain over the
                    // one being dragged, every frame.
                    if (project_changed || devices_changed)
                        && let Some(project) = &project
                    {
                        engine.apply(project, &known_devices);
                    }
                    // A bare health tick that found everything running has no
                    // counters to republish and no reason to wake the UI.
                    if ticked && !project_changed && !devices_changed && gains.is_empty() {
                        continue;
                    }
                    // After any apply, not before: a project that arrived in
                    // the same batch carries the gain from before the drag,
                    // and applying it second would undo every frame of one.
                    for (id, gain_db) in gains {
                        engine.set_gain_db(id, gain_db);
                    }
                    // Cloned, not taken: an apply that changed nothing must
                    // leave the engine holding the counters it is still
                    // writing into.
                    levels.store(Some(Arc::new(engine.levels().clone())));
                    wake_ui();
                }
            }
        })?;
        Ok(Self {
            commands: Some(commands),
            levels,
            devices,
            monitor,
            // Waits only for the mixer to be built, which is the worker's
            // first act. An `Err` means it never got that far, which is the
            // same answer as a mixer that failed: record without audio.
            mixer: mixer_rx.recv().ok().flatten(),
            worker: Some(worker),
        })
    }

    /// Sets one source's gain now, without waiting for the project.
    ///
    /// For a fader being dragged. What is heard follows the handle, and the
    /// project is told when the gesture ends — so the level moves under the
    /// pointer while the database still records one edit per drag.
    ///
    /// Dropped if the source is not open. A gain arriving for something with
    /// no capture behind it has nothing to set, and the next `apply` carries
    /// the value anyway.
    pub fn set_gain_db(&self, id: AudioSourceId, gain_db: f32) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(AudioCommand::Gain(id, gain_db));
        }
    }

    /// Tells the audio graph what the project now holds.
    pub fn apply(&self, snapshot: &AudioSnapshot) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(AudioCommand::Project(Box::new(snapshot.clone())));
        }
    }

    /// The most recent peak for one source, or `None` when nothing is
    /// measuring it.
    pub fn peak_db(&self, id: AudioSourceId) -> Option<f32> {
        self.levels.load_full()?.peak_db(id)
    }

    /// Whether this source has a capture running behind it.
    ///
    /// `None` while nothing has been published yet — not "no", because the
    /// two mean opposite things to a dock deciding what to draw, and the
    /// first pass would otherwise hide every channel for a frame.
    pub fn is_running(&self, id: AudioSourceId) -> Option<bool> {
        Some(self.levels.load_full()?.is_running(id))
    }

    /// Every audio endpoint the machine currently has, or `None` before the
    /// first enumeration.
    /// Where a recording's audio track attaches, or `None` when the mixer
    /// never started.
    /// Where a recording's audio track attaches, and the control that says
    /// what format it will be in.
    pub fn mixer(&self) -> Option<(TeeHandle, MixerHandle)> {
        self.mixer.clone()
    }

    /// Tells the mixer what to sum into from now on.
    pub fn set_mix_format(&self, format: MixFormat) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(AudioCommand::MixFormat(format));
        }
    }

    /// Points monitoring at an endpoint, or turns it off with `None`.
    pub fn set_monitor_device(&self, device: Option<String>) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(AudioCommand::MonitorDevice(device));
        }
    }

    pub fn devices(&self) -> Option<Arc<Vec<AudioDeviceTarget>>> {
        self.devices.load_full()
    }

    /// Where the monitor mix is published, for the engine thread to read
    /// again on every pass.
    ///
    /// The slot rather than its contents: unlike the recording's mixer this
    /// one comes and goes with the monitoring endpoint, so a handle taken
    /// once would be a mix nothing plays.
    pub fn monitor_publisher(&self) -> Arc<ArcSwapOption<MixerHandle>> {
        Arc::clone(&self.monitor)
    }
}

impl Drop for AudioManager {
    fn drop(&mut self) {
        // Told, not merely let go of. The worker owns the endpoint watch,
        // which owns a sender for this same channel — so dropping this one
        // leaves `recv` waiting on a sender only the worker could release,
        // and the join below would never return.
        if let Some(commands) = self.commands.take() {
            let _ = commands.send(AudioCommand::Shutdown);
        }
        // Joined rather than detached, because dropping the engine it owns
        // is what stops the captures and the mixer.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::audio::DEFAULT_MIX_FORMAT;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Dropping the manager has to stop its worker and return.
    ///
    /// It did not, once: the endpoint watch holds a `Sender` for the same
    /// channel the worker is receiving on, and the watch lives on the
    /// worker's own stack — so dropping the manager's sender left one alive
    /// that only the worker could release, `recv` never returned, and the
    /// `join` in `Drop` hung the application on exit.
    ///
    /// Run on a thread of its own with a deadline rather than inline,
    /// because the failure being caught is a hang: inline, this test would
    /// not fail, it would stop.
    #[test]
    fn dropping_the_manager_returns_rather_than_hanging() {
        let (finished, done) = mpsc::channel();
        thread::Builder::new()
            .name("audio-drop-under-test".to_owned())
            .spawn(move || {
                // The mixer starts either way; captures do not, since no
                // project snapshot is ever sent. That is enough — the
                // channel and the watch are what this is about.
                let manager = AudioManager::spawn(DEFAULT_MIX_FORMAT, || {});
                drop(manager);
                let _ = finished.send(());
            })
            .expect("spawning the thread under test");

        assert!(
            done.recv_timeout(Duration::from_secs(10)).is_ok(),
            "dropping AudioManager did not return: its worker is still waiting on a sender \
             nothing will drop"
        );
    }
}
