//! What is done to an audio source's sound before it reaches the mix.
//!
//! The audio counterpart of [`super::Filter`], and kept apart from it rather
//! than made another kind of it. A picture filter hangs off a Source and an
//! audio one off an [`AudioSource`](super::AudioSource) — a mixer channel,
//! which is in no Scene — so they are stored against different rows, and
//! neither kind of filter makes sense on the other's owner: there is no
//! picture in a microphone to key.
//!
//! Separate ids follow from separate rows. A [`super::FilterId`] and an
//! [`AudioFilterId`] can hold the same number, and a command naming one can
//! never be read as naming the other.

/// Row identity for one audio filter, assigned by the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioFilterId(pub i64);

stored_by_name! {
    /// What an audio filter does.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AudioFilterKind {
        /// Takes steady background noise out of speech.
        NoiseSuppression => "noise_suppression",
        /// Silences what is below a level.
        NoiseGate => "noise_gate",
        /// Turns down what is above a level, by a ratio.
        Compressor => "compressor",
        /// Lets nothing out above a level.
        Limiter => "limiter",
    }
}

/// One filter on an audio source, as it is stored and shown.
///
/// `enabled` sits beside the settings rather than inside them for the reason
/// a picture filter's does: turning one off must not cost it what it was
/// tuned to.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioFilter {
    pub id: AudioFilterId,
    pub enabled: bool,
    /// Which kind this is, and its settings, in one — see
    /// [`super::Filter::settings`] on why there is no separate kind.
    pub settings: AudioFilterSettings,
}

/// The settings of whichever audio filter this is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AudioFilterSettings {
    /// Nothing to set: RNNoise has no knob, which is much of why it is the
    /// suppression people reach for.
    NoiseSuppression,
    NoiseGate(NoiseGateSettings),
    Compressor(CompressorSettings),
    Limiter(LimiterSettings),
}

impl AudioFilterSettings {
    pub fn kind(&self) -> AudioFilterKind {
        match self {
            Self::NoiseSuppression => AudioFilterKind::NoiseSuppression,
            Self::NoiseGate(_) => AudioFilterKind::NoiseGate,
            Self::Compressor(_) => AudioFilterKind::Compressor,
            Self::Limiter(_) => AudioFilterKind::Limiter,
        }
    }

    /// What a filter of `kind` starts on.
    pub fn default_for(kind: AudioFilterKind) -> Self {
        match kind {
            AudioFilterKind::NoiseSuppression => Self::NoiseSuppression,
            AudioFilterKind::NoiseGate => Self::NoiseGate(NoiseGateSettings::default()),
            AudioFilterKind::Compressor => Self::Compressor(CompressorSettings::default()),
            AudioFilterKind::Limiter => Self::Limiter(LimiterSettings::default()),
        }
    }

    /// The same settings brought within range, whichever kind they are —
    /// what every way of changing them passes through.
    pub fn sanitised(self) -> Self {
        match self {
            Self::NoiseSuppression => Self::NoiseSuppression,
            Self::NoiseGate(settings) => Self::NoiseGate(settings.sanitised()),
            Self::Compressor(settings) => Self::Compressor(settings.sanitised()),
            Self::Limiter(settings) => Self::Limiter(settings.sanitised()),
        }
    }
}

/// `db` within `min..=max`, and a level that is no level at all at
/// `otherwise` — where the filter it is for does least.
fn clamp_db(db: f32, min: f32, max: f32, otherwise: f32) -> f32 {
    if db.is_finite() {
        db.clamp(min, max)
    } else {
        otherwise
    }
}

/// A noise gate's five settings, as the Filters dock edits them.
///
/// Mirrors `media_pp::elements::AudioGateOptions` rather than reusing it, for
/// the reason [`super::ChromaKeySettings`] mirrors its element's: this is
/// written to the database and drawn by the UI. Whole milliseconds rather
/// than a `Duration`, because that is the unit the dock shows and stores and
/// nothing finer is ever set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseGateSettings {
    /// The level, in dBFS, at or above which a closed gate opens.
    pub open_threshold_db: f32,
    /// The level, in dBFS, below which an open gate starts to close. Kept at
    /// or below `open_threshold_db` wherever it is set.
    pub close_threshold_db: f32,
    pub attack_ms: u32,
    pub hold_ms: u32,
    pub release_ms: u32,
}

impl Default for NoiseGateSettings {
    /// A streaming application's noise gate starts from these, so they are
    /// the numbers someone who has used one expects to see.
    fn default() -> Self {
        Self {
            open_threshold_db: -26.0,
            close_threshold_db: -32.0,
            attack_ms: 25,
            hold_ms: 200,
            release_ms: 150,
        }
    }
}

impl NoiseGateSettings {
    /// The lowest a threshold can be set to, in dBFS. Below this the gate is
    /// open to anything a microphone picks up at all.
    pub const MIN_THRESHOLD_DB: f32 = -96.0;

    /// The same settings with the two thresholds put in order and within
    /// range — what every way of changing them passes through, so the
    /// engine is never handed a pair its gate refuses.
    pub fn sanitised(self) -> Self {
        let clamp = |db: f32| clamp_db(db, Self::MIN_THRESHOLD_DB, 0.0, Self::MIN_THRESHOLD_DB);
        let open = clamp(self.open_threshold_db);
        Self {
            open_threshold_db: open,
            close_threshold_db: clamp(self.close_threshold_db).min(open),
            ..self
        }
    }
}

/// A compressor's five settings, as the Filters dock edits them — mirroring
/// `media_pp::elements::AudioCompressorOptions` as [`NoiseGateSettings`]
/// mirrors the gate's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompressorSettings {
    /// The level, in dBFS, above which it starts turning down.
    pub threshold_db: f32,
    /// How much it turns down what is over the threshold: at 4, a level 12 dB
    /// over comes out 3 dB over.
    pub ratio: f32,
    pub attack_ms: u32,
    pub release_ms: u32,
    /// Gain after compression, in dB, to make up for what it took off.
    pub output_gain_db: f32,
}

impl Default for CompressorSettings {
    /// A streaming application's compressor starts from these.
    fn default() -> Self {
        Self {
            threshold_db: -18.0,
            ratio: 10.0,
            attack_ms: 6,
            release_ms: 60,
            output_gain_db: 0.0,
        }
    }
}

impl CompressorSettings {
    pub const MIN_THRESHOLD_DB: f32 = -60.0;
    pub const MAX_RATIO: f32 = 32.0;
    /// How far the output gain goes either way, in dB.
    pub const OUTPUT_GAIN_DB: f32 = 32.0;

    /// The same settings within range. A ratio under one would be an
    /// expander, which the element refuses; one that is not a number is
    /// taken as one, which does nothing.
    pub fn sanitised(self) -> Self {
        Self {
            threshold_db: clamp_db(self.threshold_db, Self::MIN_THRESHOLD_DB, 0.0, 0.0),
            ratio: if self.ratio.is_finite() {
                self.ratio.clamp(1.0, Self::MAX_RATIO)
            } else {
                1.0
            },
            output_gain_db: if self.output_gain_db.is_finite() {
                self.output_gain_db
                    .clamp(-Self::OUTPUT_GAIN_DB, Self::OUTPUT_GAIN_DB)
            } else {
                0.0
            },
            ..self
        }
    }
}

/// A limiter's two settings, as the Filters dock edits them — mirroring
/// `media_pp::elements::AudioLimiterOptions`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LimiterSettings {
    /// The level, in dBFS, nothing comes out over.
    pub threshold_db: f32,
    pub release_ms: u32,
}

impl Default for LimiterSettings {
    /// A streaming application's limiter starts from these.
    fn default() -> Self {
        Self {
            threshold_db: -6.0,
            release_ms: 60,
        }
    }
}

impl LimiterSettings {
    pub const MIN_THRESHOLD_DB: f32 = -60.0;

    /// The same settings within range.
    pub fn sanitised(self) -> Self {
        Self {
            threshold_db: clamp_db(self.threshold_db, Self::MIN_THRESHOLD_DB, 0.0, 0.0),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_survives_a_round_trip_through_storage() {
        for kind in AudioFilterKind::ALL {
            assert_eq!(
                AudioFilterKind::from_storage_name(kind.storage_name()),
                Some(kind)
            );
            assert_eq!(AudioFilterSettings::default_for(kind).kind(), kind);
        }
        assert_eq!(AudioFilterKind::from_storage_name("reverb"), None);
    }

    /// The element refuses a close threshold above the open one, and a
    /// slider dragged past the other would otherwise hand it exactly that.
    #[test]
    fn a_close_threshold_above_the_open_one_is_brought_down_to_it() {
        let settings = NoiseGateSettings {
            open_threshold_db: -30.0,
            close_threshold_db: -20.0,
            ..NoiseGateSettings::default()
        }
        .sanitised();
        assert_eq!(settings.close_threshold_db, -30.0);
    }

    /// What the elements refuse — a ratio under one, a level that is not a
    /// number — comes back as where each does least, not as an error: a
    /// value from a database somebody edited.
    #[test]
    fn a_compressor_or_limiter_setting_the_element_refuses_is_brought_to_where_it_does_least() {
        let compressor = CompressorSettings {
            threshold_db: f32::NAN,
            ratio: 0.5,
            output_gain_db: f32::INFINITY,
            ..CompressorSettings::default()
        }
        .sanitised();
        assert_eq!(
            (
                compressor.threshold_db,
                compressor.ratio,
                compressor.output_gain_db
            ),
            (0.0, 1.0, 0.0)
        );
        let limiter = LimiterSettings {
            threshold_db: -200.0,
            ..LimiterSettings::default()
        }
        .sanitised();
        assert_eq!(limiter.threshold_db, LimiterSettings::MIN_THRESHOLD_DB);
    }

    #[test]
    fn a_threshold_out_of_range_is_brought_into_it() {
        let settings = NoiseGateSettings {
            open_threshold_db: 12.0,
            close_threshold_db: f32::NAN,
            ..NoiseGateSettings::default()
        }
        .sanitised();
        assert_eq!(settings.open_threshold_db, 0.0);
        assert_eq!(
            settings.close_threshold_db,
            NoiseGateSettings::MIN_THRESHOLD_DB
        );
    }
}
