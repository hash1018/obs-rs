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
