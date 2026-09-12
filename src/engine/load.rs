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

use media_pp::graph::ElementId;
use media_pp::stats::{ElementState, ElementStats, PipelineStats, TickStats};

use crate::snapshots::Role;

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

/// What one reading of one element says, as the fold reads it.
///
/// A view of [`ElementStats`] rather than the thing itself: it is the few
/// fields a row is made of, and unlike `ElementStats` it can be built — by
/// a caller on another platform's backend, and by a test, which is what
/// lets the folding below be checked without a running pipeline.
#[derive(Debug, Clone, Default)]
pub struct Element<'a> {
    /// The name its caller gave it.
    pub name: &'a str,
    /// Buffers handed to it.
    pub taken: u64,
    /// Buffers pushed through its busiest output port — the busiest rather
    /// than all of them added up, because a `Tee` pushes each buffer once
    /// per branch and what is wanted is buffers, not pushes.
    ///
    /// `None` where it has no output ports at all, which is how the end of
    /// a branch is known.
    pub pushed: Option<u64>,
    /// Time spent inside it, including every stage it feeds on the same
    /// thread.
    pub busy: Duration,
    /// How long since it last handled anything.
    pub idle_for: Option<Duration>,
    /// Calls into it that failed.
    pub errors: u64,
    /// Its queue, if it is one.
    pub queue: Option<media_pp::stats::QueueStats>,
    /// Its stable identity. `None` only in a view built by hand.
    pub id: Option<ElementId>,
    /// Whether it is in the graph, rather than on a finished branch that is
    /// still draining.
    pub attached: bool,
    /// Bytes of packets through its busiest output port — for an encoder,
    /// what it has written.
    pub bytes: u64,
    /// Its ticks, for an element that produces on a schedule of its own —
    /// which here is the compositor.
    pub ticks: Option<TickStats>,
}

impl<'a> From<&'a ElementStats> for Element<'a> {
    fn from(element: &'a ElementStats) -> Self {
        Self {
            name: &element.name,
            taken: element.buffers_in,
            pushed: element.pads.iter().map(|pad| pad.buffers).max(),
            busy: element.busy,
            idle_for: element.idle_for,
            errors: element.errors,
            queue: element.queue,
            id: Some(element.id),
            attached: element.state == ElementState::Attached,
            bytes: element.pads.iter().map(|pad| pad.bytes).max().unwrap_or(0),
            ticks: element.ticks,
        }
    }
}

/// Every element of one pipeline, as the fold reads them.
pub fn elements(stats: &PipelineStats) -> Vec<Element<'_>> {
    stats.elements.iter().map(Element::from).collect()
}

/// One reading of everything the Stats dock shows.
///
/// Built here rather than in the dock because the numbers underneath are
/// running totals: a rate is the difference between two readings, and only
/// the loop that took them knows how far apart they were. What the dock
/// receives is already the answer.
pub struct Reading {
    /// Totals as they last stood, kept to measure the next reading against.
    previous: std::collections::HashMap<String, Totals>,
    /// The compositor's ticks as they last stood, and whose they were — see
    /// [`Reading::rendering`].
    rendered: Option<(Option<ElementId>, TickStats)>,
    /// Each output's bytes as they last stood, and which run of it wrote
    /// them — see [`Reading::output`].
    written: std::collections::HashMap<crate::snapshots::Subject, (Option<ElementId>, u64)>,
    taken_at: std::time::Instant,
}

#[derive(Clone, Copy, Default)]
struct Totals {
    buffers: u64,
    busy: Duration,
}

impl Reading {
    pub fn new() -> Self {
        Self {
            previous: std::collections::HashMap::new(),
            rendered: None,
            written: std::collections::HashMap::new(),
            taken_at: std::time::Instant::now(),
        }
    }

    /// Turns what the pipelines say now into rows, against what they said
    /// last time.
    ///
    /// `named` says which of the few things worth reporting an element
    /// belongs to and what it can say about it, and leaves out everything it
    /// does not name — most of a graph is plumbing nobody would act on. It
    /// is given the element's own reading as well as its name, because a
    /// name does not always tell the elements of one Source apart: a media
    /// file's demuxer and the input it eventually feeds the compositor
    /// share one.
    ///
    /// # One row from several elements
    ///
    /// A subject is rarely one element. A recording is a queue, an encoder
    /// and a muxer, and each holds a different part of the answer — see
    /// [`Role`](crate::snapshots::Role). Everything named alike is folded
    /// into one row, taking from each only what it is asked for.
    ///
    /// # Why each pipeline is labelled
    ///
    /// Several elements of one Source carry the same name — measured on a
    /// media file: its demuxer and its compositor input are both
    /// `scene-item-1`. Keyed by name alone they would overwrite each other
    /// between readings and be measured against each other's totals, which
    /// showed up as a 30 fps clip reporting a rate that climbed every
    /// second. The label keeps each pipeline's names to itself.
    pub fn take(
        &mut self,
        pipelines: &[(&str, Vec<Element<'_>>)],
        named: impl Fn(&Element<'_>) -> Option<(crate::snapshots::Subject, Role)>,
    ) -> crate::snapshots::StatsSnapshot {
        use std::collections::HashMap;

        let now = std::time::Instant::now();
        let interval = now.duration_since(self.taken_at);
        let mut current = HashMap::new();
        let mut folded: HashMap<crate::snapshots::Subject, crate::snapshots::StatsRow> =
            HashMap::new();

        for (pipeline, stats) in pipelines {
            for element in stats {
                let Some((subject, role)) = named(element) else {
                    continue;
                };
                // What it handled: buffers taken, for a stage; buffers
                // pushed, for a source, which takes none and makes them
                // instead.
                let totals = Totals {
                    buffers: element.taken.max(element.pushed.unwrap_or(0)),
                    busy: element.busy,
                };
                // Without an earlier reading there is no rate to report, so
                // the pass after something starts shows it idle rather than
                // as having done all of it at once.
                let key = format!("{pipeline}/{}", element.name);
                let over = self.previous.get(&key).copied().unwrap_or(totals);
                current.insert(key, totals);

                let row =
                    folded
                        .entry(subject.clone())
                        .or_insert_with(|| crate::snapshots::StatsRow {
                            subject,
                            buffers: None,
                            busy: None,
                            queue: None,
                            idle_for: None,
                            errors: 0,
                        });
                match role {
                    Role::Throughput => {
                        let handled = totals.buffers.saturating_sub(over.buffers);
                        row.buffers = Some(row.buffers.unwrap_or(0).max(handled));
                    }
                    Role::Cost => {
                        // The largest rather than the sum: a queue's `busy`
                        // already covers every stage it feeds, so adding
                        // those in would count the same work twice.
                        let busy = share(totals.busy.saturating_sub(over.busy), interval);
                        row.busy = Some(row.busy.unwrap_or(0.0).max(busy));
                        if let Some(queue) = element.queue {
                            row.queue = Some(row.queue.unwrap_or(0.0).max(fullness(queue)));
                        }
                    }
                }
                // The most recent, because a subject has been doing
                // something as long as any part of it has.
                row.idle_for = match (row.idle_for, element.idle_for) {
                    (Some(theirs), Some(mine)) => Some(theirs.min(mine)),
                    (existing, mine) => existing.or(mine),
                };
                row.errors += element.errors;
            }
        }
        self.previous = current;
        self.taken_at = now;
        let mut rows: Vec<_> = folded.into_values().collect();
        rows.sort_by_key(|row| order(&row.subject));
        crate::snapshots::StatsSnapshot {
            rows: std::sync::Arc::new(rows),
            interval,
            rendering: self.rendering(pipelines),
            recording: self.output(OutputKind::Recording, pipelines, interval),
            broadcast: self.output(OutputKind::Broadcast, pipelines, interval),
            disk_available: None,
        }
    }

    /// How the compositor is keeping its frame rate: the ticks it has drawn
    /// and missed, and what drawing a frame took over this interval.
    ///
    /// Found by its ticks rather than its name — it is the only element that
    /// reports any.
    fn rendering(
        &mut self,
        pipelines: &[(&str, Vec<Element<'_>>)],
    ) -> Option<crate::snapshots::Rendering> {
        let (run, ticks) = pipelines
            .iter()
            .flat_map(|(_, elements)| elements)
            .find_map(|element| Some((element.id, element.ticks?)))?;
        // Against the same compositor only: one built again starts from
        // nothing, and measuring it against the last one's totals would read
        // as no frames drawn at all.
        let before = self
            .rendered
            .replace((run, ticks))
            .filter(|(was, _)| *was == run);
        let frame_time = before.and_then(|(_, before)| {
            let made = ticks.made.saturating_sub(before.made);
            (made > 0).then(|| {
                Duration::from_secs_f64(
                    ticks.work.saturating_sub(before.work).as_secs_f64() / made as f64,
                )
            })
        });
        Some(crate::snapshots::Rendering {
            run,
            made: ticks.made,
            missed: ticks.missed,
            frame_time,
        })
    }

    /// What one output has been handed, lost and written, or `None` when it
    /// is not running.
    ///
    /// Its video queue is the head of it: what reached the queue is every
    /// frame the output was given, and what the queue threw away or gave up
    /// waiting to hand on is what never reached the file or the wire. An
    /// output's queues block rather than drop, so a frame is lost only once
    /// the wait has run out — by which point the compositor has been held up
    /// too, and says so as missed ticks. Its encoders, video and audio, are
    /// what it has written.
    ///
    /// Only what is still in the graph: a finished output's branch drains
    /// for a moment after it stops, and counting it would show a recording
    /// that is not running.
    fn output(
        &mut self,
        kind: OutputKind,
        pipelines: &[(&str, Vec<Element<'_>>)],
        interval: Duration,
    ) -> Option<crate::snapshots::OutputStats> {
        let subject = match kind {
            OutputKind::Recording => crate::snapshots::Subject::Recording,
            OutputKind::Broadcast => crate::snapshots::Subject::Broadcast,
        };
        let prefix = kind.prefix();
        let attached = || {
            pipelines
                .iter()
                .flat_map(|(_, elements)| elements)
                .filter(|element| element.attached)
        };
        let queue = format!("{prefix}-queue");
        let Some(head) =
            attached().find(|element| element.name == queue && element.queue.is_some())
        else {
            self.written.remove(&subject);
            return None;
        };
        let encoders = [format!("{prefix}-encode"), format!("{prefix}-audio-encode")];
        let bytes = attached()
            .filter(|element| encoders.iter().any(|name| name == element.name))
            .map(|element| element.bytes)
            .sum();
        // Against the same run only, for the reason the compositor's are:
        // a recording started again has written nothing yet.
        let before = self
            .written
            .insert(subject, (head.id, bytes))
            .filter(|(was, _)| *was == head.id);
        let bitrate = before.and_then(|(_, before)| {
            (!interval.is_zero())
                .then(|| bytes.saturating_sub(before) as f64 * 8.0 / interval.as_secs_f64())
        });
        Some(crate::snapshots::OutputStats {
            run: head.id,
            frames: head.taken,
            lost: head.queue.map_or(0, |queue| queue.dropped) + head.errors,
            bytes,
            bitrate,
        })
    }
}

/// The order rows are shown in: what draws, then what it is written to,
/// then what feeds it. Sources last because there are many and they are the
/// least often the answer.
fn order(subject: &crate::snapshots::Subject) -> (u8, String) {
    use crate::snapshots::Subject;
    match subject {
        Subject::Compositor => (0, String::new()),
        Subject::Recording => (1, String::new()),
        Subject::Broadcast => (2, String::new()),
        Subject::Source(name) => (3, name.clone()),
    }
}

fn share(busy: Duration, interval: Duration) -> f32 {
    if interval.is_zero() {
        return 0.0;
    }
    busy.as_secs_f32() / interval.as_secs_f32()
}

#[cfg(test)]
mod folding {
    use super::*;
    use crate::snapshots::Subject;
    use media_pp::stats::QueueStats;

    fn queue(len: usize, capacity: usize) -> QueueStats {
        QueueStats {
            len,
            capacity,
            dropped: 0,
            blocked: Duration::ZERO,
        }
    }

    /// Two readings a second apart, so a rate is per second and arithmetic
    /// rather than timing decides what the tests below assert.
    fn twice<'a>(
        pipelines: impl Fn(u64) -> Vec<(&'a str, Vec<Element<'a>>)>,
        named: impl Fn(&Element<'_>) -> Option<(Subject, Role)> + Copy,
    ) -> Vec<crate::snapshots::StatsRow> {
        read_twice(pipelines, named).rows.as_ref().clone()
    }

    /// The same, answering the whole second reading rather than its rows.
    fn read_twice<'a>(
        pipelines: impl Fn(u64) -> Vec<(&'a str, Vec<Element<'a>>)>,
        named: impl Fn(&Element<'_>) -> Option<(Subject, Role)> + Copy,
    ) -> crate::snapshots::StatsSnapshot {
        let mut reading = Reading::new();
        reading.take(&pipelines(0), named);
        reading.taken_at = std::time::Instant::now() - Duration::from_secs(1);
        reading.take(&pipelines(1), named)
    }

    /// A recording as the summary reads it: the video queue at its head on
    /// the compositor's pipeline, its video encoder beside it, and its audio
    /// encoder on the mixer's. `second` scales every total.
    fn a_recording<'a>(second: u64) -> Vec<(&'a str, Vec<Element<'a>>)> {
        vec![
            (
                "preview",
                vec![
                    Element {
                        name: "record-queue",
                        taken: 60 * second,
                        errors: second,
                        queue: Some(QueueStats {
                            dropped: 2 * second,
                            ..queue(0, 8)
                        }),
                        attached: true,
                        ..Element::default()
                    },
                    Element {
                        name: "record-encode",
                        bytes: 750_000 * second,
                        attached: true,
                        ..Element::default()
                    },
                ],
            ),
            (
                "mix",
                vec![Element {
                    name: "record-audio-encode",
                    bytes: 20_000 * second,
                    attached: true,
                    ..Element::default()
                }],
            ),
        ]
    }

    /// What OBS shows for an output: frames it was given and lost, what its
    /// encoders wrote — video and audio, from two pipelines — and the rate
    /// they wrote it at.
    #[test]
    fn an_outputs_totals_come_from_its_queue_and_both_of_its_encoders() {
        let snapshot = read_twice(a_recording, |_| None);
        let recording = snapshot.recording.expect("a recording is running");
        assert_eq!(recording.frames, 60);
        assert_eq!(
            recording.lost, 3,
            "thrown away, and given up on after waiting — both never reached the file"
        );
        assert_eq!(recording.bytes, 770_000);
        let bitrate = recording.bitrate.expect("a second reading has a rate");
        assert!(
            (bitrate - 6_160_000.0).abs() < 100_000.0,
            "770 kB in about a second is about 6.2 Mb/s: {bitrate}"
        );
        assert_eq!(snapshot.broadcast, None, "and nothing is being broadcast");
    }

    /// A stopped recording's branch drains for a moment outside the graph.
    /// Counted, it would show a recording running after it had stopped.
    #[test]
    fn a_finished_outputs_draining_branch_is_not_a_running_output() {
        let snapshot = read_twice(
            |second| {
                let mut pipelines = a_recording(second);
                for (_, elements) in &mut pipelines {
                    for element in elements {
                        element.attached = false;
                    }
                }
                pipelines
            },
            |_| None,
        );
        assert_eq!(snapshot.recording, None);
    }

    /// The first reading of a run has nothing to measure a rate against,
    /// and says so rather than dividing a whole run's bytes by a second.
    #[test]
    fn an_outputs_first_reading_has_no_bitrate() {
        let mut reading = Reading::new();
        let snapshot = reading.take(&a_recording(10), |_| None);
        assert_eq!(snapshot.recording.map(|r| r.bitrate), Some(None));
    }

    /// What a frame took to draw, over the interval: the compositor's work
    /// between two readings over the frames it drew in between.
    #[test]
    fn the_time_a_frame_takes_to_draw_is_measured_between_two_readings() {
        let snapshot = read_twice(
            |second| {
                vec![(
                    "preview",
                    vec![Element {
                        name: "preview-compositor",
                        ticks: Some(TickStats {
                            made: 60 * second,
                            missed: second,
                            work: Duration::from_millis(18 * second),
                        }),
                        ..Element::default()
                    }],
                )]
            },
            |_| None,
        );
        let rendering = snapshot
            .rendering
            .expect("the compositor reports its ticks");
        assert_eq!((rendering.made, rendering.missed), (60, 1));
        assert_eq!(rendering.frame_time, Some(Duration::from_micros(300)));
    }

    /// A recording is a queue, an encoder and a muxer, and only the muxer
    /// knows what reached the file while only the queue knows what the
    /// encoding cost. One row, built from both.
    #[test]
    fn an_outputs_elements_fold_into_one_row() {
        let rows = twice(
            |second| {
                vec![(
                    "preview",
                    vec![
                        Element {
                            name: "record-queue",
                            taken: 60 * second,
                            busy: Duration::from_millis(300 * second),
                            queue: Some(queue(2, 8)),
                            ..Element::default()
                        },
                        Element {
                            name: "record-video",
                            taken: 60 * second,
                            ..Element::default()
                        },
                    ],
                )]
            },
            |element| {
                Some((
                    Subject::Recording,
                    if element.queue.is_some() {
                        Role::Cost
                    } else {
                        Role::Throughput
                    },
                ))
            },
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].buffers, Some(60));
        // Near rather than exact: the interval is a real one, a hair over
        // the second it was set to.
        assert!(
            (rows[0].busy.unwrap() - 0.3).abs() < 0.01,
            "{:?}",
            rows[0].busy
        );
        assert_eq!(rows[0].queue, Some(0.25));
    }

    /// The defect this was written against: a media file's demuxer and the
    /// compositor input it feeds are both named `scene-item-1`, so totals
    /// kept by name alone overwrote each other between readings and each
    /// was measured against the other. A 30 fps clip reported a rate that
    /// climbed every second — 385, then 402, then 418.
    #[test]
    fn elements_of_two_pipelines_sharing_a_name_do_not_measure_each_other() {
        let rows = twice(
            |second| {
                vec![
                    (
                        "preview",
                        vec![Element {
                            name: "shared",
                            taken: 1_000 * second,
                            ..Element::default()
                        }],
                    ),
                    (
                        "scene-item-1",
                        vec![Element {
                            name: "shared",
                            taken: 30 * second,
                            ..Element::default()
                        }],
                    ),
                ]
            },
            |element| Some((Subject::Source(element.name.to_owned()), Role::Throughput)),
        );
        // Both pipelines name the same subject, so the row is the busier of
        // them — and crucially 1000, not 1000 plus the difference between
        // the two totals.
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].buffers, Some(1_000));
    }

    /// What a Source costs is not reported at all, and the row must leave
    /// the column empty rather than claim nothing: a pacer's wait is spent
    /// inside the call, so a healthy 30 fps clip reads as fully busy with a
    /// queue at capacity. Only what is asked of an element is taken from it.
    #[test]
    fn an_element_asked_only_for_throughput_contributes_no_cost() {
        let rows = twice(
            |second| {
                vec![(
                    "scene-item-1",
                    vec![Element {
                        name: "scene-item-1",
                        taken: 30 * second,
                        busy: Duration::from_secs(second),
                        queue: Some(queue(8, 8)),
                        ..Element::default()
                    }],
                )]
            },
            |_| Some((Subject::Source("Clip".to_owned()), Role::Throughput)),
        );
        assert_eq!(rows[0].buffers, Some(30));
        assert_eq!(rows[0].busy, None);
        assert_eq!(rows[0].queue, None);
    }

    /// A compositor takes nothing in — it is its pipeline's source, and what
    /// it made is on its output port. Read as buffers taken it was reported
    /// as doing nothing while drawing sixty frames a second.
    #[test]
    fn what_a_source_element_made_counts_as_what_it_handled() {
        let rows = twice(
            |second| {
                vec![(
                    "preview",
                    vec![Element {
                        name: "preview-compositor",
                        taken: 0,
                        pushed: Some(60 * second),
                        ..Element::default()
                    }],
                )]
            },
            |_| Some((Subject::Compositor, Role::Throughput)),
        );
        assert_eq!(rows[0].buffers, Some(60));
    }

    /// Most of a graph is plumbing, and a row for each would bury the few
    /// that someone would act on.
    #[test]
    fn an_element_nobody_named_is_left_out() {
        let rows = twice(
            |_| {
                vec![(
                    "preview",
                    vec![Element {
                        name: "preview-rate",
                        ..Element::default()
                    }],
                )]
            },
            |_| None,
        );
        assert!(rows.is_empty());
    }

    /// The first reading has nothing to measure against, and a total is the
    /// whole run. Counted from zero it would report an hour of recording as
    /// having happened in one second.
    #[test]
    fn the_first_reading_of_something_shows_no_rate_rather_than_all_of_it() {
        let mut reading = Reading::new();
        let pipelines = vec![(
            "preview",
            vec![Element {
                name: "record-video",
                taken: 100_000,
                ..Element::default()
            }],
        )];
        let snapshot = reading.take(&pipelines, |_| Some((Subject::Recording, Role::Throughput)));
        assert_eq!(snapshot.rows[0].buffers, Some(0));
    }

    /// A subject has been doing something as long as any part of it has, so
    /// the muxer waiting on a queue does not make the recording look stalled.
    #[test]
    fn a_rows_idle_time_is_that_of_its_busiest_part() {
        let rows = twice(
            |_| {
                vec![(
                    "preview",
                    vec![
                        Element {
                            name: "record-queue",
                            idle_for: Some(Duration::from_millis(4)),
                            queue: Some(queue(0, 8)),
                            ..Element::default()
                        },
                        Element {
                            name: "record-video",
                            idle_for: Some(Duration::from_secs(9)),
                            ..Element::default()
                        },
                    ],
                )]
            },
            |element| {
                Some((
                    Subject::Recording,
                    if element.queue.is_some() {
                        Role::Cost
                    } else {
                        Role::Throughput
                    },
                ))
            },
        );
        assert_eq!(rows[0].idle_for, Some(Duration::from_millis(4)));
    }

    /// Rows come in the order they are read in, which is the order the
    /// engine happened to build its pipelines in. What is shown is fixed.
    #[test]
    fn rows_are_ordered_by_what_they_are_rather_than_by_when_they_were_read() {
        let rows = twice(
            |_| {
                vec![(
                    "preview",
                    vec![
                        Element {
                            name: "zebra",
                            ..Element::default()
                        },
                        Element {
                            name: "stream",
                            ..Element::default()
                        },
                        Element {
                            name: "apple",
                            ..Element::default()
                        },
                        Element {
                            name: "compositor",
                            ..Element::default()
                        },
                    ],
                )]
            },
            |element| {
                let subject = match element.name {
                    "compositor" => Subject::Compositor,
                    "stream" => Subject::Broadcast,
                    name => Subject::Source(name.to_owned()),
                };
                Some((subject, Role::Throughput))
            },
        );
        let order: Vec<_> = rows.iter().map(|row| row.subject.clone()).collect();
        assert_eq!(
            order,
            vec![
                Subject::Compositor,
                Subject::Broadcast,
                Subject::Source("apple".to_owned()),
                Subject::Source("zebra".to_owned()),
            ]
        );
    }
}
