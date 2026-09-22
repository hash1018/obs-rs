//! How many threads a video decoded in software gets, and how they share the
//! work — by codec and picture size.
//!
//! Only a stream `VideoDecodeBin` decodes in software ever uses this: one
//! whose codec the GPU has no decoder for, or with alpha, or 4:2:2 or 4:4:4,
//! or one the GPU refused. What the GPU decodes — 10-bit 4:2:0 included,
//! brought down to 8 bits on the GPU — has no threads to give.
//!
//! # Where the numbers come from
//!
//! Each cell is the fewest threads that decode a 60-a-second picture of that
//! size at one and a half to two times the rate — measured at 1080p and 4K on
//! a twelve-thread machine, one thread against several:
//!
//! ```text
//!               1080p, 1 thread    4K, 1 thread    4K, 4 threads
//! H.264              186               47              146
//! HEVC 10-bit        138               33              104
//! ProRes 4444         86               23               68  (6: 91)
//! VP9                316                —                —
//! ```
//!
//! More than needed costs twice over. Several pictures at once keeps a copy
//! of the decoder per thread — HEVC 10-bit at 4K took 369 MB on four threads
//! and 750 MB on thirteen — and the threads spend time keeping in step, some
//! ten to twenty percent more work in all. Fewer than needed is a picture
//! that cannot keep up, which is why nothing here is left at one thread
//! beyond 1080p.
//!
//! ProRes and DNxHD split within one picture: every picture stands alone,
//! several at once is no faster, and slices cost no memory per thread. VP9
//! splits into tiles where it was encoded with them, which is enough below
//! 4K; at 4K, a file encoded without tiles would not keep up on slices, so it
//! takes several pictures at once. Motion JPEG gains from neither.
//!
//! A live stream always decodes within one picture, since several pictures
//! at once would hold a picture per thread back from someone watching it
//! now. For H.264 and HEVC, which are rarely split within a picture, that is
//! no faster than one thread — a 4K 10-bit HEVC camera the GPU refused may
//! not keep up in software.

use std::num::NonZeroU32;

use media_pp::elements::{DecodePath, DecodeThreadKind, DecodeThreading, VideoDecodeBin};
use media_pp::ffmpeg::codec::Id;

/// What a decoded stream is played as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::engine) enum Playback {
    /// A file, which has no deadline.
    File,
    /// A live stream, whose picture is wanted now.
    Live,
}

/// One cell of the table.
#[derive(Debug, Clone, Copy)]
enum Cell {
    /// Left as FFmpeg opens a decoder: one thread.
    One,
    Threads(DecodeThreadKind, u32),
}

/// The largest picture area in each size column but the last, which takes
/// anything larger.
const COLUMNS: [u64; 3] = [1280 * 720, 1920 * 1080, 2560 * 1440];

/// The column a stream of unknown size is read from: 1080p.
const UNKNOWN_SIZE: usize = 1;

/// A codec's row: ≤720p, ≤1080p, ≤1440p, and larger.
fn row(codec: Id) -> [Cell; 4] {
    use Cell::{One, Threads};
    use DecodeThreadKind::{Frame, Slice};
    match codec {
        Id::H264 => [One, One, Threads(Frame, 2), Threads(Frame, 4)],
        Id::HEVC => [One, Threads(Frame, 2), Threads(Frame, 3), Threads(Frame, 4)],
        Id::PRORES | Id::DNXHD => [One, Threads(Slice, 2), Threads(Slice, 4), Threads(Slice, 6)],
        Id::VP9 => [One, One, Threads(Slice, 2), Threads(Frame, 4)],
        Id::MJPEG => [One; 4],
        // Unmeasured, and so given room.
        _ => [One, Threads(Frame, 2), Threads(Frame, 4), Threads(Frame, 4)],
    }
}

/// The threading a software decode of `codec` at `size` gets — `None` for
/// FFmpeg's own single thread.
pub(in crate::engine) fn threading(
    codec: Id,
    size: Option<[u32; 2]>,
    playback: Playback,
) -> Option<DecodeThreading> {
    let column = size.map_or(UNKNOWN_SIZE, |[width, height]| {
        let area = u64::from(width) * u64::from(height);
        COLUMNS
            .iter()
            .position(|&largest| area <= largest)
            .unwrap_or(COLUMNS.len())
    });
    match row(codec)[column] {
        Cell::One => None,
        Cell::Threads(kind, threads) => Some(DecodeThreading {
            threads: NonZeroU32::new(threads),
            kind: match playback {
                Playback::File => kind,
                Playback::Live => DecodeThreadKind::Slice,
            },
        }),
    }
}

/// Says, once, when a Source's video turned out to be decoded in software,
/// why, and what the table gave it to do that with — which is what to read
/// when the table's numbers are being weighed against a real file. Nothing is
/// said for a stream the GPU decodes, which the table does not touch; one the
/// GPU refuses later is media-pp's to say, in its own log.
pub(in crate::engine) fn log(
    item: &str,
    codec: Id,
    size: Option<[u32; 2]>,
    threading: Option<DecodeThreading>,
    decoder: &VideoDecodeBin,
) {
    let DecodePath::Software(reason) = decoder.path() else {
        return;
    };
    let size = size.map_or_else(|| "size unknown".to_owned(), |[w, h]| format!("{w}x{h}"));
    let threads = match threading {
        None => "one thread".to_owned(),
        Some(threading) => format!(
            "{:?} on {} threads",
            threading.kind,
            threading
                .threads
                .map_or_else(|| "all".to_owned(), |threads| threads.to_string())
        ),
    };
    tracing::info!(
        "\"{item}\" decodes its video in software ({reason:?}): {codec:?} {size}, {threads}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn threads(threading: Option<DecodeThreading>) -> Option<(DecodeThreadKind, u32)> {
        threading.map(|threading| (threading.kind, threading.threads.map_or(0, NonZeroU32::get)))
    }

    #[test]
    fn a_file_gets_its_codecs_cell_for_its_size() {
        use DecodeThreadKind::{Frame, Slice};
        let file = |codec, size| threads(threading(codec, Some(size), Playback::File));

        assert_eq!(file(Id::H264, [1920, 1080]), None, "one thread is plenty");
        assert_eq!(file(Id::H264, [3840, 2160]), Some((Frame, 4)));
        assert_eq!(file(Id::HEVC, [1920, 1080]), Some((Frame, 2)));
        assert_eq!(file(Id::HEVC, [2560, 1440]), Some((Frame, 3)));
        assert_eq!(file(Id::PRORES, [3840, 2160]), Some((Slice, 6)));
        assert_eq!(file(Id::DNXHD, [1920, 1080]), Some((Slice, 2)));
        assert_eq!(file(Id::VP9, [2560, 1440]), Some((Slice, 2)));
        assert_eq!(file(Id::VP9, [3840, 2160]), Some((Frame, 4)));
        assert_eq!(file(Id::MJPEG, [3840, 2160]), None, "it gains from neither");
        assert_eq!(file(Id::AV1, [1280, 720]), None);
        assert_eq!(file(Id::AV1, [1920, 1080]), Some((Frame, 2)));
    }

    /// A size is the largest of its column, and anything past the last is
    /// the last: 1080p is still 1080p, one pixel more is the next column,
    /// and 8K is what 4K is.
    #[test]
    fn a_size_falls_in_the_first_column_it_fits() {
        let hevc = |size| threads(threading(Id::HEVC, Some(size), Playback::File));
        assert_eq!(hevc([1280, 720]), None);
        assert_eq!(hevc([1280, 721]).map(|(_, n)| n), Some(2));
        assert_eq!(hevc([7680, 4320]).map(|(_, n)| n), Some(4));
    }

    /// A stream that does not say its size is taken as 1080p.
    #[test]
    fn an_unknown_size_is_read_as_1080p() {
        assert_eq!(
            threads(threading(Id::HEVC, None, Playback::File)),
            Some((DecodeThreadKind::Frame, 2))
        );
        assert_eq!(threads(threading(Id::H264, None, Playback::File)), None);
    }

    /// Live keeps the count and never decodes several pictures at once.
    #[test]
    fn a_live_stream_decodes_within_one_picture() {
        assert_eq!(
            threads(threading(Id::H264, Some([3840, 2160]), Playback::Live)),
            Some((DecodeThreadKind::Slice, 4))
        );
        assert_eq!(
            threads(threading(Id::PRORES, Some([3840, 2160]), Playback::Live)),
            Some((DecodeThreadKind::Slice, 6))
        );
        assert_eq!(
            threads(threading(Id::H264, Some([1920, 1080]), Playback::Live)),
            None
        );
    }
}
