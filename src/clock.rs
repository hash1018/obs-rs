//! The user's own wall clock, in their own time zone.
//!
//! # Why this is not just `OffsetDateTime::now_local()`
//!
//! `time` refuses to read the local UTC offset from a process that has more
//! than one thread, on every Unix. It is not being cautious for its own sake:
//! the C library call it would have to make reads the `TZ` environment
//! variable, and another thread setting the environment while it does is a
//! data race with no defined behaviour. So `now_local` answers `Err` in this
//! application for the whole of its life, and every caller that falls back to
//! UTC is nine hours out in Seoul.
//!
//! That is survivable for a recording's file name, which is what the two
//! callers before this one did. It is not survivable for a clock drawn on
//! the Canvas, which is the whole point of a clock.
//!
//! So the offset is read exactly once, from `main`, before this process has
//! a second thread — where the call is sound and succeeds — and every reading
//! afterwards is that offset applied to a UTC instant.
//!
//! # What that costs
//!
//! The offset is the one this process started in. A machine that crosses a
//! daylight-saving boundary while obs-rs is running shows the old offset
//! until it is restarted. Re-reading it later is exactly what cannot be done
//! safely, and an hour once or twice a year against a clock that is right the
//! rest of the time is the better side of that trade.

use std::sync::OnceLock;

use time::{OffsetDateTime, UtcOffset};

static LOCAL_OFFSET: OnceLock<UtcOffset> = OnceLock::new();

/// Reads this machine's UTC offset and remembers it.
///
/// **Must be called from `main` before any thread is spawned**, which is the
/// condition that makes it succeed at all. Calling it twice is harmless and
/// the second call is ignored; calling it late is not an error either, it
/// just fails and leaves everything on UTC.
pub fn capture_local_offset() {
    if let Ok(offset) = UtcOffset::current_local_offset() {
        let _ = LOCAL_OFFSET.set(offset);
    } else {
        eprintln!("could not read this machine's time zone; clocks will show UTC");
    }
}

/// Now, in the offset [`capture_local_offset`] found — or UTC if it found
/// none, which is a wrong clock rather than no clock.
pub fn now_local() -> OffsetDateTime {
    let now = OffsetDateTime::now_utc();
    match LOCAL_OFFSET.get() {
        Some(offset) => now.to_offset(*offset),
        None => now,
    }
}

/// Now, as unix microseconds — what a stopwatch is stamped with.
///
/// UTC underneath, so a timer is unaffected by the offset having been read
/// or not: only the wall clock's *display* needs a time zone, and an elapsed
/// duration is the difference between two instants in any of them.
pub fn now_micros() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever offset was captured — and under a test harness, which is
    /// multithreaded before the first test runs, it is usually none — the
    /// two must describe the same *instant*. An offset applied by adding
    /// hours to a UTC reading rather than by relabelling it would put every
    /// recording's name nine hours into the future here, which is a worse
    /// bug than the one this module exists to fix.
    #[test]
    fn the_local_reading_is_the_same_instant_as_the_utc_one() {
        let before = OffsetDateTime::now_utc();
        let local = now_local();
        let after = OffsetDateTime::now_utc();
        assert!(
            (before..=after).contains(&local),
            "{local} is not between {before} and {after}"
        );
    }

    /// And in the offset that was captured, where one was. This is what a
    /// clock on the Canvas reads and what a file is named for, so the two
    /// halves — right instant, right offset — are asserted apart.
    #[test]
    fn the_local_reading_carries_the_captured_offset() {
        capture_local_offset();
        let Some(captured) = LOCAL_OFFSET.get() else {
            // The harness is already multithreaded, so the capture fails
            // here exactly as it would from a late caller in the
            // application. UTC is then what everything reads, which is the
            // documented fallback rather than a failure.
            assert_eq!(now_local().offset(), UtcOffset::UTC);
            return;
        };
        assert_eq!(now_local().offset(), *captured);
    }

    /// Microseconds, not milliseconds or nanoseconds: a stopwatch stored in
    /// the wrong unit reads a thousand times fast, and nothing else in the
    /// arithmetic would notice.
    #[test]
    fn the_stopwatch_clock_is_in_microseconds() {
        let micros = now_micros();
        let seconds = OffsetDateTime::now_utc().unix_timestamp();
        assert!(
            (micros / 1_000_000 - seconds).abs() <= 1,
            "{micros} µs is not {seconds} s"
        );
    }
}
