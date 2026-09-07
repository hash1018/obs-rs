//! Starting a broadcast: the same two branches a recording is, ending at a
//! server instead of a file.
//!
//! Everything up to the muxer is [`Output`]'s and is shared verbatim — the
//! compositor's `Tee` through a video encoder, the mixer's through an audio
//! one. What is here is the part that differs: opening the connection, and
//! the two ways that can go wrong that a file has no equivalent of.
//!
//! # A file opens, a broadcast negotiates
//!
//! `FileMuxer::create` fails only if the path is unwritable, and it fails at
//! once. `RtmpMuxer::create` performs the RTMP handshake, which means a DNS
//! lookup, a TCP connection and a reply — up to ten seconds of waiting, on
//! whichever thread asked. That thread is the engine loop, which is also
//! what moves every layer and reads every command.
//!
//! It is done there anyway, and deliberately. The alternative is starting a
//! broadcast on a thread of its own and reporting back, which is what
//! `SourceOpener` does for a camera — and a camera is worth it because a
//! Scene may hold six of them and one dialog must not stop the others. A
//! broadcast is one thing, started by one press, whose whole purpose is to
//! be running a moment later. Freezing the picture for the length of a
//! handshake is visible; the machinery to avoid it is not free, and what it
//! would buy is a Preview that keeps moving while the user waits for the
//! very thing they just asked for.
//!
//! # What is never shown
//!
//! The publish URL carries the stream key, so it is made at the moment the
//! connection opens and dropped straight after — see
//! [`StreamingSettings::publish_url`](crate::settings::StreamingSettings::publish_url).
//! Nothing here logs it, and `media-pp` reports through
//! `RtmpMuxer::redacted_url` for the same reason.

use std::time::Instant;

use media_pp::{element::Sink, elements::RtmpMuxer};

use super::super::backend::{Backend, BackendError};
use super::{Output, OutputState, TrackDef};

/// Opens the broadcast this state's settings describe, and starts both
/// tracks publishing to it.
///
/// Answers with the instant it began, which is what the clock in the status
/// bar counts from — the same shape `start_recording` has, for the same
/// reason: the loop publishes one instant and everything else derives from
/// it, so nothing can disagree about whether a broadcast exists.
pub(in crate::engine) fn start_streaming(
    backend: &Backend,
    state: &mut OutputState,
) -> Result<Instant, BackendError> {
    if state.broadcast.is_some() {
        return Err("a broadcast is already running".into());
    }
    let settings = state.streaming.clone();
    if !settings.is_addressable() {
        return Err("set a server address and a stream key first".into());
    }

    // Probed here rather than taken from the list published at startup, for
    // the reason `start_recording` gives: the mix format can have moved
    // since, and which encoders open depends on it.
    let audio_codecs = super::available_audio_codecs(state.mix_format());
    let mut settings = settings;
    if !audio_codecs.contains(&settings.audio_codec) {
        let Some(fallback) = audio_codecs.first().copied() else {
            return Err("this build has no audio codec a broadcast can carry".into());
        };
        eprintln!(
            "{:?} cannot be opened here; broadcasting with {fallback:?}",
            settings.audio_codec
        );
        settings.audio_codec = fallback;
    }
    if !backend.available_encoders().contains(&settings.encoder) {
        let Some(fallback) = backend.available_encoders().first().copied() else {
            return Err("this machine has no encoder a broadcast can use".into());
        };
        eprintln!(
            "{:?} cannot be opened here; broadcasting with {fallback:?}",
            settings.encoder
        );
        settings.encoder = fallback;
    }

    let running = Output::start(
        backend,
        state.mixer.as_ref(),
        backend.frame_rate(),
        &settings.encoding(backend.size),
        |tracks| open_rtmp_muxer(&settings.publish_url(), tracks),
    )?;
    state.broadcast = Some(running);
    // The address without its key, which is the only form of it that leaves
    // this function.
    println!("broadcasting to {}", redacted(&settings.server));
    Ok(Instant::now())
}

/// Connects, declares the tracks, and writes the FLV header.
///
/// `url` carries the stream key and is borrowed for exactly this call.
fn open_rtmp_muxer(url: &str, tracks: Vec<TrackDef>) -> Result<Vec<Box<dyn Sink>>, BackendError> {
    let mut muxer = RtmpMuxer::create(url)?;
    for track in tracks {
        muxer.add_stream(track.name, track.parameters, track.time_base)?;
    }
    Ok(muxer.open()?)
}

/// A server address with anything past the application path removed.
///
/// The server half of the settings should hold `rtmp://host/app` and no key
/// — the two are stored apart. But people paste whole URLs into the first
/// field they see, and a key pasted there would otherwise reach the log. So
/// what is kept is the scheme, the host, and **one** path segment; anything
/// after that is where a key would be.
///
/// That does mean a service whose application path is two segments deep logs
/// the second as `…`. Losing half an address in a log line is the cheaper
/// mistake of the two.
fn redacted(server: &str) -> String {
    let trimmed = server.trim().trim_end_matches('/');
    let Some((scheme, rest)) = trimmed.split_once("//") else {
        return trimmed.to_owned();
    };
    let mut segments = rest.splitn(3, '/');
    let host = segments.next().unwrap_or_default();
    let kept = match segments.next() {
        Some(application) => format!("{scheme}//{host}/{application}"),
        None => return trimmed.to_owned(),
    };
    match segments.next() {
        Some(_) => format!("{kept}/…"),
        None => kept,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key pasted into the server field must not reach the log — which is
    /// the only thing this function is for.
    #[test]
    fn a_server_address_is_logged_without_its_last_segment() {
        assert_eq!(
            redacted("rtmp://live.twitch.tv/app/live_123456_secret"),
            "rtmp://live.twitch.tv/app/…"
        );
        // A bare server, which is what the field is meant to hold, is not
        // shortened — there is nothing after the application path to hide.
        assert_eq!(
            redacted("rtmp://live.twitch.tv/app"),
            "rtmp://live.twitch.tv/app"
        );
        assert_eq!(
            redacted("rtmp://live.twitch.tv/app/"),
            "rtmp://live.twitch.tv/app"
        );
        // A host on its own keeps its host: cutting at the scheme's own
        // slashes would leave `rtmp:/…`, which names nothing.
        assert_eq!(redacted("rtmp://live.twitch.tv"), "rtmp://live.twitch.tv");
    }
}
