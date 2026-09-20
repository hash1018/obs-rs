//! What a platform without a Scene-in-a-Scene answers, which is every one
//! but Windows so far — the CUDA backend composites a Canvas and has no
//! second compositor to give a Scene of its own yet.

use crate::engine::source::OpenOutcome;

pub(in crate::engine) fn open() -> OpenOutcome {
    OpenOutcome::Absent("a Scene inside a Scene is not drawn on this platform yet".to_owned())
}
