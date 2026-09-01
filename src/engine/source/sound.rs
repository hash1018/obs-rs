//! The sound branch a Source carries with its own picture.
//!
//! Two Sources have one: a media file and a live stream. What they carry is
//! the same shape — decode, hold, pace, fade, then split between the mixer
//! and a meter — and the two differ only in what is upstream of it, which is
//! why this is a module rather than a copy in each.
//!
//! ```text
//! packets ─ SwDecoder ─ Queue ─ Pacer ─ AudioVolume ─ Tee ┬ AudioMixer
//!                                                         └ AppSink (meter)
//! ```
//!
//! The `Tee` hangs off the *fader*, so the meter shows what the fader let
//! through rather than what arrived at it: pulling one down empties its
//! meter, and so does muting.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use media_pp::element::{Context, Sink, Source as SourceElement};
use media_pp::elements::{
    AppSink, AudioVolume, AudioVolumeHandle, MixerHandle, Pacer, SwDecoder, TeeBuilder,
};
use media_pp::ffmpeg;

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
    fader: AudioVolume,
    /// What the Audio Mixer dock moves, and what it reads.
    pub(in crate::engine) volume: AudioVolumeHandle,
    /// The mixer input this Source's sound is summed into.
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

/// Builds it, or answers `None` for a Source with no sound and for a machine
/// whose mixer never started — the picture is worth showing either way.
pub(in crate::engine) fn build(
    name: &str,
    track: Option<Track>,
    mixer: Option<&MixerHandle>,
    gain_db: f32,
    muted: bool,
    meters: &Arc<MediaMeters>,
) -> Result<Option<Sound>, BackendError> {
    let (Some(track), Some(mixer)) = (track, mixer) else {
        return Ok(None);
    };
    let decoder = SwDecoder::new(format!("{name}-audio-decoder"), track.params)?;

    // The fader lives in this pipeline rather than the audio thread's,
    // because this sound belongs to this Source rather than to a device
    // everything shares.
    let (fader, volume) = AudioVolume::new(format!("{name}-volume"));
    let _ = volume.set_gain_db(gain_db);
    volume.set_muted(muted);

    let meter = AppSink::new(format!("{name}-meter"), {
        let meters = Arc::clone(meters);
        move |buffer| {
            if let media_pp::buffer::MediaBuffer::Audio(frame) = &buffer {
                meters.peak.store(
                    crate::engine::audio::peak_db(frame).to_bits(),
                    Ordering::Relaxed,
                );
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
) -> media_pp::error::Result<()> {
    let meter = context.branch().to(sound.meter)?;
    let mix = context.branch().to(sound.mix)?;
    let tee = TeeBuilder::new("audio-tee", context.clone())
        .branch(meter)
        .branch(mix)
        .build()?;
    let faded = context
        .branch()
        .pipe(sound.decoder)
        .queue("audio", QUEUE_DEPTH)
        .pipe(match sound.discontinuity_limit {
            Some(limit) => Pacer::with_discontinuity_limit("audio-pacer", sound.time_base, limit)?,
            None => Pacer::new("audio-pacer", sound.time_base)?,
        })
        .pipe(sound.fader)
        .to_branch(tee)?;
    context.attach(source, sound.index, faded)?;
    Ok(())
}
