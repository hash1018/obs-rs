//! The menu bar: what is in it, and the one way it is drawn inside the
//! window.
//!
//! What the menus hold is [`model`], built afresh each pass from the state it
//! shows, and what picking an entry does is [`apply`]. Drawing is separate,
//! because a Mac draws it twice over: [`show`] puts it in the window with
//! egui, and on macOS `native_menu` puts the same model in the menu bar at
//! the top of the screen instead, where a Mac application's menus are. One
//! model is what keeps the two from listing different things.

use eframe::egui;

use crate::capture::MonitorTarget;
use crate::hotkey::{Chord, HotkeyAction, HotkeySettings};
use crate::i18n::{Locale, LocalizationManager, TextKey};
use crate::settings::Theme;
use crate::snapshots::{HistorySnapshot, StatusSnapshot};

use super::{UiAction, UiState, docking::DockPanel};
use crate::ui::Projector;

/// One menu in the bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    pub title: String,
    pub items: Vec<MenuItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MenuItem {
    /// An entry that does something. Greyed out when there is nothing for it
    /// to do, which is `command` being `None`.
    Command {
        label: String,
        command: Option<MenuCommand>,
        mark: Mark,
        /// The key that does the same, where it is the window's own — see
        /// [`window_shortcut`].
        shortcut: Option<Chord>,
    },
    Submenu {
        label: String,
        items: Vec<MenuItem>,
    },
    /// The screens a projector can fill, which [`projector_items`] lists
    /// only when somebody opens it: finding them out asks the system, which
    /// is not something to do every pass for a menu that is shut.
    Projectors {
        label: String,
    },
    Separator,
}

/// What an entry shows beside its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    None,
    /// A switch, on or off.
    Check(bool),
    /// One of several, and whether it is the one in force.
    Choice(bool),
}

/// What picking an entry does — see [`apply`].
#[derive(Debug, Clone, PartialEq)]
pub enum MenuCommand {
    Action(UiAction),
    Fullscreen(bool),
    Projector(Option<Projector>),
    Dock(DockPanel, bool),
    About,
}

/// Carries out what an entry was picked for.
pub fn apply(command: MenuCommand, state: &mut UiState, actions: &mut Vec<UiAction>) {
    match command {
        MenuCommand::Action(action) => actions.push(action),
        MenuCommand::Fullscreen(on) => {
            state.fullscreen = on;
            actions.push(UiAction::SetFullscreen(on));
        }
        MenuCommand::Projector(projector) => state.show_projector(projector),
        MenuCommand::Dock(panel, open) => state.dock_layout.set_open(panel, open),
        MenuCommand::About => state.about_open = true,
    }
}

/// What the menus hold now.
///
/// `theme` is the one in force, which is egui's — see [`show`].
pub fn model(
    state: &UiState,
    theme: Theme,
    status: &StatusSnapshot,
    history: &HistorySnapshot,
    hotkeys: &HotkeySettings,
    i18n: &LocalizationManager,
) -> Vec<Menu> {
    let command = |label: TextKey, command: Option<MenuCommand>| MenuItem::Command {
        label: owned(i18n, label),
        command,
        mark: Mark::None,
        shortcut: None,
    };
    let action =
        |label: TextKey, action: UiAction| command(label, Some(MenuCommand::Action(action)));

    let mut file = vec![
        // Also on the Controls dock, and here because that dock can be
        // closed: settings reachable only from something the user can put
        // away is settings they can lose.
        MenuItem::Command {
            label: owned(i18n, TextKey::MenuSettings),
            command: Some(MenuCommand::Action(UiAction::OpenSettings)),
            mark: Mark::None,
            shortcut: window_shortcut(hotkeys, HotkeyAction::OpenSettings),
        },
        // The one place the application says where it put the files it
        // made. Otherwise that is a path on a settings page, to be read and
        // typed somewhere else.
        action(TextKey::MenuShowRecordings, UiAction::ShowRecordings),
        // Beside the folder it saves into. A hotkey does the same from
        // inside a game; this is where it can be found.
        action(TextKey::MenuScreenshot, UiAction::TakeScreenshot),
        // The Sources dock's right click does the same for any row; this is
        // where it can be found without knowing that, and it acts on what is
        // selected.
        command(
            TextKey::MenuScreenshotSource,
            state
                .editor
                .selected_item_id()
                .map(|item| MenuCommand::Action(UiAction::TakeSourceScreenshot(item))),
        ),
    ];
    // Beside the screenshots, and for the same reason: the Controls dock's
    // button can be closed away, and its hotkey is set nowhere by default.
    // Only while the buffer holds a clip is there anything to save.
    if status.replay_enabled || status.replay.is_some() {
        file.push(command(
            TextKey::MenuSaveReplay,
            status
                .replay
                .is_some_and(|fill| fill.saveable())
                .then_some(MenuCommand::Action(UiAction::SaveReplay)),
        ));
    }
    file.push(MenuItem::Separator);
    file.push(action(TextKey::MenuExit, UiAction::Exit));

    let docks = DockPanel::ALL
        .into_iter()
        .map(|panel| {
            let open = state.dock_layout.is_open(panel);
            MenuItem::Command {
                label: owned(i18n, panel.title()),
                command: Some(MenuCommand::Dock(panel, !open)),
                mark: Mark::Check(open),
                shortcut: None,
            }
        })
        .collect();
    let themes = [
        (Theme::System, TextKey::ThemeSystem),
        (Theme::Light, TextKey::ThemeLight),
        (Theme::Dark, TextKey::ThemeDark),
    ]
    .into_iter()
    .map(|(each, label)| MenuItem::Command {
        label: owned(i18n, label),
        command: Some(MenuCommand::Action(UiAction::SetTheme(each))),
        mark: Mark::Choice(theme == each),
        shortcut: None,
    })
    .collect();
    let languages = Locale::ALL
        .into_iter()
        .map(|locale| MenuItem::Command {
            label: owned(
                i18n,
                match locale {
                    Locale::EnUs => TextKey::LanguageEnglish,
                    Locale::KoKr => TextKey::LanguageKorean,
                },
            ),
            command: Some(MenuCommand::Action(UiAction::SetLocale(locale))),
            mark: Mark::Choice(i18n.locale() == locale),
            shortcut: None,
        })
        .collect();

    vec![
        Menu {
            title: owned(i18n, TextKey::MenuFile),
            items: file,
        },
        Menu {
            title: owned(i18n, TextKey::MenuEdit),
            items: edit_items(history, hotkeys, i18n),
        },
        Menu {
            title: owned(i18n, TextKey::MenuView),
            items: vec![
                MenuItem::Command {
                    label: owned(i18n, TextKey::MenuFullscreen),
                    command: Some(MenuCommand::Fullscreen(!state.fullscreen)),
                    mark: Mark::Check(state.fullscreen),
                    shortcut: window_shortcut(hotkeys, HotkeyAction::Fullscreen),
                },
                MenuItem::Projectors {
                    label: owned(i18n, TextKey::MenuProjector),
                },
                MenuItem::Submenu {
                    label: owned(i18n, TextKey::MenuDocks),
                    items: docks,
                },
                MenuItem::Submenu {
                    label: owned(i18n, TextKey::MenuTheme),
                    items: themes,
                },
                MenuItem::Submenu {
                    label: owned(i18n, TextKey::MenuLanguage),
                    items: languages,
                },
            ],
        },
        Menu {
            title: owned(i18n, TextKey::MenuHelp),
            items: vec![
                // Where both logs are — this application's and the
                // library's — for whoever is asked what went wrong. A
                // shipped build has no console, so this is the only way to
                // them short of knowing the path.
                action(TextKey::MenuShowLogs, UiAction::ShowLogs),
                MenuItem::Separator,
                command(TextKey::MenuAbout, Some(MenuCommand::About)),
            ],
        },
    ]
}

/// A translated label, as the model keeps it.
fn owned(i18n: &LocalizationManager, key: TextKey) -> String {
    i18n.text(key).into_owned()
}

/// The key bound to one of the window's own actions — Settings, Fullscreen,
/// Undo, Redo — to be shown beside its entry.
///
/// Only those: a key that can be global is heard by whatever listens for it
/// there, and a menu claiming it too would be two answers to one press.
fn window_shortcut(hotkeys: &HotkeySettings, action: HotkeyAction) -> Option<Chord> {
    hotkeys.binding(action)
}

/// Draws the menu bar inside the window — everywhere but macOS, where
/// `native_menu` puts it in the system's menu bar.
#[cfg(not(target_os = "macos"))]
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut UiState,
    status: &StatusSnapshot,
    history: &HistorySnapshot,
    hotkeys: &HotkeySettings,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    // Read from egui rather than from any copy kept here. `set_theme` writes
    // exactly this, so it is the one answer that cannot drift from what the
    // window is actually drawing — which a second copy did, once the
    // Settings dialog gained a way to change it too.
    let theme: Theme = ui.ctx().options(|options| options.theme_preference).into();
    let menus = model(state, theme, status, history, hotkeys, i18n);
    let mut picked = Vec::new();
    egui::Panel::top("menu_bar")
        .exact_size(28.0)
        .frame(egui::Frame::new().fill(ui.visuals().panel_fill))
        .show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                for menu in &menus {
                    ui.menu_button(&menu.title, |ui| {
                        show_items(ui, &menu.items, state, i18n, &mut picked);
                    });
                }
            });
        });
    for command in picked {
        apply(command, state, actions);
    }
}

#[cfg(not(target_os = "macos"))]
fn show_items(
    ui: &mut egui::Ui,
    items: &[MenuItem],
    state: &UiState,
    i18n: &LocalizationManager,
    picked: &mut Vec<MenuCommand>,
) {
    for item in items {
        match item {
            MenuItem::Separator => {
                ui.separator();
            }
            MenuItem::Submenu { label, items } => {
                ui.menu_button(label, |ui| show_items(ui, items, state, i18n, picked));
            }
            MenuItem::Projectors { label } => {
                ui.menu_button(label, |ui| {
                    let monitors = match crate::capture::source_picker() {
                        crate::capture::SourcePicker::Enumerated { monitors, .. } => monitors,
                        crate::capture::SourcePicker::SystemDialog => Vec::new(),
                    };
                    show_items(
                        ui,
                        &projector_items(&monitors, state, i18n),
                        state,
                        i18n,
                        picked,
                    );
                });
            }
            MenuItem::Command {
                label,
                command,
                mark,
                shortcut,
            } => {
                let enabled = command.is_some();
                let clicked = match *mark {
                    Mark::Check(mut on) => ui
                        .add_enabled(enabled, egui::Checkbox::new(&mut on, label))
                        .clicked(),
                    Mark::Choice(on) => ui
                        .add_enabled(enabled, egui::Button::selectable(on, label))
                        .clicked(),
                    Mark::None => {
                        let mut button = egui::Button::new(label);
                        if let Some(chord) = shortcut {
                            button = button.shortcut_text(chord.label());
                        }
                        ui.add_enabled(enabled, button).clicked()
                    }
                };
                if clicked && let Some(command) = command {
                    picked.push(command.clone());
                    ui.close();
                }
            }
        }
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
/// fullscreen have.
///
/// A desktop that shows its own picker for captures cannot be asked which
/// screens it has either, so there `monitors` is empty and the list is one
/// entry: a window, which the user puts where they want and fills the screen
/// with themselves.
pub fn projector_items(
    monitors: &[MonitorTarget],
    state: &UiState,
    i18n: &LocalizationManager,
) -> Vec<MenuItem> {
    let mut items: Vec<MenuItem> = monitors
        .iter()
        .map(|monitor| {
            let open = matches!(
                &state.projector,
                Some(Projector::Screen { name, .. }) if *name == monitor.name
            );
            MenuItem::Command {
                label: format!(
                    "{} — {}×{}",
                    monitor.name, monitor.rect.width, monitor.rect.height
                ),
                command: Some(MenuCommand::Projector((!open).then(|| Projector::Screen {
                    name: monitor.name.clone(),
                    x: monitor.rect.x,
                    y: monitor.rect.y,
                    width: monitor.rect.width,
                    height: monitor.rect.height,
                }))),
                mark: Mark::Choice(open),
                shortcut: None,
            }
        })
        .collect();
    let windowed = matches!(state.projector, Some(Projector::Window));
    items.push(MenuItem::Command {
        label: owned(i18n, TextKey::MenuProjectorWindow),
        command: Some(MenuCommand::Projector(
            (!windowed).then_some(Projector::Window),
        )),
        mark: Mark::Choice(windowed),
        shortcut: None,
    });
    items
}

/// Undo and Redo, each naming the step it would move, with the key that
/// does the same.
///
/// The key is shown as it is bound rather than written in: it can be moved
/// on the Hotkeys page, and a menu that went on saying Ctrl+Z afterwards
/// would be telling somebody to press a key that no longer does it.
fn edit_items(
    history: &HistorySnapshot,
    hotkeys: &HotkeySettings,
    i18n: &LocalizationManager,
) -> Vec<MenuItem> {
    use crate::project::ProjectCommand;

    [
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
    ]
    .into_iter()
    .map(
        |(label, with, without, action, command)| MenuItem::Command {
            label: super::edit::menu_item(label, with, without, i18n),
            command: label.map(|_| MenuCommand::Action(UiAction::Project(command))),
            mark: Mark::None,
            shortcut: window_shortcut(hotkeys, action),
        },
    )
    .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use eframe::egui::Key;

    use super::*;
    use crate::snapshots::ReplayFill;

    fn find<'a>(menus: &'a [Menu], wanted: &str) -> Option<&'a MenuItem> {
        fn search<'a>(items: &'a [MenuItem], wanted: &str) -> Option<&'a MenuItem> {
            items.iter().find_map(|item| match item {
                MenuItem::Command { label, .. } if label == wanted => Some(item),
                MenuItem::Submenu { items, .. } => search(items, wanted),
                _ => None,
            })
        }
        menus.iter().find_map(|menu| search(&menu.items, wanted))
    }

    fn command(item: Option<&MenuItem>) -> Option<&MenuCommand> {
        match item {
            Some(MenuItem::Command { command, .. }) => command.as_ref(),
            other => panic!("not an entry: {other:?}"),
        }
    }

    /// What is greyed out and what is left out follow the state, as they did
    /// when the menu was drawn straight from it: nothing selected greys out
    /// the source's screenshot, an empty history greys out Undo, and Save
    /// Replay is there only while the buffer is, and live only once it holds
    /// a clip.
    #[test]
    fn the_menus_follow_what_there_is_to_do() {
        let i18n = LocalizationManager::new(Locale::EnUs);
        let state = UiState::default();
        let hotkeys = HotkeySettings::default();
        let history = HistorySnapshot::default();
        let mut status = StatusSnapshot::default();
        let menus = |status: &StatusSnapshot| {
            model(&state, Theme::System, status, &history, &hotkeys, &i18n)
        };

        let idle = menus(&status);
        assert_eq!(
            command(find(&idle, "Settings")),
            Some(&MenuCommand::Action(UiAction::OpenSettings))
        );
        assert_eq!(
            command(find(&idle, "Save Screenshot of Selected Source")),
            None
        );
        assert!(find(&idle, "Save Replay").is_none());
        let Some(MenuItem::Command { shortcut, .. }) = find(&idle, "Undo") else {
            panic!("no Undo");
        };
        assert_eq!(*shortcut, Some(Chord::ctrl(Key::Z)));
        assert_eq!(command(find(&idle, "Undo")), None);

        status.replay_enabled = true;
        status.replay = Some(ReplayFill {
            buffered: Duration::ZERO,
            length: Duration::from_secs(30),
        });
        assert_eq!(command(find(&menus(&status), "Save Replay")), None);
        status.replay = Some(ReplayFill {
            buffered: Duration::from_secs(30),
            length: Duration::from_secs(30),
        });
        assert_eq!(
            command(find(&menus(&status), "Save Replay")),
            Some(&MenuCommand::Action(UiAction::SaveReplay))
        );
    }

    /// Fullscreen is a switch the window keeps, and turning it on through the
    /// menu is the same as through its key.
    #[test]
    fn picking_fullscreen_switches_it_and_asks_the_window() {
        let mut state = UiState::default();
        let mut actions = Vec::new();
        apply(MenuCommand::Fullscreen(true), &mut state, &mut actions);
        assert!(state.fullscreen);
        assert_eq!(actions, [UiAction::SetFullscreen(true)]);
    }
}
