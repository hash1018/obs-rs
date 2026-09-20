use super::docking::DockLayout;
use super::editor::SceneEditorState;
use super::panels::filters::FiltersPanelState;
use super::panels::scenes::ScenesPanelState;
use super::panels::sources::SourcesPanelState;
use super::panels::stats::StatsPanelState;
use super::preview::PreviewViewState;
use super::settings::SettingsDialogState;

#[derive(Default)]
pub struct UiState {
    pub(super) about_open: bool,
    /// Whether the "a recording is running" question is up — see
    /// `shell::confirm_exit`.
    pub(super) exit_confirm_open: bool,
    /// Why the project could not be opened, until the user has been told.
    ///
    /// `Some` means nothing this session does will be remembered, which is
    /// not something to leave a user to work out — see
    /// `shell::report_project_error`.
    pub(super) project_error: Option<String>,
    pub(super) dock_layout: DockLayout,
    pub(super) fullscreen: bool,
    /// The Canvas on another screen, while one is open — see
    /// [`Projector`]. `None` is the ordinary case: this is asked for when it
    /// is wanted and closed when it is not, rather than remembered across
    /// runs, where a window appearing on a second screen at launch would be
    /// a surprise.
    pub projector: Option<Projector>,
    pub(super) scenes: ScenesPanelState,
    pub(super) editor: SceneEditorState,
    pub(super) sources: SourcesPanelState,
    pub(super) filters: FiltersPanelState,
    pub(super) stats: StatsPanelState,
    pub(super) preview: PreviewViewState,
    pub(super) settings: SettingsDialogState,
    pub(super) hotkeys: super::shell::hotkeys::HotkeyState,
}

impl UiState {
    /// Starts with the arrangement a settings file described.
    ///
    /// Only what the user arranged comes from it. Everything else here is
    /// this run's own state — an open dialog, a drag in progress — and
    /// starting a session inside one of those is not something to restore.
    pub fn restored(docks: &crate::ui::WorkspaceDocks, zoom: &crate::ui::PreviewZoom) -> Self {
        Self {
            dock_layout: DockLayout::restored(docks),
            preview: PreviewViewState::restored(zoom),
            ..Self::default()
        }
    }

    /// Whether the Preview is handing a Browser Source the keyboard, which
    /// is what keeps a hotkey from being spent on this window while someone
    /// is typing into a page — see `shell::hotkeys::keyboard_taken`.
    pub fn interacting_with_a_page(&self) -> bool {
        self.editor.interacting.is_some()
    }

    /// The dock arrangement as it stands, for the settings file.
    pub fn docks(&self) -> crate::ui::WorkspaceDocks {
        self.dock_layout.placement()
    }

    /// The Preview zoom as it stands, for the settings file.
    pub fn preview_zoom(&self) -> crate::ui::PreviewZoom {
        self.preview.zoom()
    }

    /// Says the project could not be opened, so the user is told before
    /// building a Scene that will not be there next time.
    pub fn report_project_error(&mut self, error: String) {
        self.project_error = Some(error);
    }

    /// Asks whether to quit while a recording is running.
    pub fn confirm_exit(&mut self) {
        self.exit_confirm_open = true;
    }

    /// Opens the Filters dock on one sound's filters, opening the dock
    /// itself if it was closed — a menu item that answered by changing a
    /// dock nobody can see would look like it did nothing.
    pub fn show_audio_filters(&mut self, host: super::AudioFilterHost) {
        self.filters.show_audio(host);
        self.dock_layout
            .set_open(super::docking::DockPanel::Filters, true);
    }

    /// Opens the Settings dialog on a copy of what is currently set.
    ///
    /// The draft is seeded by the caller's settings rather than read from
    /// anywhere in here: this module holds no copy of them, and one taken
    /// from a stale place would quietly put old values back on Apply.
    pub fn open_settings(&mut self, settings: &crate::settings::AppSettings) {
        self.settings.open_with(settings);
    }

    /// The same, on the Hotkeys page — for whatever says a key is missing
    /// and should take the user to where one is set.
    pub fn open_hotkey_settings(&mut self, settings: &crate::settings::AppSettings) {
        self.settings.open_with(settings);
        self.settings.show_hotkeys();
    }
}

/// Where a projector window is, while one is open.
///
/// What it shows is fixed — the Canvas, as it is composited, with none of the
/// editor's own marks — so all this says is which screen it fills, or that it
/// is an ordinary window the user places themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Projector {
    /// Filling one screen, by the name and place the display list gave.
    Screen {
        name: String,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    /// A window like any other, for a desktop that will not let an
    /// application choose a screen — and for a second view on this one.
    Window,
}
