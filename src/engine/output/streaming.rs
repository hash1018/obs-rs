//! Starting a broadcast: the same two branches a recording is, ending at a
//! server instead of a file.
//!
//! Everything up to the muxer is [`Output`]'s and is shared verbatim — the
//! compositor's `Tee` through a video encoder, the mixer's through an audio
//! one. What is here is the part that differs: opening the connection.
//!
//! # Why this happens on a thread
//!
//! `FileMuxer::create` fails only if the path is unwritable, and it fails at
//! once. `RtmpMuxer::create` performs the RTMP handshake — a DNS lookup, a
//! TCP connection and a reply, up to ten seconds of waiting.
//!
//! Ten seconds once, on a press the user just made, would be tolerable. But
//! a broadcast that drops is retried, and a server that is down refuses every
//! attempt: on the engine loop that is ten seconds of frozen Preview every
//! few seconds, for as long as the outage lasts. So connecting is done where
//! a Source is opened, on a thread that answers back through the loop's own
//! queue — see `SourceOpener`, whose shape this follows exactly.
//!
//! # What is never shown
//!
//! The publish URL carries the stream key, so it is made at the moment the
//! connection opens and dropped straight after — see
//! [`StreamingSettings::publish_url`](crate::settings::StreamingSettings::publish_url).
//! Nothing here logs it, and `media-pp` reports through
//! `RtmpMuxer::redacted_url` for the same reason.

use std::sync::Arc;

use media_pp::element::Sink;
use media_pp::elements::{MixerHandle, RtmpMuxer, TeeHandle};

use crate::settings::StreamingSettings;

use super::super::backend::{Backend, BackendError};
use super::{Output, OutputKind, TrackDef, Tracks};

/// One broadcast to open, as it was asked for.
pub(in crate::engine) struct BroadcastRequest {
    pub(in crate::engine) settings: StreamingSettings,
    pub(in crate::engine) fps: u32,
    /// Cloned rather than borrowed: the mixer outlives one connect, and the
    /// thread cannot hold a reference into the engine loop's own state.
    pub(in crate::engine) mixer: Option<(TeeHandle, MixerHandle)>,
}

/// Connects and starts both tracks publishing, on whichever thread calls.
///
/// Never the engine loop: this blocks for the handshake. See this module's
/// own docs, and `BroadcastOpener`, which is what calls it.
pub(in crate::engine) fn connect(
    backend: &Arc<Backend>,
    request: BroadcastRequest,
) -> Result<Output, BackendError> {
    let mut settings = request.settings;
    if !settings.is_addressable() {
        return Err("set a server address and a stream key first".into());
    }

    // Probed against what this machine can actually open, rather than
    // trusted from the settings: a stored encoder or codec that will not
    // open here should broadcast with something else instead of refusing.
    let audio_codecs = super::available_audio_codecs(
        request
            .mixer
            .as_ref()
            .and_then(|(_, handle)| handle.mix_format())
            .unwrap_or(super::super::audio::DEFAULT_MIX_FORMAT),
    );
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
        request.mixer.as_ref(),
        OutputKind::Broadcast,
        request.fps,
        &settings.encoding(backend.size),
        |tracks| open_rtmp_muxer(&settings.publish_url(), tracks),
    )?;
    // The address without its key, which is the only form of it that leaves
    // this function.
    println!("broadcasting to {}", redacted(&settings.server));
    Ok(running)
}

/// Connects, declares the tracks, and writes the FLV header.
///
/// `url` carries the stream key and is borrowed for exactly this call.
fn open_rtmp_muxer(
    url: &str,
    tracks: Tracks<TrackDef>,
) -> Result<Tracks<Box<dyn Sink>>, BackendError> {
    let mut muxer = RtmpMuxer::create(url)?;
    let added =
        tracks.try_map(|track| muxer.add_stream(track.name, track.parameters, track.time_base))?;
    let mut sinks = muxer.open()?;
    Ok(added.try_map(|track| sinks.take(track))?)
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
