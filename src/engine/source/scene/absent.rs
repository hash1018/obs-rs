//! What a platform without a Scene-in-a-Scene answers: every one but
//! Windows, Linux and macOS, which is every one with no backend here at all.

use crate::engine::source::OpenOutcome;

pub(in crate::engine) fn open() -> OpenOutcome {
    OpenOutcome::Absent("a Scene inside a Scene is not drawn on this platform yet".to_owned())
}
