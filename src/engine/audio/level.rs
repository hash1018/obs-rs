//! What the mixer dock's meters read, and what makes the dock look.
//!
//! One number per source, published where the UI thread can take it without
//! waiting on the audio thread — a meter that is one frame stale is a meter,
//! and one that blocks the graph to be current is not.
//!
//! Publishing a number is not the same as it being seen, though. The UI draws
//! only when something asks it to, and until [`MeterWake`] nothing on the
//! audio side did: a meter moved when the Preview changed, or the pointer
//! did, or the status bar's once-a-second sample came round. Over a Scene
//! with nothing moving in it, that was a meter updating once a second.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::domain::AudioSourceId;

/// The quietest a meter shows, matching the mixer dock's own scale.
const METER_FLOOR_DB: f32 = -60.0;

/// How fast a meter falls once what it measured has passed.
///
/// A meter that dropped straight to each buffer's peak would flicker between
/// syllables faster than anyone can read it; one that fell too slowly would
/// still be showing a word after the next one had started. At this rate the
/// whole scale empties in three seconds, and a single word leaves a trace
/// long enough to see.
const DECAY_DB_PER_SECOND: f32 = 20.0;

/// The most often a meter asks to be drawn.
///
/// Twenty a second, below the Preview's thirty, because what it buys is
/// cheaper to give up. egui has no partial repaint, so moving one meter bar
/// redraws the whole window — over an empty Scene, somebody talking into a
/// microphone cost about two points of GPU at thirty. The meter already
/// falls smoothly between readings (see [`DECAY_DB_PER_SECOND`]), so a third
/// fewer of them is hard to see and a third less of that cost.
///
/// Also the delay each request asks for, which is what lets one coincide
/// with a repaint the Preview was going to cause anyway — egui counts a pass
/// that happens first as having served it. Over a Scene that is moving, the
/// meters cost nothing at any rate.
pub const METER_INTERVAL: Duration = Duration::from_millis(50);

/// What a meter's number is stored as: `f32` bits, with zero kept for "never
/// measured".
///
/// Which leaves one level that cannot be stored as itself, and it is the one
/// that matters most: exactly 0 dB, which is what a sample at exactly full
/// scale reads — and a 16-bit `i16::MAX` is exactly that. Stored plainly it
/// would read back as no measurement at all, so a meter pinned at the top
/// would go blank and its clip lamp would never light. Negative zero is the
/// same level to every comparison and is not zero bits.
fn level_bits(level_db: f32) -> u32 {
    if level_db == 0.0 {
        (-0.0f32).to_bits()
    } else {
        level_db.to_bits()
    }
}

/// Asks the UI to draw because a meter moved, no more often than
/// [`METER_INTERVAL`].
///
/// One for the whole application, cloned into every writer, so the limit is
/// on the window rather than per meter — six sources talking at once are
/// still one repaint per interval. Cloning shares it; calling it costs two
/// atomic operations and, at most once an interval, whatever the callback
/// does.
#[derive(Clone)]
pub struct MeterWake {
    shared: Arc<WakeShared>,
}

struct WakeShared {
    wake: Box<dyn Fn() + Send + Sync>,
    epoch: Instant,
    /// When `wake` last ran, in nanoseconds after `epoch` plus one, so that
    /// zero can mean it never has.
    last: AtomicU64,
}

impl MeterWake {
    /// `wake` should ask for a repaint within [`METER_INTERVAL`] rather than
    /// at once — see there.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            shared: Arc::new(WakeShared {
                wake: Box::new(wake),
                epoch: Instant::now(),
                last: AtomicU64::new(0),
            }),
        }
    }

    fn now(&self) -> u64 {
        u64::try_from(self.shared.epoch.elapsed().as_nanos())
            .unwrap_or(u64::MAX - 1)
            .saturating_add(1)
    }

    /// Wakes the UI unless it was woken less than an interval ago.
    ///
    /// The one that loses a race between two writers is dropped rather than
    /// retried: the winner's repaint reads both their numbers.
    fn wake(&self) {
        let now = self.now();
        let last = self.shared.last.load(Ordering::Relaxed);
        let interval = METER_INTERVAL.as_nanos() as u64;
        if last != 0 && now.saturating_sub(last) < interval {
            return;
        }
        if self
            .shared
            .last
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            (self.shared.wake)();
        }
    }

    /// Wakes the UI whatever the interval says.
    ///
    /// For the one wake that must not be dropped: the last. A meter that has
    /// just come to rest is asked for nothing after it, so a limited wake
    /// lost here would leave the level before it on screen until something
    /// else happened to draw.
    fn wake_regardless(&self) {
        self.shared.last.store(self.now(), Ordering::Relaxed);
        (self.shared.wake)();
    }
}

/// One source's meter, on the thread its buffers arrive on: what it shows,
/// and waking the UI when that moves.
///
/// Rises to a buffer's peak at once and falls at [`DECAY_DB_PER_SECOND`], and
/// the falling is worked out here rather than in the UI because this is the
/// side that sees every buffer. The UI reads one number per repaint, three
/// buffers or so apart; a meter that only held the newest buffer's peak lost
/// the other two, loud ones included. What it holds now has already taken
/// them in.
///
/// Time is counted in samples, not on a clock: a buffer says how long it
/// lasts, and the level falls by that much whenever it arrives. A meter
/// paused with its source therefore resumes where it stopped instead of
/// having fallen through the pause.
pub(in crate::engine) struct Meter {
    shown: f32,
    wake: MeterWake,
}

impl Meter {
    pub(in crate::engine) fn new(wake: MeterWake) -> Self {
        Self {
            shown: METER_FLOOR_DB,
            wake,
        }
    }

    /// Takes in one buffer, stores what the meter now shows in `into`, and
    /// wakes the UI while that is anything but the floor.
    ///
    /// Silence costs nothing: a meter resting at the floor asks for no
    /// repaint at all, which is what keeps a quiet session from drawing the
    /// whole window twenty times a second for a channel with nothing in it.
    pub(in crate::engine) fn measure(
        &mut self,
        frame: &media_pp::ffmpeg::frame::Audio,
        into: &AtomicU32,
    ) {
        let lasts = frame.samples() as f32 / frame.rate().max(1) as f32;
        let before = self.shown;
        self.shown = peak_db(frame)
            .max(before - DECAY_DB_PER_SECOND * lasts)
            .max(METER_FLOOR_DB);
        into.store(level_bits(self.shown), Ordering::Relaxed);
        if self.shown > METER_FLOOR_DB {
            self.wake.wake();
        } else if before > METER_FLOOR_DB {
            self.wake.wake_regardless();
        }
    }
}

/// What each source's meter shows, in `f32` bits so the UI can read it
/// without a lock.
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

    /// What this source's meter shows — see [`Meter`] — or `None` when it
    /// has produced nothing, which is what a source that failed to open, or
    /// has not been given a device, looks like.
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
fn peak_db(frame: &media_pp::ffmpeg::frame::Audio) -> f32 {
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

    /// `seconds` of mono audio at 48 kHz, every sample at `loudest`.
    fn buffer(loudest: f32, seconds: f32) -> media_pp::ffmpeg::frame::Audio {
        use media_pp::ffmpeg;

        let mut frame = ffmpeg::frame::Audio::new(
            ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
            (48_000.0 * seconds) as usize,
            ffmpeg::ChannelLayout::default(1),
        );
        frame.set_rate(48_000);
        frame.plane_mut::<f32>(0).fill(loudest);
        frame
    }

    /// A meter, the number it publishes, and how often it has woken the UI.
    fn meter() -> (Meter, AtomicU32, Arc<std::sync::atomic::AtomicUsize>) {
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&woken);
        let wake = MeterWake::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        (Meter::new(wake), AtomicU32::new(0), woken)
    }

    fn shown(level: &AtomicU32) -> f32 {
        f32::from_bits(level.load(Ordering::Relaxed))
    }

    /// Up at once, down at the decay rate — and the time it falls for is
    /// how long the audio lasted, not how long it took to arrive.
    #[test]
    fn a_meter_rises_at_once_and_falls_by_the_length_of_what_follows() {
        let (mut meter, level, _) = meter();

        // Half of full scale is about -6 dB.
        meter.measure(&buffer(0.5, 0.01), &level);
        let loud = shown(&level);
        assert!((loud + 6.02).abs() < 0.01, "rose straight to {loud}");

        // Half a second of silence takes half the decay rate off it.
        meter.measure(&buffer(0.0, 0.5), &level);
        let expected = loud - DECAY_DB_PER_SECOND * 0.5;
        assert!(
            (shown(&level) - expected).abs() < 0.01,
            "fell to {} where {expected} was due",
            shown(&level)
        );

        // Something louder than where it has fallen to wins outright.
        meter.measure(&buffer(1.0, 0.01), &level);
        assert_eq!(shown(&level), 0.0);
    }

    /// The loud buffers between two repaints are what a meter of the newest
    /// peak alone lost: a quiet one after a loud one used to be all the UI
    /// would see.
    #[test]
    fn a_loud_buffer_is_still_showing_after_a_quiet_one() {
        let (mut meter, level, _) = meter();
        meter.measure(&buffer(1.0, 0.01), &level);
        meter.measure(&buffer(0.001, 0.01), &level);
        assert!(
            shown(&level) > -1.0,
            "ten milliseconds later the loud one is still most of it, got {}",
            shown(&level)
        );
    }

    /// Exactly full scale is what a 16-bit sample at its maximum reads, and
    /// plain `f32` bits for it are the zero that means "never measured".
    #[test]
    fn full_scale_is_stored_as_a_measurement() {
        assert_ne!(level_bits(0.0), 0, "zero bits would read as no reading");
        assert_eq!(f32::from_bits(level_bits(0.0)), 0.0);
        assert!(
            f32::from_bits(level_bits(0.0)) >= 0.0,
            "and it still counts as reaching full scale for the clip lamp"
        );

        let (mut meter, level, _) = meter();
        meter.measure(&buffer(1.0, 0.01), &level);
        assert_ne!(level.load(Ordering::Relaxed), 0);
    }

    /// Silence asks for nothing. This is the whole cost of the change for a
    /// session with nothing playing: none.
    #[test]
    fn a_silent_meter_never_wakes_the_ui() {
        let (mut meter, level, woken) = meter();
        for _ in 0..100 {
            meter.measure(&buffer(0.0, 0.01), &level);
        }
        assert_eq!(woken.load(Ordering::SeqCst), 0);
        assert_eq!(shown(&level), METER_FLOOR_DB);
    }

    /// A meter that is moving wakes the UI, but no more often than the
    /// interval, however many buffers arrive in it.
    #[test]
    fn a_moving_meter_wakes_the_ui_no_more_often_than_the_interval() {
        let (mut meter, level, woken) = meter();
        let started = Instant::now();
        for _ in 0..100 {
            meter.measure(&buffer(0.5, 0.01), &level);
        }
        let allowed = started.elapsed().as_nanos() / METER_INTERVAL.as_nanos() + 1;
        let woken = woken.load(Ordering::SeqCst) as u128;
        assert!(woken >= 1, "a meter that moved must be drawn");
        assert!(
            woken <= allowed,
            "{woken} wakes where the interval allows {allowed}"
        );
    }

    /// Coming to rest is the one wake the limit may not drop: nothing after
    /// it would ask again, and the last level drawn would stay on screen.
    #[test]
    fn coming_to_rest_always_wakes_the_ui() {
        let (mut meter, level, woken) = meter();
        meter.measure(&buffer(1.0, 0.01), &level);
        let before = woken.load(Ordering::SeqCst);

        // Four seconds of silence is more than the whole scale's fall, and
        // it arrives well inside the interval the wake above started.
        meter.measure(&buffer(0.0, 4.0), &level);
        assert_eq!(shown(&level), METER_FLOOR_DB);
        assert_eq!(woken.load(Ordering::SeqCst), before + 1);
    }
}
