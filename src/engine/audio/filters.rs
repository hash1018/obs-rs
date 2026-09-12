//! Turning a mixer channel's stored filters into elements in its chain.
//!
//! The audio twin of `engine::source::filters`, and a rack for the same
//! reason: a filter added, removed or reordered is swapped in between two
//! buffers rather than by reopening the capture — which for a microphone is
//! a gap in what it records, and for Desktop Audio the same.
//!
//! The rack sits between the capture and the fader, which is where a
//! streaming application puts a channel's filters and where they belong: a
//! gate listens to what the microphone heard, not to what was left after the
//! fader, or pulling the fader down would close it.
//!
//! # The bridge
//!
//! Every filter takes `f32`, and noise suppression takes 48 kHz only. A
//! capture is usually both already — it is what the endpoints run at — but
//! one that is not gets an [`AudioResampler`] at the head of the rack while
//! a filter needs it, the way a picture rack gets its colour conversion. It
//! goes with the last filter that needed it, and the channel is back to what
//! it cost before.
//!
//! # What changes cost
//!
//! A filter turned off is left out of the rack rather than kept in it doing
//! nothing: no element has a switch, and one that is not there costs nothing
//! at all. So enabling, adding, removing and reordering all refill the rack,
//! and only settings go through a handle.

use std::collections::HashMap;
use std::time::Duration;

use media_pp::contract::{InputContract, MediaKind, MemoryDomain, OutputContract, PortContract};
use media_pp::element::Filter as PpFilter;
use media_pp::elements::{
    AudioCompressor, AudioCompressorHandle, AudioCompressorOptions, AudioFormat, AudioGate,
    AudioGateHandle, AudioGateOptions, AudioLimiter, AudioLimiterHandle, AudioLimiterOptions,
    AudioResampler, NOISE_SUPPRESSOR_SAMPLE_RATE, NoiseSuppressor, Rack, RackHandle,
};
use media_pp::ffmpeg;

use crate::domain::{
    AudioFilter, AudioFilterId, AudioFilterKind, AudioFilterSettings, CompressorSettings,
    LimiterSettings, NoiseGateSettings,
};

use super::BackendError;

/// What a build answers: the elements for the rack, in order, and the ones
/// among them with settings by the filter they stand for — two lists of
/// different lengths, since a bridge and a suppressor have nothing to retune.
type Built = Result<(Vec<Box<dyn PpFilter>>, HashMap<AudioFilterId, Tuner>), BackendError>;

/// The handle of a filter in the rack whose settings change without a
/// refill.
enum Tuner {
    Gate(AudioGateHandle),
    Compressor(AudioCompressorHandle),
    Limiter(AudioLimiterHandle),
}

/// A channel's rack, and what refilling it needs.
///
/// Held by the open source rather than by its branch — see the picture
/// rack's own `FilterRack` for the reasoning, which is the same.
pub(super) struct AudioFilterRack {
    handle: RackHandle,
    name: String,
    /// What the capture delivers, which is what the rack is handed and so
    /// what a bridge would convert from.
    capture: AudioFormat,
    /// What is in the rack now: the filters that are on, in order.
    running: Vec<(AudioFilterId, AudioFilterKind)>,
    /// The ones among them with settings, which change without a refill.
    tuners: HashMap<AudioFilterId, Tuner>,
}

/// Creates a channel's rack, empty, and the way back to it.
///
/// Its contracts are declared rather than derived — what is in it changes —
/// and say the one thing true whatever it holds: decoded audio in system
/// memory.
pub(super) fn rack(name: &str, capture: AudioFormat) -> (Rack, AudioFilterRack) {
    let port = PortContract::frame(MediaKind::AudioFrame, MemoryDomain::System);
    let (rack, handle) = Rack::new(
        format!("{name}-filters"),
        InputContract::Fixed(port),
        OutputContract::Fixed(port),
    );
    (
        rack,
        AudioFilterRack {
            handle,
            name: name.to_owned(),
            capture,
            running: Vec::new(),
            tuners: HashMap::new(),
        },
    )
}

impl AudioFilterRack {
    /// Brings the rack in line with what the project holds.
    ///
    /// A refill only when which filters are on, or their order, has changed;
    /// otherwise the filters are retuned through their handles and nothing is
    /// swapped. Atomic from the caller's side, as the picture rack's is:
    /// every element is built before anything is swapped, so one that fails
    /// to build leaves the rack running what it was.
    pub(super) fn apply(&mut self, filters: &[AudioFilter]) -> Result<(), BackendError> {
        if shape(filters) == self.running {
            for filter in filters {
                self.retune(filter.id, &filter.settings);
            }
            return Ok(());
        }
        let (elements, tuners) = self.build(filters)?;
        self.handle
            .replace(elements)
            .map_err(|error| BackendError::from(error.to_string()))?;
        self.running = shape(filters);
        self.tuners = tuners;
        Ok(())
    }

    /// One filter's settings, for a slider still under the pointer — the
    /// project hears once the gesture ends, as the fader's does.
    ///
    /// Every mapping below sanitises first, so none of these can be refused.
    pub(super) fn retune(&self, id: AudioFilterId, settings: &AudioFilterSettings) {
        match (self.tuners.get(&id), settings) {
            (Some(Tuner::Gate(handle)), AudioFilterSettings::NoiseGate(settings)) => {
                let _ = handle.set_options(gate_options(*settings));
            }
            (Some(Tuner::Compressor(handle)), AudioFilterSettings::Compressor(settings)) => {
                let _ = handle.set_options(compressor_options(*settings));
            }
            (Some(Tuner::Limiter(handle)), AudioFilterSettings::Limiter(settings)) => {
                let _ = handle.set_options(limiter_options(*settings));
            }
            _ => {}
        }
    }

    fn build(&self, filters: &[AudioFilter]) -> Built {
        let on: Vec<&AudioFilter> = filters.iter().filter(|filter| filter.enabled).collect();
        let mut elements: Vec<Box<dyn PpFilter>> = Vec::with_capacity(on.len() + 1);
        if let Some(target) = bridge(self.capture, &on) {
            elements.push(Box::new(
                AudioResampler::new(
                    format!("{}-filter-format", self.name),
                    target,
                    // Both captures count their `pts` in samples at their own
                    // rate — see `time_base` on either.
                    ffmpeg::Rational::new(1, self.capture.sample_rate as i32),
                )
                .map_err(|error| BackendError::from(error.to_string()))?,
            ));
        }
        let mut tuners = HashMap::new();
        let refused = |error: &dyn std::fmt::Display| BackendError::from(error.to_string());
        for filter in on {
            let name = |role: &str| format!("{}-{role}-{}", self.name, filter.id.0);
            match filter.settings {
                AudioFilterSettings::NoiseSuppression => {
                    elements.push(Box::new(NoiseSuppressor::new(name("denoise"))));
                }
                AudioFilterSettings::NoiseGate(settings) => {
                    let (gate, handle) =
                        AudioGate::with_options(name("gate"), gate_options(settings))
                            .map_err(|error| refused(&error))?;
                    elements.push(Box::new(gate));
                    tuners.insert(filter.id, Tuner::Gate(handle));
                }
                AudioFilterSettings::Compressor(settings) => {
                    let (compressor, handle) = AudioCompressor::with_options(
                        name("compressor"),
                        compressor_options(settings),
                    )
                    .map_err(|error| refused(&error))?;
                    elements.push(Box::new(compressor));
                    tuners.insert(filter.id, Tuner::Compressor(handle));
                }
                AudioFilterSettings::Limiter(settings) => {
                    let (limiter, handle) =
                        AudioLimiter::with_options(name("limiter"), limiter_options(settings))
                            .map_err(|error| refused(&error))?;
                    elements.push(Box::new(limiter));
                    tuners.insert(filter.id, Tuner::Limiter(handle));
                }
            }
        }
        Ok((elements, tuners))
    }
}

/// What identifies the chain that should be running: the filters that are
/// on, of which kinds, in which order. Anything else about them is a handle
/// call.
fn shape(filters: &[AudioFilter]) -> Vec<(AudioFilterId, AudioFilterKind)> {
    filters
        .iter()
        .filter(|filter| filter.enabled)
        .map(|filter| (filter.id, filter.settings.kind()))
        .collect()
}

/// The format a bridge at the head of the rack converts to, or `None` when
/// the capture's own will do for every filter that is on.
fn bridge(capture: AudioFormat, on: &[&AudioFilter]) -> Option<AudioFormat> {
    if on.is_empty() {
        return None;
    }
    let is_f32 = matches!(capture.sample_format, ffmpeg::format::Sample::F32(_));
    let suppressing = on
        .iter()
        .any(|filter| filter.settings.kind() == AudioFilterKind::NoiseSuppression);
    let rate = if suppressing {
        NOISE_SUPPRESSOR_SAMPLE_RATE
    } else {
        capture.sample_rate
    };
    if is_f32 && rate == capture.sample_rate {
        return None;
    }
    Some(AudioFormat::new(
        ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
        rate,
        capture.channels,
    ))
}

/// The library's options for what the project stored.
fn gate_options(settings: NoiseGateSettings) -> AudioGateOptions {
    let settings = settings.sanitised();
    AudioGateOptions {
        open_threshold_db: settings.open_threshold_db,
        close_threshold_db: settings.close_threshold_db,
        attack: Duration::from_millis(settings.attack_ms.into()),
        hold: Duration::from_millis(settings.hold_ms.into()),
        release: Duration::from_millis(settings.release_ms.into()),
    }
}

fn compressor_options(settings: CompressorSettings) -> AudioCompressorOptions {
    let settings = settings.sanitised();
    AudioCompressorOptions {
        threshold_db: settings.threshold_db,
        ratio: settings.ratio,
        attack: Duration::from_millis(settings.attack_ms.into()),
        release: Duration::from_millis(settings.release_ms.into()),
        output_gain_db: settings.output_gain_db,
    }
}

fn limiter_options(settings: LimiterSettings) -> AudioLimiterOptions {
    let settings = settings.sanitised();
    AudioLimiterOptions {
        threshold_db: settings.threshold_db,
        release: Duration::from_millis(settings.release_ms.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(id: i64, enabled: bool, settings: AudioFilterSettings) -> AudioFilter {
        AudioFilter {
            id: AudioFilterId(id),
            enabled,
            settings,
        }
    }

    fn format(sample_format: ffmpeg::format::Sample, rate: u32) -> AudioFormat {
        AudioFormat::new(sample_format, rate, 2)
    }

    const F32: ffmpeg::format::Sample =
        ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed);
    const I16: ffmpeg::format::Sample =
        ffmpeg::format::Sample::I16(ffmpeg::format::sample::Type::Packed);

    /// A capture already in a format every filter takes gets no bridge —
    /// which is the usual case, and the one that must cost nothing.
    #[test]
    fn a_capture_every_filter_takes_is_not_converted() {
        let suppress = filter(1, true, AudioFilterSettings::NoiseSuppression);
        assert_eq!(bridge(format(F32, 48_000), &[&suppress]), None);
        let gate = filter(
            2,
            true,
            AudioFilterSettings::default_for(AudioFilterKind::NoiseGate),
        );
        assert_eq!(
            bridge(format(F32, 44_100), &[&gate]),
            None,
            "a gate takes any rate"
        );
        assert_eq!(
            bridge(format(I16, 44_100), &[]),
            None,
            "nothing to convert for"
        );
    }

    /// Noise suppression takes 48 kHz only, and either filter `f32` only.
    #[test]
    fn a_capture_a_filter_cannot_take_is_converted_to_what_it_can() {
        let suppress = filter(1, true, AudioFilterSettings::NoiseSuppression);
        assert_eq!(
            bridge(format(F32, 44_100), &[&suppress]),
            Some(format(F32, 48_000))
        );
        let gate = filter(
            2,
            true,
            AudioFilterSettings::default_for(AudioFilterKind::NoiseGate),
        );
        assert_eq!(
            bridge(format(I16, 44_100), &[&gate]),
            Some(format(F32, 44_100)),
            "the rate is left alone when nothing needs it changed"
        );
    }

    /// Turning a filter off takes it out of the chain, and so changes the
    /// shape; retuning one does not.
    #[test]
    fn only_what_is_on_and_its_order_decide_a_refill() {
        let gate = |open| {
            AudioFilterSettings::NoiseGate(NoiseGateSettings {
                open_threshold_db: open,
                ..NoiseGateSettings::default()
            })
        };
        let before = [
            filter(1, true, AudioFilterSettings::NoiseSuppression),
            filter(2, true, gate(-26.0)),
        ];
        let retuned = [
            filter(1, true, AudioFilterSettings::NoiseSuppression),
            filter(2, true, gate(-40.0)),
        ];
        let off = [
            filter(1, false, AudioFilterSettings::NoiseSuppression),
            filter(2, true, gate(-26.0)),
        ];
        assert_eq!(shape(&before), shape(&retuned));
        assert_ne!(shape(&before), shape(&off));
        assert_eq!(
            shape(&off),
            [(AudioFilterId(2), AudioFilterKind::NoiseGate)]
        );
    }

    /// What the project stores is what the element takes, whatever the
    /// sliders were left at.
    #[test]
    fn stored_gate_settings_are_always_ones_the_gate_takes() {
        let settings = NoiseGateSettings {
            open_threshold_db: -40.0,
            close_threshold_db: -10.0,
            ..NoiseGateSettings::default()
        };
        assert!(AudioGate::with_options("gate", gate_options(settings)).is_ok());
        let compressor = CompressorSettings {
            threshold_db: f32::NAN,
            ratio: 0.0,
            ..CompressorSettings::default()
        };
        assert!(
            AudioCompressor::with_options("compressor", compressor_options(compressor)).is_ok()
        );
        let limiter = LimiterSettings {
            threshold_db: f32::NEG_INFINITY,
            ..LimiterSettings::default()
        };
        assert!(AudioLimiter::with_options("limiter", limiter_options(limiter)).is_ok());
    }
}
