mod audio;
mod scenes;
mod sources;
mod stats;
mod status;

pub use audio::{AudioSnapshot, AudioSourceSnapshot};
pub use scenes::{SceneSnapshot, ScenesSnapshot};
pub use sources::{SceneItemSnapshot, SourcesSnapshot};
pub use stats::{OutputStats, Rendering, Role, StatsRow, StatsSnapshot, Subject};
pub use status::{ScreenshotReport, SourceStatus, StatusSnapshot};

/// Read-only application state consumed while drawing one UI frame.
///
/// Add future Studio, scene, or output snapshots here instead of growing every
/// UI function's parameter list.
#[derive(Default)]
pub struct Snapshots {
    pub audio: AudioSnapshot,
    pub stats: StatsSnapshot,
    pub scenes: ScenesSnapshot,
    pub sources: SourcesSnapshot,
    pub status: StatusSnapshot,
}
