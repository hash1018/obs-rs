//! The sound branch a Source carries with its own picture.
//!
//! Two Sources have one: a media file and a live stream. What they carry is
//! the same shape — decode, hold, pace, fade, then split between the mixer
//! and a meter — and the two differ only in what is upstream of it, which is
//! why this is a module rather than a copy in each.
//!
//! ```text
//! packets ─ SwDecoder ─ Queue ─ Pacer ─ Rack ─ AudioVolume ─ Tee ┬ AppSink (meter)
//!                                                                ├ AudioMixer (recording)
//!                                                                └ AudioMixer (monitor)
//! ```
//!
//! The `Tee` hangs off the *fader*, so the meter shows what the fader let
//! through rather than what arrived at it: pulling one down empties its
//! meter, and so does muting.
//!
//! # The filters sit after the `Pacer`
//!
//! The `Rack` holds the Source's audio filters, before the fader as a mixer
//! channel's are. After the `Pacer` rather than before it, for two reasons.
//! A bridge in the rack — the resampler it takes on for a filter that needs
//! another format — counts the `pts` it puts out in samples, and the `Pacer`
//! is told the stream's own time base; ahead of it, one would have it
//! waiting on the wrong clock. And after it, a slider moved while a clip
//! plays is heard a frame later rather than a queue's depth later. Nothing
//! downstream reads `pts` again: the mixer counts its own samples.
//!
//! # The monitor branch comes and goes while it plays
//!
//! The meter and the recording's mix are fixed; the monitor's is not.
//! Whether a Source is monitored can be changed with a click while a clip is
//! halfway through, so the `Tee` is dynamic and [`SoundRouting`] puts that
//! one branch on and takes it off — rather than the Source being reopened
//! the way a device channel is. A device channel can afford the reopen; a
//! file would restart from the beginning, which is a much larger thing to do
//! to somebody who only asked to hear it.

use std::sync::Arc;
use std::time::Duration;

use media_pp::element::{Context, Sink, Source as SourceElement};
use media_pp::elements::{
    AppSink, AudioFormat, AudioVolume, AudioVolumeHandle, MixerHandle, Pacer, Rack, SwDecoder,
    TeeBuilder, TeeHandle,
};
use media_pp::ffmpeg;
use media_pp::graph::BranchId;

use crate::domain::{AudioFilter, AudioFilterId, AudioFilterSettings};
use crate::engine::audio::filters::{self, AudioFilterRack};
use crate::engine::audio::{Meter, MeterWake};
use crate::engine::backend::BackendError;
use crate::engine::source::MediaMeters;

/// Decoded audio frames kept ahead of the `Pacer`, at 1024 samples each —
/// about a second and a half.
///
/// A file's read-ahead is only as deep as its shallowest branch, since one
/// cursor serves both streams: whichever queue fills first blocks it for
/// both. Audio frames are small and hold no decoder surface, so this is the
/// branch that can afford to be the deep one.
pub(in crate::engine) const QUEUE_DEPTH: usize = 64;

/// One stream's index and what its branch is built from.
pub(in crate::engine) struct Track {
    pub(in crate::engine) index: usize,
    pub(in crate::engine) params: ffmpeg::codec::Parameters,
    pub(in crate::engine) time_base: ffmpeg::Rational,
}

/// The branch, built before the pipeline is.
///
/// Everything here can fail for an ordinary reason — a codec this FFmpeg was
/// not built with, a mixer that has gone — so it is built where an error can
/// be reported rather than unwrapped inside the builder closure, on a thread
/// with nowhere to report from.
pub(in crate::engine) struct Sound {
    index: usize,
    time_base: ffmpeg::Rational,
    decoder: SwDecoder,
    /// Where the Source's audio filters go, and the way back to it.
    rack: Rack,
    filters: AudioFilterRack,
    /// What goes in it before the first buffer, so a clip starts filtered.
    initial_filters: Vec<AudioFilter>,
    fader: AudioVolume,
    /// What the Audio Mixer dock moves, and what it reads.
    pub(in crate::engine) volume: AudioVolumeHandle,
    /// The mixer input this Source's sound is summed into for the recording.
    mix: Box<dyn Sink>,
    /// The `AppSink` that measures the level.
    meter: Box<dyn Sink>,
    /// Set for a live sender, whose timeline can restart under it — see
    /// [`Sound::with_discontinuity_limit`].
    discontinuity_limit: Option<Duration>,
}

impl Sound {
    /// Paces this sound like the live stream it is: a timestamp further
    /// ahead than `limit` is a timeline that restarted rather than a gap to
    /// wait out.
    ///
    /// The picture's own `Pacer` has to be given the same limit. A jump that
    /// re-anchored one branch and not the other would leave the sound
    /// playing against an origin the picture no longer shares.
    pub(in crate::engine) fn with_discontinuity_limit(mut self, limit: Duration) -> Self {
        self.discontinuity_limit = Some(limit);
        self
    }
}

/// What a Source registers its audio with the mixer as.
///
/// Distinct from the compositor's input name even though the two registries
/// could not collide, so a log naming one is never ambiguous about which.
pub(in crate::engine) fn mixer_name(name: &str) -> String {
    format!("{name}-audio")
}

/// And what the monitor mix knows it by.
///
/// A name of its own, though the two registries are separate and the same
/// string would work in both. It is for the log: a Source in both mixes
/// draws two branches into two elements, and one name between them leaves a
/// topology diagram with no way to say which is which.
///
/// Built on [`mixer_name`] rather than beside it, because the `-audio` that
/// one adds is not "the recording" — it is this Source's sound as opposed to
/// its picture, which is a distinction the monitor's copy shares.
fn monitor_name(name: &str) -> String {
    format!("{}-monitor", mixer_name(name))
}

/// Whether a running Source's sound is in the monitor mix, and the `Tee` that
/// decides it.
///
/// Held by the engine loop rather than by the pipeline, because the answer is
/// the project's and can change without the Source restarting.
pub(in crate::engine) struct SoundRouting {
    tee: TeeHandle,
    /// The base name the monitor registration is derived from.
    name: String,
    monitored: bool,
    branch: Option<BranchId>,
    /// The Source's audio filters, which change while it plays for the
    /// reason the monitor branch does: reopening a file restarts it.
    filters: AudioFilterRack,
}

impl SoundRouting {
    /// Brings the filters in line with what the project holds. One that
    /// cannot be built costs the filters and not the sound, as on a mixer
    /// channel.
    pub(in crate::engine) fn apply_filters(&mut self, filters: &[AudioFilter]) {
        if let Err(error) = self.filters.apply(filters) {
            eprintln!("could not put the audio filters on {}: {error}", self.name);
        }
    }

    /// One filter's settings while its slider is still held — see
    /// [`AudioFilterRack::retune`].
    pub(in crate::engine) fn retune_filter(
        &self,
        id: AudioFilterId,
        settings: &AudioFilterSettings,
    ) {
        self.filters.retune(id, settings);
    }

    /// Puts the monitor branch on or takes it off.
    ///
    /// The recording's branch is not here at all: it is on the `Tee` from the
    /// moment it is built and stays there, because a Source is recorded
    /// whether or not it is being listened to.
    pub(in crate::engine) fn apply(&mut self, monitored: bool, monitor: Option<&MixerHandle>) {
        if self.monitored == monitored {
            return;
        }
        let name = monitor_name(&self.name);
        self.monitored = match monitored {
            false => {
                if let Some(branch) = self.branch.take() {
                    // Detached first, then deregistered: the other way round
                    // leaves a branch pushing into a mixer input that has
                    // been taken back.
                    if let Err(error) = self.tee.detach(branch) {
                        eprintln!("could not take {name} off the monitor mix: {error}");
                    }
                    if let Some(monitor) = monitor {
                        monitor.remove_source(&name);
                    }
                }
                false
            }
            true => self.attach_monitor(monitor, &name),
        };
    }

    fn attach_monitor(&mut self, monitor: Option<&MixerHandle>, name: &str) -> bool {
        let Some(monitor) = monitor else {
            return false;
        };
        let Some(input) = monitor.add_source(name) else {
            eprintln!("could not register {name} with the monitor mix: it is gone");
            return false;
        };
        let attached = self
            .tee
            .branch()
            .ok_or_else(|| "the Source's Tee is gone".to_owned())
            .and_then(|branch| branch.to(input).map_err(|error| error.to_string()))
            .and_then(|branch| self.tee.attach(branch).map_err(|error| error.to_string()));
        match attached {
            Ok(branch) => {
                self.branch = Some(branch);
                true
            }
            Err(error) => {
                eprintln!("could not put {name} on the monitor mix: {error}");
                monitor.remove_source(name);
                false
            }
        }
    }
}

/// What the project says a Source's sound should be when it opens: where its
/// fader is, and what filters it goes through first.
pub(in crate::engine) struct SoundSettings<'a> {
    pub(in crate::engine) gain_db: f32,
    pub(in crate::engine) muted: bool,
    pub(in crate::engine) filters: &'a [AudioFilter],
}

/// Builds it, or answers `None` for a Source with no sound and for a machine
/// whose mixer never started — the picture is worth showing either way.
pub(in crate::engine) fn build(
    name: &str,
    track: Option<Track>,
    mixer: Option<&MixerHandle>,
    settings: SoundSettings<'_>,
    meters: &Arc<MediaMeters>,
    meter_wake: &MeterWake,
) -> Result<Option<Sound>, BackendError> {
    let SoundSettings {
        gain_db,
        muted,
        filters: audio_filters,
    } = settings;
    let (Some(track), Some(mixer)) = (track, mixer) else {
        return Ok(None);
    };
    // Read before the parameters are moved into the decoder.
    let (rack, filters) = filters::rack(
        &mixer_name(name),
        declared_format(&track.params),
        track.time_base,
    );
    let decoder = SwDecoder::new(format!("{name}-audio-decoder"), track.params)?;

    // The fader lives in this pipeline rather than the audio thread's,
    // because this sound belongs to this Source rather than to a device
    // everything shares.
    let (fader, volume) = AudioVolume::new(format!("{name}-volume"));
    let _ = volume.set_gain_db(gain_db);
    volume.set_muted(muted);

    let meter = AppSink::new(format!("{name}-meter"), {
        let meters = Arc::clone(meters);
        let mut meter = Meter::new(meter_wake.clone());
        move |buffer| {
            if let media_pp::buffer::MediaBuffer::Audio(frame) = &buffer {
                meter.measure(frame, &meters.peak);
            }
            Ok(())
        }
    });

    let mix = mixer
        .add_source(mixer_name(name))
        .ok_or("the audio mixer is gone")?;
    Ok(Some(Sound {
        index: track.index,
        time_base: track.time_base,
        decoder,
        rack,
        filters,
        initial_filters: audio_filters.to_vec(),
        fader,
        volume,
        mix,
        meter: Box::new(meter),
        discontinuity_limit: None,
    }))
}

/// Attaches it to whichever pad the source announced this stream on.
///
/// `to_branch` rather than `to`, because a `Tee` is a finished branch rather
/// than a `Sink`: attaching it to the fader's pad on its own would link the
/// same buffers but record the fan-out as the source's.
pub(in crate::engine) fn attach<S: SourceElement>(
    context: &Arc<Context>,
    source: &mut S,
    sound: Sound,
    name: &str,
) -> media_pp::error::Result<SoundRouting> {
    let meter = context.branch().to(sound.meter)?;
    let mix = context.branch().to(sound.mix)?;
    // Dynamic although both of these are permanent, so that the monitor's
    // branch can be put on and taken off while the Source plays — see
    // [`SoundRouting`].
    let (tee_branch, tee) = TeeBuilder::new("audio-tee", context.clone())
        .branch(meter)
        .branch(mix)
        .build_dynamic()?;
    let faded = context
        .branch()
        .pipe(sound.decoder)
        .queue("audio", QUEUE_DEPTH)
        .pipe(match sound.discontinuity_limit {
            Some(limit) => Pacer::with_discontinuity_limit("audio-pacer", sound.time_base, limit)?,
            None => Pacer::new("audio-pacer", sound.time_base)?,
        })
        .pipe(sound.rack)
        .pipe(sound.fader)
        .to_branch(tee_branch)?;
    context.attach(source, sound.index, faded)?;
    let mut routing = SoundRouting {
        tee,
        name: name.to_owned(),
        // Not yet. The first reconcile after this puts the branch on if the
        // project asks for it, which is also what puts it back after a
        // change.
        monitored: false,
        branch: None,
        filters: sound.filters,
    };
    // Filled now, before the pipeline runs, unlike the monitor branch: a
    // reconcile would put them in too, but a clip's first moments would
    // have played unfiltered by then.
    routing.apply_filters(&sound.initial_filters);
    Ok(routing)
}

/// What the decoder will hand the rack, as the stream declares it — what a
/// bridge is decided from.
///
/// A declaration rather than a measurement, so what it cannot say is filled
/// in with what makes the rack convert rather than trust it: a bridge takes
/// whatever actually arrives, whatever it was told to expect.
fn declared_format(params: &ffmpeg::codec::Parameters) -> AudioFormat {
    let declared = ffmpeg::codec::context::Context::from_parameters(params.clone())
        .ok()
        .and_then(|context| context.decoder().audio().ok())
        .map(|audio| (audio.format(), audio.rate(), audio.channels()));
    let (format, rate, channels) = declared.unwrap_or((ffmpeg::format::Sample::None, 0, 0));
    AudioFormat::new(
        format,
        if rate == 0 { 48_000 } else { rate },
        if channels == 0 { 2 } else { channels },
    )
}
