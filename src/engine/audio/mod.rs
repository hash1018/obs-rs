//! The audio half of the engine: capture, per-source gain, and one mix.
//!
//! Independent of the video half, and deliberately not part of a
//! `engine::backend`. Those three — compositor, capture, wgpu interop — are
//! one unit because what a capture produces decides what can accept it. Audio
//! touches none of that: the only platform-specific line here is which
//! element opens a device, and everything after it is the same on both.
//!
//! It is also independent of whether the video half started. A machine with
//! no usable GPU still has a microphone, and losing the mixer because
//! `Backend::start` failed would be losing it for an unrelated reason.
//!
//! # Shape
//!
//! ```text
//! pipeline "audio-1"  CaptureSource ─ AudioVolume ─ Tee ┬─ AppSink   (meters)
//!                                                       └─┐
//! pipeline "audio-2"  CaptureSource ─ AudioVolume ─ Tee ┬─ AppSink
//!                                                       └─┤
//!                                                         ↓
//! pipeline "audio-mix"                    AudioMixer ─ Tee ─ (a recording, when one runs)
//! ```
//!
//! One pipeline each, not one between them. A `Pipeline`'s sources are fixed
//! when it is built, so anything that has to be reopened has to be alone in
//! one — see [`AudioEngine`] for why the mixer above all.
//!
//! # The meters are post-fader
//!
//! The `Tee` hangs off [`AudioVolume`], not off the capture, so a meter shows
//! what the fader let through rather than what arrived at it — pulling one
//! down empties its meter, and so does muting.
//!
//! A `Tee` is not a `Source` — its outputs live behind a lock instead of in
//! `src_pads` — so nothing can follow it in a chain and it cannot be a `pipe`
//! stage. It is what a chain *ends* at, which is what `ChainBuilder::to_branch`
//! takes: the fan-out and the stages in front of it commit as one subgraph, so
//! the recorded topology puts the branching on the fader that really does it.
//!
//! # What a change costs
//!
//! Gain and mute go through handles and cost nothing. A device change costs
//! that one source's capture, which is reopened and registered with the
//! mixer again — nothing else stops, and the mix keeps its timeline.

mod device;
mod level;
mod manager;

pub(super) use level::Levels;
/// Measuring a buffer's peak is one function, and both halves of the engine
/// now need it: the devices' own meters and a media file Source's.
pub(in crate::engine) use level::peak_db;
pub use manager::AudioManager;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use media_pp::{
    buffer::MediaBuffer,
    elements::{
        AppSink, AudioMixer, AudioMixerOptions, AudioResampler, AudioVolume, AudioVolumeHandle,
        MixFormat, MixerHandle, TeeBuilder, TeeHandle,
    },
    ffmpeg,
    graph::BranchId,
    pipeline::Pipeline,
    queue::OverflowPolicy,
};

use crate::capture::AudioDeviceTarget;
use crate::domain::AudioSourceId;
use crate::snapshots::{AudioSnapshot, AudioSourceSnapshot};

use super::backend::BackendError;

/// What every source is mixed into, and therefore what a recording's audio
/// track is made of. 48 kHz stereo is what both backends' devices are
/// overwhelmingly already at, so the mixer's own resampler usually has
/// nothing to do.
/// What the mixer starts at when nothing has been stored yet. What it
/// actually runs at is `AudioSettings`, which it is started with and follows
/// afterwards — ask the `MixerHandle`, never this.
pub(super) const DEFAULT_MIX_FORMAT: MixFormat = MixFormat {
    sample_rate: crate::settings::DEFAULT_SAMPLE_RATE,
    channels: crate::settings::DEFAULT_CHANNELS,
};

/// How deep the monitor branch's queue is, and what it does when it fills.
///
/// Dropping rather than blocking, which is the opposite of the recording
/// branch's policy and the whole reason this has one of its own: a playback
/// device that stalls must not be able to stall the mix a recording is being
/// written from. Monitoring is what somebody is hearing right now — a buffer
/// too late to play is a buffer with no use left — while a recording is a
/// file that has to be complete.
const MONITOR_QUEUE_DEPTH: usize = 8;

/// What the monitor mix knows a source by.
///
/// A different name from the one the recording's mixer uses, though nothing
/// forces it: the two are separate registries and the same string would work
/// in both. It is for the log — a source in both mixes draws two branches
/// into two elements, and identical names in a topology diagram leave no way
/// to tell which of them is which.
fn monitor_registration(name: &str) -> String {
    format!("{name}-monitor")
}

/// Whether a source's sound is put into the monitor mix as well as the
/// recording's.
///
/// It is always in the recording's, which is what makes this one question
/// rather than two — see [`crate::domain::AudioSource::monitored`]. All this
/// adds is the other half of the answer: asking to be monitored where there
/// is nothing to play through is not monitoring, and a source in that state
/// must still reach the recording rather than falling between the two.
///
/// Shared with the Sources that carry their own sound, so a media file and a
/// microphone answer it the same way.
pub(in crate::engine) fn monitors(monitored: bool, monitoring: bool) -> bool {
    monitored && monitoring
}

/// One source that is open, in its own pipeline, and how it was opened.
struct OpenAudioSource {
    /// This source's capture, tee and fader. Dropping it stops that one
    /// capture and nothing else.
    ///
    /// Read as well as held: [`AudioEngine::close_ended`] asks whether it is
    /// still running, which is the only thing that notices a capture that
    /// stopped without an endpoint notification behind it.
    pipeline: Arc<Pipeline>,
    /// What it is registered with the mixer as, which is how it is
    /// deregistered again. The monitor mix knows it by
    /// [`monitor_registration`] of this.
    name: String,
    volume: AudioVolumeHandle,
    /// What this was opened with, so a snapshot naming something else is
    /// recognised as needing a reopen rather than a handle call.
    device: Option<String>,
    /// Whether it was wired into the monitor mix, for the same reason
    /// `device` is kept: a `Tee`'s branches are settled when it is built, so
    /// a change here is a reopen rather than a handle call.
    monitored: bool,
}

/// What plays the monitor mix, and what it was built against.
struct MonitorOutput {
    /// The branch off the monitor mix's `Tee` that carries it — resampled to
    /// the endpoint's own format, then rendered.
    branch: BranchId,
    /// The endpoint it was opened on, so a settings change is recognised.
    device: String,
    /// What the mix was running at when the resampler in this branch was
    /// built. The resampler is given the mix's time base at construction, so
    /// a rate change is a rebuild rather than a handle call.
    mix_format: MixFormat,
}

/// The mixer, and the sources feeding it.
///
/// # The mixer outlives its inputs
///
/// It is built once and never rebuilt, in a pipeline of its own that holds
/// nothing else. Its output timestamps are a count from when it started, so
/// restarting it restarts that count — which is invisible today, when nothing
/// consumes the mix, and would be a recording losing its audio timeline the
/// moment one does.
///
/// So a device change closes and reopens *that source's* pipeline and
/// re-registers it with the mixer through [`MixerHandle`], which is a runtime
/// registration for exactly this. The mixer never notices, and neither does
/// the source beside it: changing the microphone used to restart desktop
/// audio too, because both lived in the one pipeline a change had to rebuild.
pub(super) struct AudioEngine {
    /// `None` only if building it failed, which leaves the sources with
    /// nowhere to mix into and is reported once.
    mixer: Option<RunningMixer>,
    /// The mix that is played back.
    ///
    /// A second mixer and not a branch off the first, because it is a
    /// different sum: what is monitored is a *subset* of what is recorded,
    /// and a branch off the recording's mix would play everything in it —
    /// the desktop, and your own voice back at you. A subset needs its own
    /// sum however small the difference.
    ///
    /// It runs from startup like the other, whether or not a monitoring
    /// device is set: it is cheap to sum nothing, and a mixer built later
    /// would be one every source had to be re-registered with.
    monitor: Option<RunningMixer>,
    /// What is playing the monitor mix, or `None` when no monitoring device
    /// is set — which is the state every installation starts in.
    monitor_output: Option<MonitorOutput>,
    sources: HashMap<AudioSourceId, OpenAudioSource>,
    levels: Levels,
}

struct RunningMixer {
    _pipeline: Arc<Pipeline>,
    handle: MixerHandle,
    /// Where a recording's audio track attaches, and the reason the mixer
    /// fans out at all — see [`start_mixer`].
    tee: TeeHandle,
}

impl AudioEngine {
    /// Starts the mixer. It runs from here until this is dropped, whether or
    /// not anything is feeding it.
    pub(super) fn new(format: MixFormat) -> Self {
        let mixer = match start_mixer("audio-mix", "mix-tee", format) {
            Ok(mixer) => Some(mixer),
            Err(error) => {
                eprintln!("could not start the audio mixer: {error}");
                None
            }
        };
        // Its own failure, reported on its own: losing monitoring is not
        // losing the recording, and the two must not be able to take each
        // other down.
        let monitor = match start_mixer("audio-monitor", "monitor-tee", format) {
            Ok(mixer) => Some(mixer),
            Err(error) => {
                eprintln!("could not start the monitor mix: {error}");
                None
            }
        };
        Self {
            mixer,
            monitor,
            monitor_output: None,
            sources: HashMap::new(),
            levels: Levels::default(),
        }
    }

    pub(super) fn levels(&self) -> &Levels {
        &self.levels
    }

    /// Changes what the mix is summed into, from the mixer's next tick.
    ///
    /// A handle call, not a restart: the mixer keeps its own sample count and
    /// so its timeline, which is the reason it is alone in a pipeline of its
    /// own to begin with. Every input notices and rebuilds its own resampler.
    pub(super) fn set_mix_format(&mut self, format: MixFormat) {
        for (mixer, what) in [
            (self.mixer.as_ref(), "audio mixer"),
            (self.monitor.as_ref(), "monitor mix"),
        ] {
            let Some(mixer) = mixer else {
                continue;
            };
            if mixer.handle.mix_format() == Some(format) {
                continue;
            }
            if !mixer.handle.set_mix_format(format) {
                eprintln!(
                    "the {what} refused {}Hz, {} channel(s)",
                    format.sample_rate, format.channels
                );
            }
        }
        // The resampler in the monitor branch was given the mix's time base
        // when it was built, so it is the one thing here that cannot be told
        // about a rate change — it is rebuilt instead.
        if self
            .monitor_output
            .as_ref()
            .is_some_and(|output| output.mix_format != format)
        {
            let device = self
                .monitor_output
                .as_ref()
                .map(|output| output.device.clone());
            self.set_monitor_device(device.as_deref(), format);
        }
    }

    /// Points monitoring at an endpoint, or turns it off with `None`.
    ///
    /// `None` is the state an installation starts in and is not the same as
    /// "the system default": the default output is usually what Desktop Audio
    /// is already listening to, and monitoring into that is the feedback loop
    /// this setting exists to keep out. Choosing one is therefore something
    /// the user does deliberately.
    ///
    /// Every source's wiring depends on whether this succeeded, so the
    /// sources are left to the next `apply`, which compares wiring and
    /// reopens whatever changed.
    pub(super) fn set_monitor_device(&mut self, device: Option<&str>, format: MixFormat) {
        let unchanged = match (&self.monitor_output, device) {
            (Some(output), Some(device)) => output.device == device && output.mix_format == format,
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }

        if let Some(output) = self.monitor_output.take()
            && let Some(monitor) = &self.monitor
            && let Err(error) = monitor.tee.finish_branch(output.branch)
        {
            eprintln!("could not stop the previous monitoring device: {error}");
        }

        let Some(device) = device else {
            return;
        };
        let Some(monitor) = &self.monitor else {
            eprintln!("the monitor mix never started, so there is nothing to play");
            return;
        };
        match attach_monitor_output(&monitor.tee, device, format) {
            Ok(branch) => {
                self.monitor_output = Some(MonitorOutput {
                    branch,
                    device: device.to_owned(),
                    mix_format: format,
                });
            }
            // Reported and left off rather than retried: the endpoint named
            // is gone or refused, and every source stays wired to the
            // recording, which is the state `Wiring::for_mode` falls back to.
            Err(error) => eprintln!("could not open the monitoring device {device}: {error}"),
        }
    }

    /// Where a recording's audio track attaches, or `None` when the mixer
    /// never started — in which case a recording is written without one
    /// rather than refused.
    pub(super) fn mixer_access(&self) -> Option<(TeeHandle, MixerHandle)> {
        self.mixer
            .as_ref()
            .map(|mixer| (mixer.tee.clone(), mixer.handle.clone()))
    }

    /// The mix that is played back, for a Source that carries its own sound
    /// and is opened on the other thread.
    ///
    /// `None` while no monitoring endpoint is set as well as when the mixer
    /// never started: a handle to a mix nothing is playing would let a
    /// Source register with it and hear nothing. A Source asking to be
    /// monitored with nowhere to play is still recorded, which is what the
    /// caller decides from exactly this answer.
    pub(super) fn monitor_access(&self) -> Option<MixerHandle> {
        self.monitor_output.as_ref()?;
        self.monitor.as_ref().map(|mixer| mixer.handle.clone())
    }

    /// Brings the running sources in line with what the project holds and
    /// what the machine currently has plugged in.
    ///
    /// Gain and mute are handle calls on a source that is already open. A
    /// device change, or a source appearing or going, touches only that
    /// source. Called again on every endpoint change too, which is what
    /// opens a source whose device has just arrived — and closes one whose
    /// device has just left.
    /// Sets one open source's gain, for a fader being dragged.
    ///
    /// Nothing to do for a source with no capture behind it: there is no
    /// handle to set, and the next `apply` carries the value once the project
    /// has it.
    pub(super) fn set_gain_db(&mut self, id: AudioSourceId, gain_db: f32) {
        if let Some(open) = self.sources.get(&id) {
            let _ = open.volume.set_gain_db(gain_db);
        }
    }

    pub(super) fn apply(&mut self, snapshot: &AudioSnapshot, devices: &[AudioDeviceTarget]) {
        if self.mixer.is_none() {
            return;
        }

        // Gone from the project: close it and take its registration back.
        let wanted: Vec<AudioSourceId> = snapshot.items.iter().map(|source| source.id).collect();
        let dropped: Vec<AudioSourceId> = self
            .sources
            .keys()
            .copied()
            .filter(|id| !wanted.contains(id))
            .collect();
        for id in dropped {
            self.close(id);
        }

        for source in &snapshot.items {
            // Nothing to open it on. Closing rather than leaving it running
            // is what an unplugged microphone needs: the capture behind it
            // has lost its endpoint, and the mixer dock hides a source that
            // is not running — so one left in the map would show a channel
            // with a meter that can never move again.
            if !device::device_available(devices, source) {
                self.close(source.id);
                continue;
            }
            let wanted = monitors(source.monitored, self.monitor_output.is_some());
            match self.sources.get(&source.id) {
                // Already open on the endpoint asked for, and feeding the
                // mixes it should: the fader and the mute button are all that
                // can have changed.
                Some(open) if open.device == source.device && open.monitored == wanted => {
                    let _ = open.volume.set_gain_db(source.gain_db);
                    open.volume.set_muted(source.muted);
                }
                _ => self.reopen(source),
            }
        }
    }

    /// Closes every source whose capture has stopped by itself, and says
    /// whether it closed any.
    ///
    /// This is not the unplugged-microphone case. That raises a WASAPI
    /// endpoint notification, which wakes the worker and reaches `apply`,
    /// where `device_available` answers no and the channel is closed. What
    /// this is for is a capture that ends with no endpoint change behind it —
    /// a driver reset, the audio service restarting,
    /// `AUDCLNT_E_DEVICE_INVALIDATED` for a reason the endpoint list does not
    /// show. Nothing asked, so nothing noticed.
    ///
    /// The symptom is worth stating because it is the reason this is here at
    /// all: the source stays in the map, so the dock keeps drawing its
    /// channel, and the meter holds the last peak it read forever. For a
    /// microphone that was quiet when its capture died, that is
    /// indistinguishable from a working one — and in a recording it is a
    /// track silently lost until somebody plays the file back.
    ///
    /// The video engine has asked this of its own Sources all along (see
    /// `notice_dropped_streams`); this is the audio half of the same
    /// question.
    pub(super) fn close_ended(&mut self) -> bool {
        let ended: Vec<(AudioSourceId, String)> = self
            .sources
            .iter()
            .filter(|(_, open)| !open.pipeline.is_running())
            .map(|(id, open)| (*id, open.name.clone()))
            .collect();
        for (id, name) in &ended {
            eprintln!("the capture behind {name} stopped on its own");
            self.close(*id);
        }
        !ended.is_empty()
    }

    /// Stops one source and takes its mixer registration back, leaving the
    /// project's own entry alone — this is about what is running, not about
    /// what the user asked for.
    ///
    /// Removing its counter is what tells the dock the channel is gone, so
    /// every path that stops a source goes through here rather than dropping
    /// it out of the map directly.
    fn close(&mut self, id: AudioSourceId) {
        if let Some(open) = self.sources.remove(&id) {
            // Both, without asking which it was in: a registration it never
            // had is a name neither mixer knows, and taking one back that is
            // not there costs a lookup.
            if let Some(mixer) = &self.mixer {
                mixer.handle.remove_source(&open.name);
            }
            if let Some(monitor) = &self.monitor {
                monitor
                    .handle
                    .remove_source(&monitor_registration(&open.name));
            }
        }
        self.levels.forget(id);
    }

    /// Closes this source if it was open and opens it again on the endpoint
    /// the project now names.
    fn reopen(&mut self, source: &AudioSourceSnapshot) {
        let name = format!("audio-{}", source.id.0);
        // Closed before the new one opens: two captures of one endpoint is
        // something both backends allow and neither is what was asked for.
        self.close(source.id);
        let Some(mixer) = self.mixer.as_ref().map(|mixer| mixer.handle.clone()) else {
            return;
        };
        let monitor = self
            .monitor
            .as_ref()
            .map(|monitor| monitor.handle.clone())
            .filter(|_| self.monitor_output.is_some());
        let monitored = monitors(source.monitored, monitor.is_some());

        match open_source(&mixer, monitor.as_ref(), &name, source, monitored) {
            Ok((open, peak)) => {
                self.levels.track(source.id, peak);
                self.sources.insert(source.id, open);
            }
            Err(error) => {
                // One source that cannot open must not cost the others
                // theirs — a missing microphone is not a reason to lose
                // desktop audio. The mixer dock leaves its channel out until
                // it opens, which is what says so.
                eprintln!("could not open audio source {}: {error}", source.name);
                mixer.remove_source(&name);
                if let Some(monitor) = &monitor {
                    monitor.remove_source(&monitor_registration(&name));
                }
            }
        }
    }
}

/// The mixer, alone in a pipeline of its own, fanning out through a `Tee`.
///
/// Alone because it must outlive every source that comes and goes through it
/// — see [`AudioEngine`]. It emits whether or not anything is listening,
/// which is what a recording attached later needs.
///
/// The `Tee` is built with no branches on it at all, and `build_dynamic` so
/// that one can be added while it runs: a recording's audio track is opened
/// long after this, and the mixer must not be rebuilt to take it. It is the
/// same arrangement the compositor's own output `Tee` has, for the same
/// reason.
fn start_mixer(
    pipeline_name: &str,
    tee_name: &'static str,
    format: MixFormat,
) -> Result<RunningMixer, BackendError> {
    let (mixer, handle) = AudioMixer::new(
        format!("{pipeline_name}-mixer"),
        AudioMixerOptions {
            sample_rate: format.sample_rate,
            channels: format.channels,
        },
    );
    let mut tee = None;
    let pipeline = Pipeline::new(pipeline_name, mixer, |source, context| {
        let (tee_branch, tee_handle) =
            TeeBuilder::new(tee_name, context.clone()).build_dynamic()?;
        context.attach(source, 0, tee_branch)?;
        tee = Some(tee_handle);
        Ok(())
    })?;
    let tee = tee.expect("Pipeline::new runs the builder before returning");
    pipeline.run()?;
    Ok(RunningMixer {
        _pipeline: pipeline,
        handle,
        tee,
    })
}

/// Builds and attaches the branch that plays the monitor mix.
///
/// The resampler is not optional politeness: both renderers require the audio
/// to already be in the endpoint's own format and refuse anything else, since
/// neither converts. The mix runs at whatever the settings say, so something
/// has to.
fn attach_monitor_output(
    tee: &TeeHandle,
    device: &str,
    format: MixFormat,
) -> Result<BranchId, BackendError> {
    let (renderer, endpoint_format) = device::open_renderer("monitor-renderer", device)?;
    let resampler = AudioResampler::new(
        "monitor-resampler",
        endpoint_format,
        ffmpeg::Rational::new(1, format.sample_rate as i32),
    )?;
    let branch = tee
        .branch()
        .ok_or("the monitor mix's Tee is gone")?
        // A thread boundary, like the recording branch has, and for a
        // sharper version of the same reason: a render endpoint is paced by
        // the sound card, so writing to it from the mixer's own thread would
        // put the mix — and therefore the recording — behind the speakers.
        .queue_with_policy(
            "monitor-queue",
            MONITOR_QUEUE_DEPTH,
            OverflowPolicy::DropNewest,
        )
        .pipe(resampler)
        .to(Box::new(renderer))?;
    Ok(tee.attach(branch)?)
}

/// Opens one capture and wires it to the recording's mix, and to the monitor
/// mix when `monitored`, in a pipeline of its own.
///
/// Whether the monitor branch is there is settled here rather than adjusted
/// afterwards: a `Tee`'s branches are fixed when it is built, so a source
/// whose monitoring changed is reopened — the same cost, and for the same
/// reason, as one whose device changed.
fn open_source(
    mixer: &MixerHandle,
    monitor: Option<&MixerHandle>,
    name: &str,
    source: &AudioSourceSnapshot,
    monitored: bool,
) -> Result<(OpenAudioSource, Arc<AtomicU32>), BackendError> {
    let mixer_input = mixer.add_source(name).ok_or("the audio mixer is gone")?;
    let monitor_input = match (monitored, monitor) {
        (true, Some(monitor)) => Some(
            monitor
                .add_source(monitor_registration(name))
                .ok_or("the monitor mix is gone")?,
        ),
        _ => None,
    };
    let capture = device::open_capture(name, source.kind, source.device.as_deref())?;

    let (volume, volume_handle) = AudioVolume::new(format!("{name}-volume"));
    let _ = volume_handle.set_gain_db(source.gain_db);
    volume_handle.set_muted(source.muted);

    let peak = Arc::new(AtomicU32::new(0));
    let meter = AppSink::new(format!("{name}-meter"), {
        let peak = Arc::clone(&peak);
        move |buffer| {
            if let MediaBuffer::Audio(frame) = &buffer {
                peak.store(level::peak_db(frame).to_bits(), Ordering::Relaxed);
            }
            Ok(())
        }
    });

    let tee_name = format!("{name}-tee");
    let pipeline = Pipeline::new(name, capture, move |source_element, context| {
        // The `Tee` hangs off the *fader*, not the capture, so what every
        // branch carries is what the fader let through — a meter that
        // measures the level rather than the one before it, and monitoring
        // that goes quiet when the channel is pulled down.
        let meter_branch = context.branch().to(Box::new(meter))?;
        let mut tee = TeeBuilder::new(tee_name, context.clone())
            .branch(meter_branch)
            .branch(context.branch().to(mixer_input)?);
        if let Some(monitor_input) = monitor_input {
            tee = tee.branch(context.branch().to(monitor_input)?);
        }
        let tee = tee.build()?;
        // `to_branch` rather than `to`, because a `Tee` is a finished branch
        // rather than a `Sink` — it is what a chain ends *at*. Attaching it
        // to the fader's pad on its own would link the same buffers but
        // record the fan-out as the capture's; see `ChainBuilder::to_branch`.
        let faded = context.branch().pipe(volume).to_branch(tee)?;
        context.attach(source_element, 0, faded)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok((
        OpenAudioSource {
            pipeline,
            name: name.to_owned(),
            volume: volume_handle,
            device: source.device.clone(),
            monitored,
        },
        peak,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pipeline standing in for one source's capture, so the engine can be
    /// asked about a capture that stopped without opening a real device.
    ///
    /// `TestAudioSource` because it needs no endpoint and no file: what is
    /// under test is whether the engine looks, not what it was looking at.
    fn capture(name: &str, stopped: bool) -> Arc<Pipeline> {
        use media_pp::elements::{AppSink, TestAudioOptions, TestAudioSource};

        let source = TestAudioSource::new(name, TestAudioOptions::default());
        let sink = AppSink::new(format!("{name}-sink"), |_| Ok(()));
        let pipeline = Pipeline::new(name, source, move |source, context| {
            let branch = context.branch().to(Box::new(sink))?;
            context.attach(source, 0, branch)?;
            Ok(())
        })
        .expect("a synthetic source wires to an AppSink");
        pipeline.run().expect("the pipeline starts");
        if stopped {
            pipeline.stop();
            // `stop` asks; the source thread is what answers. Bounded rather
            // than assumed, so a slow machine fails the wait instead of the
            // assertion it would otherwise reach early.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while pipeline.is_running() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(!pipeline.is_running(), "the stopped pipeline must end");
        }
        pipeline
    }

    /// One entry in the engine's map, built directly: `open_source` opens a
    /// real endpoint, and this test is about what happens after one dies.
    fn open(name: &str, pipeline: Arc<Pipeline>) -> OpenAudioSource {
        let (_volume, handle) = AudioVolume::new(format!("{name}-volume"));
        OpenAudioSource {
            pipeline,
            name: name.to_owned(),
            volume: handle,
            device: None,
            monitored: false,
        }
    }

    /// An engine with no mixer behind it, which every path here tolerates —
    /// `close` removes registrations from whichever mixes exist.
    fn engine(sources: Vec<(AudioSourceId, OpenAudioSource)>) -> AudioEngine {
        let mut levels = Levels::default();
        for (id, _) in &sources {
            levels.track(*id, Arc::new(AtomicU32::new(0)));
        }
        AudioEngine {
            mixer: None,
            monitor: None,
            monitor_output: None,
            sources: sources.into_iter().collect(),
            levels,
        }
    }

    /// The failure this exists to prevent: a capture that stopped by itself
    /// used to stay in the map, so the dock kept drawing its channel and the
    /// meter held its last peak forever. A quiet microphone that died looked
    /// exactly like a quiet microphone that worked.
    #[test]
    fn a_capture_that_stopped_on_its_own_loses_its_channel() {
        let dead = AudioSourceId(1);
        let mut engine = engine(vec![(dead, open("audio-1", capture("audio-1", true)))]);

        assert!(engine.close_ended(), "the stopped capture must be noticed");
        assert!(
            !engine.sources.contains_key(&dead),
            "and closed, so the dock stops drawing a channel nothing feeds"
        );
        assert!(
            !engine.levels.is_running(dead),
            "its meter must be forgotten rather than frozen at the last peak"
        );
    }

    /// The other half, and the one that would break the microphone if it were
    /// wrong: a running capture must be left alone. Closing one every tick
    /// would reopen the endpoint once a second forever.
    #[test]
    fn a_running_capture_is_left_where_it_is() {
        let live = AudioSourceId(2);
        let mut engine = engine(vec![(live, open("audio-2", capture("audio-2", false)))]);

        assert!(
            !engine.close_ended(),
            "nothing has stopped, so nothing is closed"
        );
        assert!(engine.sources.contains_key(&live));
        assert!(
            engine.levels.is_running(live),
            "and its meter keeps being reported"
        );
    }

    /// One capture dying must not cost the others theirs — the same rule
    /// `reopen` follows when an endpoint refuses to open.
    #[test]
    fn one_dead_capture_does_not_take_the_others_with_it() {
        let dead = AudioSourceId(3);
        let live = AudioSourceId(4);
        let mut engine = engine(vec![
            (dead, open("audio-3", capture("audio-3", true))),
            (live, open("audio-4", capture("audio-4", false))),
        ]);

        assert!(engine.close_ended());
        assert!(!engine.sources.contains_key(&dead));
        assert!(
            engine.sources.contains_key(&live),
            "the surviving capture keeps its channel"
        );
    }

    /// The rule the two halves share: asking to be monitored where there is
    /// nothing to play through is not monitoring.
    ///
    /// The failure this is here to prevent used to be worse than a silent
    /// channel — a third state promised a source would be kept out of the
    /// recording, and that promise was only true on a machine monitoring to
    /// an endpoint nothing captures. There are two states now, and the
    /// recording always takes the source.
    #[test]
    fn nothing_is_monitored_where_there_is_nowhere_to_play() {
        use super::monitors;

        assert!(monitors(true, true));
        assert!(!monitors(true, false));
        assert!(!monitors(false, true));
        assert!(!monitors(false, false));
    }
}
