//! What the mixer dock's meters read.
//!
//! One number per source, published where the UI thread can take it without
//! waiting on the audio thread — a meter that is one frame stale is a meter,
//! and one that blocks the graph to be current is not.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::domain::AudioSourceId;

/// The quietest a meter shows, matching the mixer dock's own scale.
const METER_FLOOR_DB: f32 = -60.0;

/// One source's most recent peak, in `f32` bits so the UI can read it without
/// a lock.
///
/// Written by whichever thread the capture's own `Tee` pushes on, read by the
/// UI thread. `Relaxed` because a meter that is one frame stale is a meter
/// that is correct a frame later, and nothing else is ordered against it.
/// Cloning shares the counters rather than copying values: the map is a
/// handful of `Arc`s, and the UI reads the same atomics the captures write.
#[derive(Default, Clone)]
pub(in crate::engine) struct Levels {
    peaks: HashMap<AudioSourceId, Arc<AtomicU32>>,
}

impl Levels {
    /// Starts reporting this source, sharing the counter its capture writes.
    pub(super) fn track(&mut self, id: AudioSourceId, peak: Arc<AtomicU32>) {
        self.peaks.insert(id, peak);
    }

    /// Stops reporting it, which is what a closed source leaves behind — a
    /// meter that kept answering for a capture that is gone would show the
    /// last level it ever had, for as long as anyone looked.
    pub(super) fn forget(&mut self, id: AudioSourceId) {
        self.peaks.remove(&id);
    }

    /// The peak of the last buffer this source produced, or `None` when it
    /// has produced none — which is what a source that failed to open, or has
    /// not been given a device, looks like.
    pub(in crate::engine) fn peak_db(&self, id: AudioSourceId) -> Option<f32> {
        let bits = self.peaks.get(&id)?.load(Ordering::Relaxed);
        (bits != 0).then(|| f32::from_bits(bits))
    }

    /// Whether this source has a capture running behind it.
    ///
    /// Its counter exists exactly while it does: `AudioEngine` inserts one
    /// when a source opens and removes it when the source closes, so asking
    /// whether the counter is here is asking whether the capture is.
    pub(in crate::engine) fn is_running(&self, id: AudioSourceId) -> bool {
        self.peaks.contains_key(&id)
    }
}

/// The loudest sample in this buffer, in decibels below full scale, floored
/// where the mixer's scale ends.
///
/// Peak rather than RMS: a meter is watched to catch a clip, and an average
/// is exactly what hides one.
///
/// Not clamped at the top. A fader that boosts can push a source past full
/// scale, and clamping here would hand the dock a `0.0` for both "reached
/// full scale" and "is 6 dB over it" — the second of which is the clip this
/// function exists to catch. The floor stays, because below it there is
/// nothing to tell apart.
pub(super) fn peak_db(frame: &media_pp::ffmpeg::frame::Audio) -> f32 {
    use media_pp::ffmpeg::format::Sample;

    let peak = match frame.format() {
        Sample::F32(_) => frame
            .plane::<f32>(0)
            .iter()
            .fold(0.0f32, |loudest, sample| loudest.max(sample.abs())),
        Sample::I16(_) => frame
            .plane::<i16>(0)
            .iter()
            .fold(0.0f32, |loudest, sample| {
                loudest.max(f32::from(*sample).abs() / f32::from(i16::MAX))
            }),
        // Anything else is not read rather than read wrongly: a meter that
        // shows a plausible number for a format it guessed at is worse than
        // one that shows nothing.
        _ => return METER_FLOOR_DB,
    };
    if peak <= 0.0 {
        return METER_FLOOR_DB;
    }
    (20.0 * peak.log10()).max(METER_FLOOR_DB)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one thing a boosting fader made possible, and the one thing the
    /// old clamp threw away: a level past full scale has to arrive as a
    /// number greater than zero, or nothing downstream can tell a clip from a
    /// take that merely touched the ceiling.
    #[test]
    fn a_level_past_full_scale_is_reported_rather_than_flattened() {
        use media_pp::ffmpeg;

        let frame = |loudest: f32| {
            let mut frame = ffmpeg::frame::Audio::new(
                ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
                4,
                ffmpeg::ChannelLayout::default(1),
            );
            frame.set_rate(48_000);
            frame.plane_mut::<f32>(0).fill(loudest);
            frame
        };

        // Twice full scale is +6 dB, and that is what has to come back.
        let over = peak_db(&frame(2.0));
        assert!(
            (over - 6.0206).abs() < 0.01,
            "a sample at twice full scale must read about +6 dB, got {over}"
        );

        assert!((peak_db(&frame(1.0))).abs() < 0.001, "full scale is 0 dB");
        assert!(peak_db(&frame(0.5)) < 0.0, "and anything under it is below");
        assert_eq!(
            peak_db(&frame(0.0)),
            METER_FLOOR_DB,
            "the floor is still a floor"
        );
    }
}
