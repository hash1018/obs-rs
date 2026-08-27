mod scenes;
mod status;

pub use scenes::{SceneSnapshot, ScenesSnapshot};
pub use status::StatusSnapshot;

/// Read-only application state consumed while drawing one UI frame.
///
/// Add future Studio, scene, or output snapshots here instead of growing every
/// UI function's parameter list.
#[derive(Default)]
pub struct Snapshots {
    pub scenes: ScenesSnapshot,
    pub status: StatusSnapshot,
}
