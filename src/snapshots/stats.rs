//! What the pipelines are doing, as the Stats dock draws it.
//!
//! One row per thing a person would act on, not one per element. The graph
//! has dozens of elements and almost all of them are plumbing; what someone
//! looking at this wants to know is which of a small number of named things
//! is behind, and by how much.
//!
//! Read once a second on the engine's own tick, because that is where the
//! pipelines are and because the numbers underneath are running totals — two
//! readings and the time between them are what make a rate, and only the
//! loop knows when it took them.

use std::sync::Arc;
use std::time::Duration;

/// Everything the Stats dock shows, as of one reading.
#[derive(Debug, Clone, Default)]
pub struct StatsSnapshot {
    /// The compositor, and the outputs hanging off it.
    pub rows: Arc<Vec<StatsRow>>,
    /// How long the reading covers, for turning totals into rates.
    pub interval: Duration,
}

/// What one named thing in the graph is doing.
#[derive(Debug, Clone, PartialEq)]
pub struct StatsRow {
    /// What it is, so the dock can name it in the reader's own language
    /// rather than showing an element's internal name.
    pub subject: Subject,
    /// Buffers it took in the interval — frames for video, packets or
    /// sample blocks for audio. `None` where the thing has no throughput of
    /// its own to report.
    pub buffers: Option<u64>,
    /// The share of the interval its thread spent inside it, where that
    /// means anything.
    ///
    /// Includes every stage after it on the same thread, up to the next
    /// queue — a chain runs as nested calls — so where it is reported it is
    /// the cost of a whole branch, which is what someone deciding what to
    /// turn off wants.
    ///
    /// `None` for anything with a pacer below it, which is every Source:
    /// a pacer holds each frame until its timestamp comes due, and that
    /// wait is spent *inside* the call, so it is counted as time busy.
    /// Measured on a 30 fps clip: its decoded-frame queue read 15.0 s busy
    /// out of 15 s. Reported, that is a Source at 100% — and the advice it
    /// gives is to turn off the cheapest thing running.
    pub busy: Option<f32>,
    /// How full its queue is, as a share of that queue — only for an
    /// output, whose queues fill because something downstream is behind.
    ///
    /// `None` elsewhere, and for the same pacer: a Source's decoded-frame
    /// queues sit at capacity whenever it is healthy, holding frames until
    /// they are due. Measured on the same clip: 8 of 8 and 64 of 64, with
    /// 14 s blocked, while nothing at all was wrong.
    pub queue: Option<f32>,
    /// How long since it last handled anything, where that is known. What
    /// says a camera stopped rather than went quiet.
    pub idle_for: Option<Duration>,
    /// Calls into it that failed, over the whole run rather than the
    /// interval: a failure an hour ago still explains a file that is short.
    pub errors: u64,
}

/// What an element can say about the subject it belongs to.
///
/// A subject is several elements and they do not hold the same news: the
/// muxer at the end of a recording knows how much reached the file but
/// costs nothing, while the queue at its head knows what the encoding cost
/// and has already counted the same frames. So each is asked for only what
/// it alone has, and the row is what they say together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Its count is how much the subject handled.
    Throughput,
    /// Its thread's time and its queue are what the subject costs.
    Cost,
}

/// Which of the things worth naming a row belongs to.
///
/// An enum rather than a string, so the dock can translate it and so the
/// engine is not deciding what the interface says.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Subject {
    /// The compositor itself — what drawing the Canvas costs, before
    /// anything is done with the result.
    Compositor,
    /// A recording, from its own queue down to its muxer.
    Recording,
    /// A broadcast, likewise.
    Broadcast,
    /// One Source, by the name the Sources dock shows it under.
    Source(String),
}
