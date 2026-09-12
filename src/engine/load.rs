//! How hard an output is making everything else wait.
//!
//! A recording or a broadcast is two branches off two `Tee`s, and each
//! begins with a `Queue` that blocks rather than drops — a frame missing
//! from a file is worse than a frame arriving late, so when the encoder or
//! the disk or the network falls behind, the queue fills and the thread
//! feeding it waits.
//!
//! # Filling comes before blocking
//!
//! Waiting is not the first sign, though — it is the second. A queue only
//! blocks once it is *full*, so an encoder that has fallen behind fills its
//! queue for some seconds before anything waits on it at all. Measured: with
//! the software encoder and the CPU held down, `record-queue` sat at four
//! and five of its eight while `blocked` stayed at zero.
//!
//! So what is reported is how close an output is to not keeping up — its
//! fullest queue, as a share of that queue. Blocking then means it got all
//! the way there, which is why any of it reads as full whatever the sample
//! happened to catch.
//!
//! Past that point the wait grows until it exceeds the queue's timeout and
//! the branch errors, which is where `super::trouble` picks it up. By then
//! the recording has already stopped. This is the same news earlier.
//!
//! # Which queues count
//!
//! Only an output's. This application has queues that drop on purpose and
//! are perfectly healthy doing it — the Preview's is one buffer deep with
//! `DropNewest`, because one stale repaint costs nothing and holding the
//! compositor costs everything. Counting those would light this up
//! constantly and say nothing.
//!
//! So the queues that count are the ones named after an output, which is
//! what `OutputKind::prefix` already decides — the same naming
//! `super::trouble` reads, and for the same reason.
//!
//! # Two pipelines again
//!
//! An output's video branch is on the compositor's pipeline and its audio
//! on the audio thread's, so the load has to be read from both and added
//! up. The engine loop reads the first; the audio thread publishes the
//! second, for the reason it forwards its own failures — `BusReceiver` and
//! `Pipeline` both live where they were made.

use std::time::Duration;

use media_pp::stats::PipelineStats;

use super::output::OutputKind;

/// What an output's queues have cost, as running totals.
///
/// Totals rather than rates, because that is what `media-pp` counts and
/// because a rate needs a window someone has to choose. Two readings and
/// the time between them give the rate over exactly that time — see
/// [`Load::since`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Load {
    /// How long upstream threads have spent waiting for room. Cumulative.
    pub blocked: Duration,
    /// Buffers an output queue threw away. Should stay zero: an output's
    /// queues block rather than drop, so anything here means one was built
    /// with a policy it should not have been. Cumulative.
    pub dropped: u64,
    /// How full the fullest output queue was, as a share of itself, at the
    /// moment this was read. Not cumulative — a gauge, and the only reading
    /// here that is.
    pub fullest: f32,
}

impl Load {
    /// What one pipeline's output queues have cost so far.
    pub fn read(stats: &PipelineStats) -> Self {
        stats
            .elements
            .iter()
            .filter(|element| is_an_outputs(&element.name))
            .filter_map(|element| element.queue)
            .fold(Self::default(), |total, queue| Self {
                blocked: total.blocked + queue.blocked,
                dropped: total.dropped + queue.dropped,
                fullest: total.fullest.max(fullness(queue)),
            })
    }

    pub fn merge(self, other: Self) -> Self {
        Self {
            blocked: self.blocked + other.blocked,
            dropped: self.dropped + other.dropped,
            fullest: self.fullest.max(other.fullest),
        }
    }

    /// What happened between an earlier reading and this one.
    ///
    /// Saturating, because a reading is not always larger than the one
    /// before it: an output that stopped takes its queues out of the graph,
    /// and the totals that come back are then smaller. That is a reset
    /// rather than negative time.
    pub fn since(self, earlier: Self) -> Self {
        Self {
            blocked: self.blocked.saturating_sub(earlier.blocked),
            dropped: self.dropped.saturating_sub(earlier.dropped),
            // A gauge is already "now"; there is nothing to subtract.
            fullest: self.fullest,
        }
    }

    /// How close the outputs are to not keeping up, from nothing to full.
    ///
    /// The fullest queue, except that any blocking at all in the interval
    /// reads as full: blocking only happens at capacity, so it happened —
    /// whatever the queue's level was at the instant this was sampled.
    pub fn pressure(self) -> f32 {
        if self.blocked.is_zero() {
            self.fullest.clamp(0.0, 1.0)
        } else {
            1.0
        }
    }
}

/// How full one queue is, as a share of itself.
fn fullness(queue: media_pp::stats::QueueStats) -> f32 {
    if queue.capacity == 0 {
        return 0.0;
    }
    queue.len as f32 / queue.capacity as f32
}

/// Whether a queue by this name belongs to an output rather than to
/// something that drops on purpose.
fn is_an_outputs(name: &str) -> bool {
    name.starts_with(OutputKind::Recording.prefix())
        || name.starts_with(OutputKind::Broadcast.prefix())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Preview's queue drops a buffer on most frames and is right to.
    /// Counting it would leave this reading high whenever anything was on
    /// screen, which is to say always.
    #[test]
    fn only_an_outputs_queues_are_counted() {
        assert!(is_an_outputs("record-queue"));
        assert!(is_an_outputs("record-audio-queue"));
        assert!(is_an_outputs("stream-queue"));
        assert!(is_an_outputs("stream-audio-queue"));

        assert!(!is_an_outputs("preview-queue"));
        assert!(!is_an_outputs("monitor-queue"));
        assert!(!is_an_outputs("camera"));
    }

    #[test]
    fn a_cumulative_reading_is_the_difference_between_two() {
        let earlier = Load {
            blocked: Duration::from_millis(120),
            dropped: 2,
            fullest: 0.9,
        };
        let now = Load {
            blocked: Duration::from_millis(200),
            dropped: 5,
            fullest: 0.25,
        };
        let over = now.since(earlier);
        assert_eq!(over.blocked, Duration::from_millis(80));
        assert_eq!(over.dropped, 3);
        // The gauge is not a total and is carried through as it stands.
        assert_eq!(over.fullest, 0.25);
    }

    /// The reading this is built on. A queue fills for seconds before it
    /// blocks at all — measured at four and five of eight with `blocked`
    /// still zero — so a number made only of blocking stays dark through
    /// exactly the stretch it exists to show.
    #[test]
    fn pressure_rises_while_a_queue_fills_rather_than_waiting_for_it_to_block() {
        let filling = Load {
            blocked: Duration::ZERO,
            dropped: 0,
            fullest: 0.5,
        };
        assert_eq!(filling.pressure(), 0.5);
    }

    /// And any blocking reads as full, whatever the sample caught: a queue
    /// only blocks at capacity, so it was full a moment ago even if it has
    /// drained by the time this looked.
    #[test]
    fn any_blocking_reads_as_full_however_empty_the_queue_looks_now() {
        let blocked = Load {
            blocked: Duration::from_micros(1),
            dropped: 0,
            fullest: 0.0,
        };
        assert_eq!(blocked.pressure(), 1.0);
    }

    /// An output that stopped takes its queues with it, so the next total
    /// is smaller than the one before. That is a reset, not a negative.
    #[test]
    fn a_total_that_went_backwards_reads_as_nothing_rather_than_wrapping() {
        let earlier = Load {
            blocked: Duration::from_secs(9),
            dropped: 9,
            fullest: 1.0,
        };
        let over = Load::default().since(earlier);
        assert_eq!(over.blocked, Duration::ZERO);
        assert_eq!(over.dropped, 0);
        assert_eq!(over.pressure(), 0.0);
    }
}
