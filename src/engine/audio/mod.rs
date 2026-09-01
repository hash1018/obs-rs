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
        AppSink, AudioMixer, AudioMixerOptions, AudioVolume, AudioVolumeHandle, MixFormat,
        MixerHandle, TeeBuilder, TeeHandle,
    },
    pipeline::Pipeline,
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

/// One source that is open, in its own pipeline, and how it was opened.
struct OpenAudioSource {
    /// This source's capture, tee and fader. Dropping it stops that one
    /// capture and nothing else.
    _pipeline: Arc<Pipeline>,
    /// What it is registered with the mixer as, which is how it is
    /// deregistered again.
    name: String,
    volume: AudioVolumeHandle,
    /// What this was opened with, so a snapshot naming something else is
    /// recognised as needing a reopen rather than a handle call.
    device: Option<String>,
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
        let mixer = match start_mixer(format) {
            Ok(mixer) => Some(mixer),
            Err(error) => {
                eprintln!("could not start the audio mixer: {error}");
                None
            }
        };
        Self {
            mixer,
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
    pub(super) fn set_mix_format(&self, format: MixFormat) {
        let Some(mixer) = &self.mixer else {
            return;
        };
        if mixer.handle.mix_format() == Some(format) {
            return;
        }
        if !mixer.handle.set_mix_format(format) {
            eprintln!(
                "the audio mixer refused {}Hz, {} channel(s)",
                format.sample_rate, format.channels
            );
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
            match self.sources.get(&source.id) {
                // Already open on the endpoint asked for: the fader and the
                // mute button are all that can have changed.
                Some(open) if open.device == source.device => {
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
        let Some(mixer) = &self.mixer else {
            return;
        };
        if let Some(open) = self.sources.remove(&id) {
            mixer.handle.remove_source(&open.name);
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
        let Some(mixer) = &self.mixer else {
            return;
        };

        match open_source(&mixer.handle, &name, source) {
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
                mixer.handle.remove_source(&name);
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
fn start_mixer(format: MixFormat) -> Result<RunningMixer, BackendError> {
    let (mixer, handle) = AudioMixer::new(
        "audio-mixer",
        AudioMixerOptions {
            sample_rate: format.sample_rate,
            channels: format.channels,
        },
    );
    let mut tee = None;
    let pipeline = Pipeline::new("audio-mix", mixer, |source, context| {
        let (tee_branch, tee_handle) =
            TeeBuilder::new("mix-tee", context.clone()).build_dynamic()?;
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

/// Opens one capture and wires it to the mixer, in a pipeline of its own.
fn open_source(
    mixer: &MixerHandle,
    name: &str,
    source: &AudioSourceSnapshot,
) -> Result<(OpenAudioSource, Arc<AtomicU32>), BackendError> {
    let mixer_input = mixer.add_source(name).ok_or("the audio mixer is gone")?;
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
        // The `Tee` hangs off the *fader*, not the capture, so what both
        // branches carry is what the fader let through — a meter that
        // measures the level rather than the one before it.
        let meter_branch = context.branch().to(Box::new(meter))?;
        let mix_branch = context.branch().to(mixer_input)?;
        let tee = TeeBuilder::new(tee_name, context.clone())
            .branch(meter_branch)
            .branch(mix_branch)
            .build()?;
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
        },
        peak,
    ))
}
