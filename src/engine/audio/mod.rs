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
use crate::domain::{AudioSourceId, MonitorMode};
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

/// Which mixes one source is fed into.
///
/// Derived from [`MonitorMode`] rather than being it, because the mode is
/// what the user asked for and this is what the machine can actually do
/// about it — see [`Wiring::for_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Wiring {
    /// Into the mix a recording is written from.
    to_mix: bool,
    /// Into the mix that is played back.
    to_monitor: bool,
}

impl Wiring {
    fn for_mode(mode: MonitorMode, monitoring: bool) -> Self {
        if !monitoring {
            // No endpoint to play anything to. A source asking to be
            // monitored still has to reach the recording, or picking a mode
            // while no monitoring device is set would quietly drop it from
            // the file — a setting in one dialog silencing a channel in
            // another, with nothing on screen to connect the two.
            return Self {
                to_mix: true,
                to_monitor: false,
            };
        }
        Self {
            to_mix: mode.reaches_output(),
            to_monitor: mode.is_monitored(),
        }
    }
}

/// One source that is open, in its own pipeline, and how it was opened.
struct OpenAudioSource {
    /// This source's capture, tee and fader. Dropping it stops that one
    /// capture and nothing else.
    _pipeline: Arc<Pipeline>,
    /// What it is registered with the mixer as, which is how it is
    /// deregistered again. The monitor mix knows it by
    /// [`monitor_registration`] of this.
    name: String,
    volume: AudioVolumeHandle,
    /// What this was opened with, so a snapshot naming something else is
    /// recognised as needing a reopen rather than a handle call.
    device: Option<String>,
    /// Which mixes it was wired into, for the same reason `device` is kept:
    /// a `Tee`'s branches are settled when it is built, so a change here is
    /// a reopen rather than a handle call.
    wiring: Wiring,
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
    /// The mix that is played back rather than recorded.
    ///
    /// A second mixer and not a second branch off the first, because the two
    /// sums are genuinely different: `MonitorOnly` is a source in this one
    /// and not in that one. It runs from startup like the other, whether or
    /// not a monitoring device is set — it is cheap to sum nothing, and a
    /// mixer built later would be one every source had to be re-registered
    /// with.
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
            let wanted = Wiring::for_mode(source.monitor, self.monitor_output.is_some());
            match self.sources.get(&source.id) {
                // Already open on the endpoint asked for, and feeding the
                // mixes it should: the fader and the mute button are all that
                // can have changed.
                Some(open) if open.device == source.device && open.wiring == wanted => {
                    let _ = open.volume.set_gain_db(source.gain_db);
                    open.volume.set_muted(source.muted);
                }
                _ => self.reopen(source),
            }
        }
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
        let wiring = Wiring::for_mode(source.monitor, monitor.is_some());

        match open_source(&mixer, monitor.as_ref(), &name, source, wiring) {
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

/// Opens one capture and wires it to the mixes `wiring` names, in a pipeline
/// of its own.
///
/// Which mixes those are is settled here rather than adjusted afterwards: a
/// `Tee`'s branches are fixed when it is built, so a source whose monitoring
/// changed is reopened — the same cost, and for the same reason, as one whose
/// device changed.
fn open_source(
    mixer: &MixerHandle,
    monitor: Option<&MixerHandle>,
    name: &str,
    source: &AudioSourceSnapshot,
    wiring: Wiring,
) -> Result<(OpenAudioSource, Arc<AtomicU32>), BackendError> {
    let mixer_input = match wiring.to_mix {
        true => Some(mixer.add_source(name).ok_or("the audio mixer is gone")?),
        false => None,
    };
    let monitor_input = match (wiring.to_monitor, monitor) {
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
        let mut tee = TeeBuilder::new(tee_name, context.clone()).branch(meter_branch);
        if let Some(mixer_input) = mixer_input {
            tee = tee.branch(context.branch().to(mixer_input)?);
        }
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
            _pipeline: pipeline,
            name: name.to_owned(),
            volume: volume_handle,
            device: source.device.clone(),
            wiring,
        },
        peak,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three modes decide two independent things, and this is where they
    /// are turned into wiring: `MonitorOnly` is the one that leaves the
    /// recording, and it is the whole reason there are three.
    #[test]
    fn each_mode_names_the_mixes_it_belongs_in() {
        assert_eq!(
            Wiring::for_mode(MonitorMode::Off, true),
            Wiring {
                to_mix: true,
                to_monitor: false
            }
        );
        assert_eq!(
            Wiring::for_mode(MonitorMode::MonitorOnly, true),
            Wiring {
                to_mix: false,
                to_monitor: true
            }
        );
        assert_eq!(
            Wiring::for_mode(MonitorMode::MonitorAndOutput, true),
            Wiring {
                to_mix: true,
                to_monitor: true
            }
        );
    }

    /// With nowhere to play it, a source asking to be monitored is still
    /// recorded. The failure this is here to prevent is silent and expensive:
    /// a mode chosen in the mixer dock while no device is set in Settings,
    /// and a channel missing from the file with nothing on screen to say
    /// which of the two did it.
    #[test]
    fn a_mode_that_cannot_be_played_still_reaches_the_recording() {
        for mode in [
            MonitorMode::Off,
            MonitorMode::MonitorOnly,
            MonitorMode::MonitorAndOutput,
        ] {
            assert_eq!(
                Wiring::for_mode(mode, false),
                Wiring {
                    to_mix: true,
                    to_monitor: false
                },
                "{mode:?} with no monitoring device"
            );
        }
    }
}
