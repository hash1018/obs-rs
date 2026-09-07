//! What went wrong somewhere the caller had already gone.
//!
//! Inside one thread a failing element propagates with `?` and whoever asked
//! learns about it. Once a buffer has crossed a `Queue` there is no longer
//! anyone to return to, so `media-pp` posts a [`BusEvent`] instead and the
//! worker keeps running — which is the right thing for it to do and the
//! reason a failure can otherwise go entirely unremarked.
//!
//! That is the state this application was in until now. A broadcast whose
//! connection dropped went on being reported as live, because nothing looked;
//! a recording whose disk filled stopped growing in silence. Both are on the
//! bus, and neither was read.
//!
//! # Two buses, because there is no third
//!
//! Neither a recording nor a broadcast has a pipeline of its own. Each is two
//! branches: video off the compositor's `Tee`, which lives in the backend's
//! own pipeline, and audio off the mixer's, which lives in the audio thread's.
//! So a failure surfaces on whichever of those two the branch belongs to, and
//! both have to be read.
//!
//! The video one is read from the engine loop, which already holds the
//! backend. The audio one cannot be — the mix pipeline belongs to the audio
//! thread and `BusReceiver` is not `Clone`, so there is exactly one place it
//! can be drained from. That thread drains it and forwards what matters; see
//! `audio::AudioEngine::poll_bus`.

use media_pp::bus::{BusEvent, BusReceiver};

use super::output::OutputKind;

/// One thing that went wrong, in the terms the engine acts in rather than the
/// terms the graph reports it in.
///
/// Deliberately not "an element failed". What the loop needs to decide is
/// whether a *recording* or a *broadcast* is over, and those are what a muxer
/// failing means — the muxer is where a whole output ends, so its failure is
/// the output's failure and nothing smaller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trouble {
    /// The broadcast stopped being written. Almost always the connection:
    /// both RTMP tracks share one, so either failing means it is gone.
    Broadcast(String),
    /// The recording stopped being written — a full disk, a removed drive.
    Recording(String),
}

/// Reads everything waiting on one bus and answers what the engine should act
/// on.
///
/// Non-blocking, and drains rather than sampling: `BusReceiver::iter` blocks
/// until every sender is dropped, which for a pipeline that is still running
/// is never. The queue is unbounded, so anything left unread accumulates for
/// the life of the process — this is called from the engine's idle tick, a
/// hundred times a second, and is empty on almost all of them.
///
/// Everything that is not a muxer failing is logged and dropped. A dropped
/// buffer says the encoder is behind, which is worth having in the log and is
/// not worth a line in the status bar: it can happen many times a second, and
/// a report that appears that often is one nobody reads.
pub(in crate::engine) fn drain(bus: &BusReceiver, source: &str) -> Vec<Trouble> {
    let mut troubles = Vec::new();
    while let Some(message) = bus.try_recv_message() {
        match message.event {
            BusEvent::Error { name, error, .. } => {
                let reason = format!("{name}: {error}");
                eprintln!("{source}: {reason}");
                // Attributed by name, not by element type. A muxer that
                // fails is not what reaches the bus: the `Queue` in front of
                // it catches what its sink returned, drops that buffer,
                // keeps its worker alive — which is right — and posts the
                // error **as its own**, hardcoded to `ElementType::Queue`
                // with the queue's own name. The failing element's identity
                // is in `media-pp`'s log and nowhere else.
                //
                // So what says which output has stopped is what its elements
                // are called, which is why they are named after
                // `OutputKind` and why that lives beside this.
                if name.starts_with(OutputKind::Broadcast.prefix()) {
                    troubles.push(Trouble::Broadcast(reason));
                } else if name.starts_with(OutputKind::Recording.prefix()) {
                    troubles.push(Trouble::Recording(reason));
                }
                // Anything else is a Source or a stage inside one. It has
                // been logged, by the line above and by `media-pp` itself;
                // what it is not is a reason to declare an output over.
            }
            // Not a failure: a branch reached the end of what it was given.
            // A recording's own tracks report this as they finish, which is
            // the ordinary way one ends.
            BusEvent::Eos { .. } | BusEvent::Seeked { .. } => {}
            BusEvent::Dropped { element_type, name } => {
                eprintln!("{source}: {element_type:?}({name}) dropped a buffer");
            }
            // `BusEvent` is `#[non_exhaustive]`: a new kind of report must
            // not stop this compiling, and the ones this acts on are named
            // above rather than left to a fallthrough.
            _ => {}
        }
    }
    troubles
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_pp::bus::Bus;
    use media_pp::pp_log::PpLog;

    fn post(bus: &Bus, name: &str, message: &str) {
        use media_pp::element::ElementType;
        bus.post(
            &PpLog::new("test", name, None),
            BusEvent::Error {
                // What a queue really posts: its own type, whatever failed
                // downstream of it. See `drain` on why that is the whole
                // reason attribution is by name.
                element_type: ElementType::Queue,
                name: name.into(),
                error: media_pp::Error::Other(message.to_owned()),
            },
        );
    }

    /// Which output has stopped is read off the element's name, because the
    /// bus carries no other trace of it.
    ///
    /// The queue names come from `OutputKind::prefix`, so this is also what
    /// catches the two drifting apart — rename an output's elements without
    /// telling this module, and a dropped broadcast stops being noticed.
    #[test]
    fn an_output_is_recognized_by_what_its_elements_are_named() {
        let (bus, receiver) = Bus::new();
        post(&bus, "stream-queue", "connection reset");
        post(&bus, "record-audio-queue", "no space left");
        // A Source's own queue, which is neither and must be left alone: a
        // camera dropping a buffer is not a recording ending.
        post(&bus, "camera", "one frame");

        let troubles = drain(&receiver, "test");
        assert_eq!(troubles.len(), 2, "got {troubles:?}");
        assert!(
            matches!(&troubles[0], Trouble::Broadcast(reason) if reason.contains("connection reset"))
        );
        assert!(
            matches!(&troubles[1], Trouble::Recording(reason) if reason.contains("no space left"))
        );
    }

    /// Draining empties the queue rather than sampling it. What is left
    /// unread is kept for the life of the process — the channel is
    /// unbounded — so a reader that took one message per pass would fall
    /// further behind on every failure, which is exactly when it matters.
    #[test]
    fn draining_takes_everything_waiting() {
        let (bus, receiver) = Bus::new();
        for _ in 0..5 {
            post(&bus, "stream-queue", "gone");
        }
        assert_eq!(drain(&receiver, "test").len(), 5);
        assert!(
            drain(&receiver, "test").is_empty(),
            "the queue was not emptied"
        );
    }

    /// And it never blocks. `BusReceiver::iter` waits for every sender to be
    /// dropped, which for a pipeline that is still running never happens —
    /// reaching for it here would hang the engine loop for good.
    #[test]
    fn draining_an_idle_bus_returns_at_once() {
        let (bus, receiver) = Bus::new();
        assert!(drain(&receiver, "test").is_empty());
        // With a live sender still held, which is the state a running
        // pipeline is always in.
        drop(bus);
        assert!(drain(&receiver, "test").is_empty());
    }
}
