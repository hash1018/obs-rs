//! What the engine loop tells the interface, and what it notices on its own.
//!
//! Two jobs that look different and are the same one: reading the state of
//! every open Source and acting on what has changed since the last pass.
//! One direction publishes — the docks draw from a snapshot this writes, and
//! never from the engine's own map, which belongs to one thread. The other
//! notices: a window that has closed, a file that has reached its end, a
//! stream that has dropped. Nothing tells the engine any of those; a
//! pipeline simply stops, and these are what look.
//!
//! Split out of `super` because they are the part of that loop with no
//! bearing on anything else in it. Nothing here opens, closes, or draws
//! anything; each is a pass over what is already open, and each is called
//! from exactly one place.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::domain::{SceneItemId, SourceKind};
use crate::snapshots::{SourceStatus, SourcesSnapshot};

use super::backend::Backend;
use super::source;
use super::{Published, SourceState, needs_asking};

/// Tells the UI which Sources are not producing a picture, and why.
///
/// Stored only on a change: the Sources list reads this on every pass, and
/// replacing the map each time would hand it a new allocation a second for an
/// answer that is almost always the same one — usually empty.
pub(super) fn publish_source_status(
    published: &Published,
    open: &HashMap<SceneItemId, SourceState>,
) {
    let status: HashMap<SceneItemId, SourceStatus> = open
        .iter()
        .filter_map(|(id, state)| {
            let status = match state {
                SourceState::Open(_) => return None,
                // Nothing has gone wrong yet. Reporting a Source as not
                // showing while it is still being opened would put a badge
                // beside every item for as long as its capture takes to
                // start, which on most of them is one frame.
                SourceState::Opening => return None,
                SourceState::Ended => SourceStatus::Ended,
                // Never opened, which "disconnected" would not be true of.
                SourceState::Failed(reason) => SourceStatus::Failed(Arc::clone(reason)),
                // Missing and Disconnected are one thing to a reader: it was
                // there, or will be, and is not showing now. Which of the two
                // it is decides what this side does next — look again, or
                // wait to be asked — not what the list says.
                SourceState::Missing { reason, .. } | SourceState::Disconnected(reason) => {
                    SourceStatus::Disconnected(reason.clone())
                }
            };
            Some((*id, status))
        })
        .collect();
    let unchanged = match published.source_status.load_full() {
        Some(current) => *current == status,
        // Nothing published yet, which an empty map says as well as `None`
        // does — and the first pass has nothing to correct.
        None => status.is_empty(),
    };
    if unchanged {
        return;
    }
    published.source_status.store(Some(Arc::new(status)));
}

/// Hands the UI the counters each playing media file writes to.
///
/// Published beside the status map and on the same occasions, because it
/// moves for the same reasons: a Source opening or closing is the only thing
/// that adds or removes a set of counters. What is inside one changes every
/// buffer and is never republished — that is the whole point of an atomic
/// here.
///
/// Compared by pointer rather than by key, because a Source that closed and
/// opened again keeps its SceneItem's id but gets new counters. Matching on
/// ids alone would leave the dock reading the dead ones, and its meter would
/// sit at whatever the previous Source last measured.
pub(super) fn publish_media_meters(
    published: &Published,
    open: &HashMap<SceneItemId, SourceState>,
) {
    let meters: HashMap<SceneItemId, Arc<source::MediaMeters>> = open
        .iter()
        .filter_map(|(id, state)| {
            let SourceState::Open(source) = state else {
                return None;
            };
            Some((*id, Arc::clone(&source.media_file.as_ref()?.meters)))
        })
        .collect();
    let unchanged = match published.media_meters.load_full() {
        Some(current) => {
            current.len() == meters.len()
                && meters.iter().all(|(id, held)| {
                    current
                        .get(id)
                        .is_some_and(|current| Arc::ptr_eq(current, held))
                })
        }
        None => meters.is_empty(),
    };
    if unchanged {
        return;
    }
    published.media_meters.store(Some(Arc::new(meters)));
}

/// Puts a live source that stopped arriving back where it can be reopened.
///
/// `RtspSource` does not reconnect: a read that fails ends it with an error
/// and the pipeline finishes, which — since a pipeline is one-shot — means
/// coming back is a new one. Nothing tells the engine that, so it asks, the
/// same way it asks about a window that closed.
///
/// Where the Source may reconnect by itself this is `Missing` and
/// `retry_missing` opens it again after its own interval; where it may not it
/// is `Disconnected` and waits to be asked.
pub(super) fn notice_dropped_streams(
    backend: &Backend,
    open: &mut HashMap<SceneItemId, SourceState>,
    snapshot: &SourcesSnapshot,
) {
    for item in &snapshot.items {
        if !matches!(item.kind, SourceKind::Rtsp | SourceKind::VideoCapture) {
            continue;
        }
        let Some(SourceState::Open(source)) = open.get(&item.id) else {
            continue;
        };
        if !source.source.ended() {
            continue;
        }
        eprintln!("\"{}\": the stream stopped arriving", item.name);
        source.source.stop();
        backend.remove_source(&source.name);
        // What ended it is in media-pp's log by now; what this can say is
        // which way it ended.
        let reason = match item.kind {
            SourceKind::VideoCapture => "the camera stopped sending pictures",
            _ => "the stream stopped arriving",
        };
        open.insert(item.id, gone(item, reason));
    }
}

/// Where a Source that stopped by itself goes: back to be looked for, or to
/// wait to be asked — see `needs_asking` — with why it stopped.
fn gone(item: &crate::snapshots::SceneItemSnapshot, reason: &str) -> SourceState {
    let reason = Some(Arc::from(reason));
    if needs_asking(item) {
        SourceState::Disconnected(reason)
    } else {
        SourceState::Missing {
            since: Instant::now(),
            reason,
        }
    }
}

/// Puts a Window Capture whose window has since closed back to `Missing`.
///
/// A window closing ends the capture: the Source stops, the compositor drops
/// the layer, and the pipeline is finished. Nothing tells the engine that, so
/// it asks — and once it knows, the Source is stopped and forgotten so that
/// `retry_missing` can open it again when the window comes back. Only a
/// Window Capture is asked: it is the one kind whose target is expected to
/// come and go.
pub(super) fn notice_closed_windows(
    backend: &Backend,
    open: &mut HashMap<SceneItemId, SourceState>,
    snapshot: &SourcesSnapshot,
) {
    for item in &snapshot.items {
        if item.kind != SourceKind::WindowCapture {
            continue;
        }
        let Some(SourceState::Open(source)) = open.get(&item.id) else {
            continue;
        };
        if !source.source.ended() {
            continue;
        }
        source.source.stop();
        backend.remove_source(&source.name);
        open.insert(item.id, gone(item, "the window was closed"));
    }
}

/// Notices a media file that has played to its end.
///
/// Nothing tells the engine that either, so it asks, the same way it asks
/// about a closed window — and only about media files, because every other
/// kind here is live and its pipeline ending means something went wrong
/// rather than something finished.
///
/// The Source is stopped once noticed. That is not tidying: `Stop` is what
/// takes its input off the audio mixer, which an `Eos` alone leaves
/// registered and silent, so a finished file would otherwise keep a channel
/// in the Audio Mixer dock for as long as its SceneItem existed.
///
/// It is not reopened *here*. Playing once is what a file that is not
/// looping was asked to do, and starting it again by itself would make the
/// setting meaningless. Someone pressing play is a different thing: the
/// Properties dock's transport asks for `ReopenSource`, which is the same
/// request the Sources dock makes for a disconnected capture.
pub(super) fn notice_ended_media(
    backend: &Backend,
    open: &mut HashMap<SceneItemId, SourceState>,
    snapshot: &SourcesSnapshot,
) {
    for item in &snapshot.items {
        if item.kind != SourceKind::MediaFile {
            continue;
        }
        let Some(SourceState::Open(source)) = open.get(&item.id) else {
            continue;
        };
        if !source.source.ended() {
            continue;
        }
        source.source.stop();
        backend.remove_source(&source.name);
        open.insert(item.id, SourceState::Ended);
    }
}
