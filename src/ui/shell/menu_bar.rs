use eframe::egui;

use crate::i18n::{Locale, LocalizationManager, TextKey};
use crate::settings::Theme;

use super::{UiAction, UiState, docking::DockPanel};
use crate::ui::Projector;

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut UiState,
    status: &crate::snapshots::StatusSnapshot,
    history: &crate::snapshots::HistorySnapshot,
    hotkeys: &crate::hotkey::HotkeySettings,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    egui::Panel::top("menu_bar")
        .exact_size(28.0)
        .frame(egui::Frame::new().fill(ui.visuals().panel_fill))
        .show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button(i18n.text(TextKey::MenuFile), |ui| {
                    // Also on the Controls dock, and here because that dock
                    // can be closed: settings reachable only from something
                    // the user can put away is settings they can lose.
                    if ui.button(i18n.text(TextKey::MenuSettings)).clicked() {
                        actions.push(UiAction::OpenSettings);
                        ui.close();
                    }
                    // The one place the application says where it put the
                    // files it made. Otherwise that is a path on a settings
                    // page, to be read and typed somewhere else.
                    if ui.button(i18n.text(TextKey::MenuShowRecordings)).clicked() {
                        actions.push(UiAction::ShowRecordings);
                        ui.close();
                    }
                    // Beside the folder it saves into. A hotkey does the same
                    // from inside a game; this is where it can be found.
                    if ui.button(i18n.text(TextKey::MenuScreenshot)).clicked() {
                        actions.push(UiAction::TakeScreenshot);
                        ui.close();
                    }
                    // The Sources dock's right click does the same for any
                    // row; this is where it can be found without knowing
                    // that, and it acts on what is selected.
                    let selected = state.editor.selected_item_id();
                    if ui
                        .add_enabled(
                            selected.is_some(),
                            egui::Button::new(i18n.text(TextKey::MenuScreenshotSource)),
                        )
                        .clicked()
                        && let Some(item) = selected
                    {
                        actions.push(UiAction::TakeSourceScreenshot(item));
                        ui.close();
                    }
                    // Beside the screenshots, and for the same reason: the
                    // Controls dock's button can be closed away, and its
                    // hotkey is set nowhere by default. Only while the
                    // buffer holds a clip is there anything to save.
                    if (status.replay_enabled || status.replay.is_some())
                        && ui
                            .add_enabled(
                                status.replay.is_some_and(|fill| fill.saveable()),
                                egui::Button::new(i18n.text(TextKey::MenuSaveReplay)),
                            )
                            .clicked()
                    {
                        actions.push(UiAction::SaveReplay);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(i18n.text(TextKey::MenuExit)).clicked() {
                        actions.push(UiAction::Exit);
                        ui.close();
                    }
                });

                ui.menu_button(i18n.text(TextKey::MenuEdit), |ui| {
                    show_edit_menu(ui, history, hotkeys, i18n, actions);
                });

                ui.menu_button(i18n.text(TextKey::MenuView), |ui| {
                    if ui
                        .checkbox(&mut state.fullscreen, i18n.text(TextKey::MenuFullscreen))
                        .changed()
                    {
                        actions.push(UiAction::SetFullscreen(state.fullscreen));
                        ui.close();
                    }

                    ui.menu_button(i18n.text(TextKey::MenuProjector), |ui| {
                        show_projector_menu(ui, state, i18n);
                    });

                    ui.menu_button(i18n.text(TextKey::MenuDocks), |ui| {
                        for panel in DockPanel::ALL {
                            dock_option(ui, state, panel, i18n.text(panel.title()));
                        }
                    });

                    ui.menu_button(i18n.text(TextKey::MenuTheme), |ui| {
                        theme_option(ui, actions, Theme::System, i18n.text(TextKey::ThemeSystem));
                        theme_option(ui, actions, Theme::Light, i18n.text(TextKey::ThemeLight));
                        theme_option(ui, actions, Theme::Dark, i18n.text(TextKey::ThemeDark));
                    });

                    ui.menu_button(i18n.text(TextKey::MenuLanguage), |ui| {
                        for locale in Locale::ALL {
                            let key = match locale {
                                Locale::EnUs => TextKey::LanguageEnglish,
                                Locale::KoKr => TextKey::LanguageKorean,
                            };
                            if ui
                                .selectable_label(i18n.locale() == locale, i18n.text(key))
                                .clicked()
                            {
                                actions.push(UiAction::SetLocale(locale));
                                ui.close();
                            }
                        }
                    });
                });

                ui.menu_button(i18n.text(TextKey::MenuHelp), |ui| {
                    // Where both logs are — this application's and the
                    // library's — for whoever is asked what went wrong. A
                    // shipped build has no console, so this is the only way
                    // to them short of knowing the path.
                    if ui.button(i18n.text(TextKey::MenuShowLogs)).clicked() {
                        actions.push(UiAction::ShowLogs);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(i18n.text(TextKey::MenuAbout)).clicked() {
                        state.about_open = true;
                        ui.close();
                    }
                });
            });
        });
}

fn dock_option(
    ui: &mut egui::Ui,
    state: &mut UiState,
    panel: DockPanel,
    label: impl Into<egui::WidgetText>,
) {
    let mut open = state.dock_layout.is_open(panel);
    if ui.checkbox(&mut open, label).changed() {
        state.dock_layout.set_open(panel, open);
        ui.close();
    }
}

/// One theme entry, marked when it is the one in force.
///
/// The mark is read from egui rather than from any copy this module keeps.
/// `set_theme` writes exactly this, so it is the one answer that cannot drift
/// from what the window is actually drawing — which a second copy here did,
/// once the Settings dialog gained a way to change it too.
fn theme_option(
    ui: &mut egui::Ui,
    actions: &mut Vec<UiAction>,
    theme: Theme,
    label: impl Into<egui::WidgetText>,
) {
    let current: Theme = ui.ctx().options(|options| options.theme_preference).into();
    if ui.selectable_label(current == theme, label).clicked() {
        actions.push(UiAction::SetTheme(theme));
        ui.close();
    }
}

/// The About box: what this is, which build, and where it comes from.
pub fn show_about(ui: &mut egui::Ui, state: &mut UiState, i18n: &LocalizationManager) {
    if !state.about_open {
        return;
    }
    let shown = crate::ui::dialog::show(
        ui.ctx(),
        "about_dialog",
        &i18n.text(TextKey::MenuAbout),
        |ui| {
            ui.strong("obs-rs");
            ui.label(format!("v{}", env!("CARGO_PKG_VERSION")));
            ui.label(i18n.text(TextKey::AboutDescription));
            ui.add_space(4.0);
            // A link rather than a label to copy by hand. egui opens it in
            // the system browser, which is the only thing anybody wants from
            // an address in an About box.
            ui.hyperlink(REPOSITORY);
            ui.add_space(12.0);
            // Its way out, now that there is no title bar to close it from.
            ui.button(i18n.text(TextKey::ActionOk)).clicked()
        },
    );
    if shown.inner || shown.escaped {
        state.about_open = false;
    }
}

/// Where this application is developed. From the manifest rather than
/// written out here, so it stays whatever `cargo` publishes.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The screens a projector can fill, and the switch that opens one.
///
/// Toggles rather than opens: the screen a projector is already on is ticked,
/// and picking it again closes that window — the same shape the docks and
/// fullscreen above have.
///
/// A desktop that shows its own picker for captures cannot be asked which
/// screens it has either, so there the list is one entry: a window, which the
/// user puts where they want and fills the screen with themselves.
fn show_projector_menu(
    ui: &mut egui::Ui,
    state: &mut crate::ui::UiState,
    i18n: &LocalizationManager,
) {
    if let crate::capture::SourcePicker::Enumerated { monitors, .. } =
        crate::capture::source_picker()
    {
        for monitor in monitors {
            let open = matches!(
                &state.projector,
                Some(Projector::Screen { name, .. }) if *name == monitor.name
            );
            let label = format!(
                "{} — {}×{}",
                monitor.name, monitor.rect.width, monitor.rect.height
            );
            if ui.selectable_label(open, label).clicked() {
                state.show_projector((!open).then(|| Projector::Screen {
                    name: monitor.name.clone(),
                    x: monitor.rect.x,
                    y: monitor.rect.y,
                    width: monitor.rect.width,
                    height: monitor.rect.height,
                }));
                ui.close();
            }
        }
    }

    let windowed = matches!(state.projector, Some(Projector::Window));
    if ui
        .selectable_label(windowed, i18n.text(TextKey::MenuProjectorWindow))
        .clicked()
    {
        state.show_projector((!windowed).then_some(Projector::Window));
        ui.close();
    }
}

/// Undo and Redo, each naming the step it would move, with the key that
/// does the same.
///
/// The key is shown as it is bound rather than written in: it can be moved
/// on the Hotkeys page, and a menu that went on saying Ctrl+Z afterwards
/// would be telling somebody to press a key that no longer does it.
fn show_edit_menu(
    ui: &mut egui::Ui,
    history: &crate::snapshots::HistorySnapshot,
    hotkeys: &crate::hotkey::HotkeySettings,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    use crate::hotkey::HotkeyAction;
    use crate::project::ProjectCommand;

    for (label, with, without, action, command) in [
        (
            history.undo.as_ref(),
            TextKey::MenuUndo,
            TextKey::MenuUndoNothing,
            HotkeyAction::Undo,
            ProjectCommand::Undo,
        ),
        (
            history.redo.as_ref(),
            TextKey::MenuRedo,
            TextKey::MenuRedoNothing,
            HotkeyAction::Redo,
            ProjectCommand::Redo,
        ),
    ] {
        let text = super::edit::menu_item(label, with, without, i18n);
        let mut button = egui::Button::new(text);
        if let Some(chord) = hotkeys.binding(action) {
            button = button.shortcut_text(chord.to_string());
        }
        if ui.add_enabled(label.is_some(), button).clicked() {
            actions.push(UiAction::Project(command));
            ui.close();
        }
    }
}
