//! The engine loop's side of a recording: what it holds between one, and how
//! one is started.
//!
//! `super` is the file, the muxer and the two branches that write it — one
//! recording, once it exists. This is what the loop keeps whether or not
//! there is one: the settings a recording would be made with, the mixer
//! every Source's sound attaches to, and the decision about which encoder
//! and codec this machine can actually deliver.
//!
//! # Why it is here and not in the loop
//!
//! Because the loop is not the only thing that will want it. A recording and
//! a stream are the same graph up to the muxer — the compositor's `Tee` and
//! the mixer's, each through an encoder — and differ in where the packets
//! go. Whatever sends them somewhere else belongs beside this, not as three
//! hundred more lines in a file that is already the engine's state machine,
//! its command dispatch and its layer geometry.

use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwapOption;

use super::super::audio;
use super::super::backend::{Backend, BackendError};
use super::Output;

/// Everything a recording is opened from, and the one that is open.
///
/// Grouped because they are only ever reached together, and because
/// `apply_command` had collected as many parameters as it can carry.
pub(in crate::engine) struct OutputState {
    /// What the *next* recording is written as. Loaded from disk before the
    /// engine existed — see `ObsApp::new` — and replaced whenever the
    /// Settings dialog is applied.
    pub(in crate::engine) settings: crate::settings::RecordingSettings,
    /// Where the *next* broadcast is published, and how. Arrives the same
    /// way and is read at the same moment: when one starts.
    pub(in crate::engine) streaming: crate::settings::StreamingSettings,
    /// The mixer, taken once at startup because it lives on a thread this one
    /// cannot ask. `None` when it never started, which records video only and
    /// plays media files without their sound.
    ///
    /// Two things attach to it from here: a recording's audio track, on the
    /// `Tee`, and a media file Source's own audio, as one more mixer input.
    /// It sits on this struct because a recording was the first of them; the
    /// second arriving is not on its own a reason to move it.
    pub(in crate::engine) mixer: Option<(
        media_pp::elements::TeeHandle,
        media_pp::elements::MixerHandle,
    )>,
    /// The mix that is played back, read fresh every pass rather than taken
    /// once like the one above.
    ///
    /// It comes and goes: there is none until a monitoring endpoint is
    /// chosen, and none again when one is taken away. A handle held from
    /// startup would be a mix nothing plays.
    pub(in crate::engine) monitor: Arc<ArcSwapOption<media_pp::elements::MixerHandle>>,
    /// Which audio codecs the linked FFmpeg carries, probed once at startup
    /// beside the video list. Kept so a stored codec that cannot open falls
    /// back rather than failing the recording — see [`usable_settings`].
    pub(in crate::engine) audio_codecs: Vec<crate::settings::RecordingAudioCodec>,
    /// The recording that is running, if one is. It rather than the backend
    /// holds the video branch too — see [`Output`].
    pub(in crate::engine) running: Option<Output>,
    /// The broadcast that is running, if one is.
    ///
    /// Beside the recording rather than instead of it: both hang off the
    /// same two `Tee`s, which take as many branches as are asked of them, so
    /// recording while streaming is not a mode — it is simply both fields
    /// being `Some`, each with its own encoder at its own bit rate.
    pub(in crate::engine) broadcast: Option<Output>,
}

impl OutputState {
    /// The mixer's own control, for whatever attaches an input to it.
    pub(in crate::engine) fn mixer_handle(&self) -> Option<&media_pp::elements::MixerHandle> {
        self.mixer.as_ref().map(|(_, mixer)| mixer)
    }

    /// The monitor mix as it stands right now, or `None` while nothing is
    /// being played back.
    pub(in crate::engine) fn monitor_handle(&self) -> Option<media_pp::elements::MixerHandle> {
        self.monitor.load_full().map(|handle| (*handle).clone())
    }

    /// What the mixer is actually summing into, or the default when it never
    /// started.
    ///
    /// Asked of the mixer rather than of the settings, because a format it
    /// refused leaves the old one running and the audio encoder has to be
    /// opened for what is really arriving.
    pub(in crate::engine) fn mix_format(&self) -> media_pp::elements::MixFormat {
        self.mixer
            .as_ref()
            .and_then(|(_, handle)| handle.mix_format())
            .unwrap_or(audio::DEFAULT_MIX_FORMAT)
    }
}

/// Opens one recording, returning when it started rather than `()` — the
/// clock the status bar counts from is the moment the file began taking
/// frames, not the moment the button was pressed.
pub(in crate::engine) fn start_recording(
    backend: &Backend,
    recording: &mut OutputState,
) -> Result<Instant, BackendError> {
    if recording.running.is_some() {
        return Err("a recording is already running".into());
    }
    // Probed here rather than taken from the list published at startup: the
    // mix format can have moved since, and which encoders open depends on it.
    // Two `avcodec_open2` calls, beside a video encoder and a muxer that are
    // about to be opened anyway.
    let audio_codecs = super::available_audio_codecs(recording.mix_format());
    let settings = usable_settings(backend, &audio_codecs, &recording.settings);
    let settings = &settings;
    let path = crate::paths::recording_file_in(
        &settings.directory_or_default(),
        settings.prefix_or_default(),
        // A recording is named for the user's own wall clock, which is what
        // makes the stamp mean anything to the person looking for the file.
        // Through `clock` rather than `now_local`, which refuses to answer
        // in a process with more than one thread and so was answering `Err`
        // here every time — naming every recording in UTC.
        crate::clock::now_local(),
        settings.format,
    );
    let running = Output::start(
        backend,
        recording.mixer.as_ref(),
        backend.frame_rate(),
        &settings.encoding(backend.size),
        |tracks| super::open_muxer(&path, settings, tracks),
    )?;
    recording.running = Some(running);
    println!("recording to {}", path.display());
    Ok(Instant::now())
}

/// The settings to record with, which are the stored ones unless the encoder
/// they name cannot be opened here.
///
/// The default is `Nvenc`, and it is a good default — but it is wrong on
/// every machine without an NVIDIA GPU, which is where the first Record press
/// would otherwise fail with nothing on screen but an error. So the encoder
/// falls through to the best one that did open.
///
/// The stored choice is not rewritten. Someone who picked NVENC on the
/// machine that has it should still find it selected after recording once on
/// a laptop that does not, rather than having their setting quietly replaced
/// by whatever that laptop could manage.
fn usable_settings(
    backend: &Backend,
    audio_codecs: &[crate::settings::RecordingAudioCodec],
    settings: &crate::settings::RecordingSettings,
) -> crate::settings::RecordingSettings {
    let mut settings = settings.clone();

    // The audio codec first, and on its own terms: a build without libopus
    // should still record, with sound, on the codec it does have — and so
    // should a mix at a rate libopus cannot take.
    if !audio_codecs.contains(&settings.audio_codec)
        && let Some(codec) = crate::settings::RecordingAudioCodec::best_of(audio_codecs)
    {
        eprintln!(
            "{} cannot be opened here; recording audio with {} instead",
            settings.audio_codec.label(),
            codec.label()
        );
        settings.audio_codec = codec;
    }

    let settings = &settings;
    let available = backend.available_encoders();
    if available.contains(&settings.encoder) {
        return settings.clone();
    }
    let Some(encoder) = crate::settings::RecordingEncoder::best_of(available) else {
        // Nothing opened at all. Recording with what was asked for will fail
        // and say why, which is better than failing with a substitution the
        // caller did not make.
        return settings.clone();
    };
    eprintln!(
        "{} cannot be opened here; recording with {} instead",
        settings.encoder.label(),
        encoder.label()
    );
    crate::settings::RecordingSettings {
        encoder,
        ..settings.clone()
    }
}

/// One line naming everything that went wrong, not only the outermost of it.
///
/// `media-pp`'s errors carry their cause as a `source`, and the outer message
/// is often the general shape — "could not open the encoder" — while the one
/// a person can act on is underneath: no NVENC on this adapter, a directory
/// that cannot be written. So the chain is walked and joined.
///
/// A cause already quoted by its parent is not repeated: `thiserror`'s
/// `#[error("... {0}")]` embeds one, and appending it again would say the
/// same thing twice in the one line a status bar has.
pub(in crate::engine) fn describe(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut next = error.source();
    while let Some(cause) = next {
        let message = cause.to_string();
        if !text.contains(&message) {
            text.push_str(": ");
            text.push_str(&message);
        }
        next = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the error chains `media-pp` and FFmpeg produce, so
    /// [`describe`] can be tested against the shapes it exists for rather
    /// than against whichever one this machine happens to fail with.
    #[derive(Debug)]
    struct Layer {
        message: &'static str,
        cause: Option<Box<Layer>>,
    }

    impl std::fmt::Display for Layer {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(self.message)
        }
    }

    impl std::error::Error for Layer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.cause
                .as_deref()
                .map(|cause| cause as &(dyn std::error::Error + 'static))
        }
    }

    fn chain(messages: &[&'static str]) -> Layer {
        let mut layers = messages.iter().rev();
        let mut error = Layer {
            message: layers.next().expect("a chain needs a layer"),
            cause: None,
        };
        for message in layers {
            error = Layer {
                message,
                cause: Some(Box::new(error)),
            };
        }
        error
    }

    /// The outermost message is the shape of the failure; the one a person
    /// can act on is usually underneath it.
    #[test]
    fn a_failure_is_described_by_its_whole_chain() {
        let error = chain(&[
            "could not open the encoder",
            "avcodec_open2 failed",
            "no NVENC capable devices found",
        ]);

        assert_eq!(
            describe(&error),
            "could not open the encoder: avcodec_open2 failed: no NVENC capable devices found"
        );
    }

    /// `thiserror`'s `#[error("... {0}")]` already embeds its source, and a
    /// status bar has one line — saying it twice would spend half of that
    /// line repeating itself.
    #[test]
    fn a_cause_its_parent_already_quotes_is_not_repeated() {
        let error = chain(&[
            "opening the file failed: access is denied",
            "access is denied",
        ]);

        assert_eq!(
            describe(&error),
            "opening the file failed: access is denied"
        );
    }
}
