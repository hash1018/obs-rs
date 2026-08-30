//! The Settings dialog: a page list on the left, the chosen page on the
//! right, and the buttons that decide what becomes real.
//!
//! # Everything here edits a draft
//!
//! The dialog holds its own [`AppSettings`], seeded from the live ones when it
//! opens, and nothing it changes reaches the application until OK or Apply.
//! That is what makes Cancel mean something, and it is also what keeps a
//! half-typed number out of the engine: a bit rate field passes through `1`
//! and `12` on the way to `120`, and each of those would otherwise be a
//! setting the next recording could have used.
//!
//! Theme and language are drafted too, even though the menu bar changes them
//! at once. One rule for the whole dialog is worth more than the moment of
//! preview an exception would buy, and opening it re-seeds the draft, so the
//! two paths cannot disagree about what is currently set.

mod general;
mod recording;
mod video;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use eframe::egui;

use crate::i18n::{LocalizationManager, TextKey};
use crate::settings::AppSettings;

use super::UiAction;

/// Which page the list on the left has selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::ui) enum SettingsPage {
    #[default]
    General,
    Video,
    Recording,
}

impl SettingsPage {
    /// Every page, in the order the list shows them.
    const ALL: [Self; 3] = [Self::General, Self::Video, Self::Recording];

    fn title(self) -> TextKey {
        match self {
            Self::General => TextKey::SettingsPageGeneral,
            Self::Video => TextKey::SettingsPageVideo,
            Self::Recording => TextKey::SettingsPageRecording,
        }
    }
}

/// The dialog's own state, including the draft it is editing.
#[derive(Default)]
pub(in crate::ui) struct SettingsDialogState {
    pub(in crate::ui) open: bool,
    page: SettingsPage,
    /// Seeded by [`SettingsDialogState::open_with`]; meaningless while closed.
    draft: AppSettings,
    /// A folder picker that is open, waiting to say what was chosen.
    ///
    /// The dialog it shows is the desktop's own and stays up until the user
    /// answers, so it is run on a thread of its own — asking for it from the
    /// pass that drew the button would freeze this window for as long as the
    /// picker was open.
    folder_picker: Option<Receiver<Option<PathBuf>>>,
}

impl SettingsDialogState {
    /// Opens the dialog on a copy of what is currently set.
    ///
    /// Re-seeded on every open rather than kept between them: the menu bar can
    /// change theme and language while this is closed, and a stale draft would
    /// quietly put them back on the next Apply.
    pub(in crate::ui) fn open_with(&mut self, settings: &AppSettings) {
        self.draft = settings.clone();
        self.open = true;
    }

    /// Whether a folder picker is up, which is what keeps a second one from
    /// being opened on top of the first.
    fn picking_folder(&self) -> bool {
        self.folder_picker.is_some()
    }

    /// Opens the desktop's folder picker, starting where the field points.
    fn pick_folder(&mut self, wake_ui: impl Fn() + Send + 'static) {
        if self.picking_folder() {
            return;
        }
        let start = self.draft.recording.directory_or_default();
        let (sender, receiver) = mpsc::channel();
        // Detached: the dialog outlives this pass, and dropping the receiver
        // when the window closes is what tells the thread nobody is waiting.
        let spawned = std::thread::Builder::new()
            .name("folder-picker".to_owned())
            .spawn(move || {
                let mut dialog = rfd::FileDialog::new();
                if start.is_dir() {
                    dialog = dialog.set_directory(&start);
                }
                let picked = dialog.pick_folder();
                if sender.send(picked).is_ok() {
                    wake_ui();
                }
            });
        if let Err(error) = spawned {
            eprintln!("could not open the folder picker: {error}");
            return;
        }
        self.folder_picker = Some(receiver);
    }

    /// Takes the picker's answer, if it has one.
    ///
    /// A cancelled picker is still an answer — it ends the wait and puts the
    /// button back — so what distinguishes the two is `Some(None)`.
    fn poll_folder_picker(&mut self) {
        let Some(receiver) = &self.folder_picker else {
            return;
        };
        match receiver.try_recv() {
            Ok(picked) => {
                if let Some(path) = picked {
                    self.draft.recording.directory = path.display().to_string();
                }
                self.folder_picker = None;
            }
            // The thread is gone without answering, which nothing can be done
            // about except stop waiting for it.
            Err(mpsc::TryRecvError::Disconnected) => self.folder_picker = None,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

/// Width of the page list. Fixed rather than proportional so the pages either
/// side of it do not reflow when the window is resized.
const PAGE_LIST_WIDTH: f32 = 120.0;

/// Enough for the widest page without the window resizing as pages change,
/// which is what makes the list feel like tabs rather than navigation. The
/// height is the Recording page's five rows plus the note that appears above
/// them while a recording is running.
const PAGE_WIDTH: f32 = 380.0;
const PAGE_HEIGHT: f32 = 210.0;

pub(in crate::ui) fn show(
    ctx: &egui::Context,
    state: &mut SettingsDialogState,
    recording: bool,
    encoders: &[crate::settings::RecordingEncoder],
    audio_codecs: &[crate::settings::RecordingAudioCodec],
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    if !state.open {
        return;
    }
    state.poll_folder_picker();
    let picking = state.picking_folder();

    let mut open = true;
    let mut browse = false;
    let mut apply = false;
    let mut close = false;
    egui::Window::new(i18n.text(TextKey::SettingsTitle))
        .id(egui::Id::new("settings_dialog"))
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            // Bounded before anything in it is laid out, because the divider
            // below is a vertical rule and one of those takes the height
            // *available* to it — which inside a window is the rest of the
            // screen. Without this the dialog grew past the bottom of the
            // application and took its own buttons with it.
            ui.scope(|ui| {
                ui.set_max_height(PAGE_HEIGHT);
                ui.horizontal_top(|ui| {
                    // `top_down_justified` rather than a plain `vertical`: the
                    // parent here lays out horizontally, so an ordinary child
                    // would put the pages in a row, and justified is what makes
                    // each entry fill the column the way a list does.
                    ui.allocate_ui_with_layout(
                        egui::vec2(PAGE_LIST_WIDTH, PAGE_HEIGHT),
                        egui::Layout::top_down_justified(egui::Align::LEFT),
                        |ui| {
                            for page in SettingsPage::ALL {
                                let selected = state.page == page;
                                if ui
                                    .selectable_label(selected, i18n.text(page.title()))
                                    .clicked()
                                {
                                    state.page = page;
                                }
                            }
                        },
                    );
                    ui.separator();
                    ui.allocate_ui(egui::vec2(PAGE_WIDTH, PAGE_HEIGHT), |ui| {
                        // `max_height` as well as the allocation: a scroll area
                        // told not to shrink takes the height *available* to it,
                        // which inside a window is the rest of the screen, and the
                        // dialog then grew past the bottom of the application.
                        egui::ScrollArea::vertical()
                            .max_height(PAGE_HEIGHT)
                            .auto_shrink([false, false])
                            .show(ui, |ui| match state.page {
                                SettingsPage::General => {
                                    general::show(ui, &mut state.draft, i18n);
                                }
                                SettingsPage::Video => {
                                    video::show(ui, &mut state.draft, recording, i18n);
                                }
                                SettingsPage::Recording => {
                                    browse = recording::show(
                                        ui,
                                        &mut state.draft,
                                        recording,
                                        picking,
                                        encoders,
                                        audio_codecs,
                                        i18n,
                                    );
                                }
                            });
                    });
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(i18n.text(TextKey::ActionOk)).clicked() {
                    apply = true;
                    close = true;
                }
                if ui.button(i18n.text(TextKey::ActionCancel)).clicked() {
                    close = true;
                }
                if ui.button(i18n.text(TextKey::ActionApply)).clicked() {
                    apply = true;
                }
            });
        });

    if browse {
        let ctx = ctx.clone();
        state.pick_folder(move || ctx.request_repaint());
    }
    if apply {
        actions.push(UiAction::ApplySettings(Box::new(state.draft.clone())));
    }
    // The window's own close button and Cancel end the same way, and neither
    // writes anything: a draft that was never applied is simply dropped.
    if close || !open {
        state.open = false;
    }
}
