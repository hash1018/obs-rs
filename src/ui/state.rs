use super::docking::DockLayout;
use super::editor::SceneEditorState;
use super::panels::scenes::ScenesPanelState;
use super::panels::sources::SourcesPanelState;
use super::preview::PreviewViewState;
use super::settings::SettingsDialogState;

#[derive(Default)]
pub struct UiState {
    pub(super) about_open: bool,
    /// Whether the "a recording is running" question is up — see
    /// `shell::confirm_exit`.
    pub(super) exit_confirm_open: bool,
    pub(super) dock_layout: DockLayout,
    pub(super) fullscreen: bool,
    pub(super) scenes: ScenesPanelState,
    pub(super) editor: SceneEditorState,
    pub(super) sources: SourcesPanelState,
    pub(super) preview: PreviewViewState,
    pub(super) settings: SettingsDialogState,
}

impl UiState {
    /// Starts with the arrangement a settings file described.
    ///
    /// Only the docks come from it. Everything else here is this run's own
    /// state — an open dialog, a drag in progress — and starting a session
    /// inside one of those is not something to restore.
    pub fn with_docks(docks: &crate::ui::WorkspaceDocks) -> Self {
        Self {
            dock_layout: DockLayout::restored(docks),
            ..Self::default()
        }
    }

    /// The dock arrangement as it stands, for the settings file.
    pub fn docks(&self) -> crate::ui::WorkspaceDocks {
        self.dock_layout.placement()
    }

    /// Asks whether to quit while a recording is running.
    pub fn confirm_exit(&mut self) {
        self.exit_confirm_open = true;
    }

    /// Opens the Settings dialog on a copy of what is currently set.
    ///
    /// The draft is seeded by the caller's settings rather than read from
    /// anywhere in here: this module holds no copy of them, and one taken
    /// from a stale place would quietly put old values back on Apply.
    pub fn open_settings(&mut self, settings: &crate::settings::AppSettings) {
        self.settings.open_with(settings);
    }
}
