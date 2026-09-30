//! The menu bar on macOS: the menus at the top of the screen, where a Mac
//! application's are, in place of the ones the other platforms draw inside
//! the window.
//!
//! What is in them is `menu_bar::model`, the same as everywhere, laid out as
//! a Mac lays out an application's menus: an application menu first, holding
//! About, Settings, hiding and Quit — which come out of File and Help to go
//! there — and a Window menu before Help. What an entry does is
//! `menu_bar::apply`, reached from AppKit through [`Target`] and handed back
//! on the next pass.
//!
//! # Keys
//!
//! An entry with a key equivalent takes that key before the window sees it,
//! which is what shows the key beside the entry and what a Mac user expects
//! of it. Only the window's own actions carry one — see `menu_bar::
//! window_shortcut` — and none of them while something takes typed input, so
//! ⌘Z in a text field is the text field's undo rather than the project's, as
//! in the window's own handling of the same keys. Hide and Quit keep theirs
//! throughout, as they do in every Mac application.
//!
//! # Kept in step, not rebuilt
//!
//! The model is made every pass. The native menus change only when it does,
//! and then in place where only titles, marks and keys moved — renaming
//! Undo after every edit must not make the menu bar flicker — and rebuilt
//! only when an entry came or went.

use std::cell::RefCell;

use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSEventModifierFlags, NSMenu,
    NSMenuItem,
};
use objc2_foundation::NSString;

use super::menu_bar::{self, Mark, Menu, MenuCommand, MenuItem};
use crate::hotkey::{Chord, HotkeySettings};
use crate::i18n::{LocalizationManager, TextKey};
use crate::settings::Theme;
use crate::snapshots::{HistorySnapshot, StatusSnapshot};
use crate::ui::{UiAction, UiState};

/// Puts the menus in the menu bar, and carries out whatever was picked from
/// them since the last pass.
///
/// Called every pass, in place of `menu_bar::show`.
pub fn show(
    ctx: &egui::Context,
    state: &mut UiState,
    status: &StatusSnapshot,
    history: &HistorySnapshot,
    hotkeys: &HotkeySettings,
    i18n: &LocalizationManager,
    actions: &mut Vec<UiAction>,
) {
    // The window's own frame runs on the main thread on macOS, which AppKit
    // requires of anything touching a menu; this is never `None` there.
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let theme: Theme = ctx.options(|options| options.theme_preference).into();
    let menus = menu_bar::model(state, theme, status, history, hotkeys, i18n);
    let typing = super::hotkeys::keyboard_taken(ctx, state);
    let tree = arrange(menus, state, i18n, typing);
    let picked = BAR.with(|bar| {
        let mut bar = bar.borrow_mut();
        let bar = bar.get_or_insert_with(|| Bar::new(main_thread, ctx.clone()));
        bar.sync(main_thread, tree);
        bar.target.ivars().picked.take()
    });
    for command in picked {
        menu_bar::apply(command, state, actions);
    }
}

thread_local! {
    /// The menu bar's one instance. Thread-local because AppKit's menus
    /// belong to the main thread and so does everything here.
    static BAR: RefCell<Option<Bar>> = const { RefCell::new(None) };
}

/// An entry as the native menus are built from: `menu_bar::MenuItem` with
/// the Mac's own entries beside it, and each key already turned into what
/// AppKit takes.
#[derive(Debug, Clone, PartialEq)]
enum Entry {
    Item {
        label: String,
        does: Does,
        mark: Mark,
        key: Option<KeyEquivalent>,
    },
    Submenu {
        label: String,
        items: Vec<Entry>,
    },
    Separator,
}

#[derive(Debug, Clone, PartialEq)]
enum Does {
    /// One of this application's own, greyed out when `None`.
    Command(Option<MenuCommand>),
    /// One the system answers itself — hiding, minimising — sent to whatever
    /// takes it, the application or the window in front.
    System(Sel),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyEquivalent {
    key: String,
    modifiers: usize,
}

impl KeyEquivalent {
    fn command(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            modifiers: NSEventModifierFlags::Command.0,
        }
    }
}

/// One menu of the bar: its title, and what it holds.
type TopLevel = (String, Vec<Entry>);

/// Lays the model out as a Mac application's menu bar.
fn arrange(
    menus: Vec<Menu>,
    state: &UiState,
    i18n: &LocalizationManager,
    typing: bool,
) -> Vec<TopLevel> {
    let text = |key: TextKey| i18n.text(key).into_owned();
    let mut settings = None;
    let mut about = None;
    let mut quit = None;
    let mut bar: Vec<TopLevel> = Vec::new();
    for menu in menus {
        let mut entries = Vec::new();
        for item in menu.items {
            // The three that move to the application menu, found by what
            // they do rather than where the model happens to put them.
            if let MenuItem::Command { command, .. } = &item {
                let slot = match command {
                    Some(MenuCommand::Action(UiAction::OpenSettings)) => Some(&mut settings),
                    Some(MenuCommand::About) => Some(&mut about),
                    Some(MenuCommand::Action(UiAction::Exit)) => Some(&mut quit),
                    _ => None,
                };
                if let Some(slot) = slot {
                    *slot = Some(entry(item, state, i18n, typing));
                    continue;
                }
            }
            entries.push(entry(item, state, i18n, typing));
        }
        bar.push((menu.title, tidy(entries)));
    }

    let system = |label: TextKey, selector: Sel, key: Option<KeyEquivalent>| Entry::Item {
        label: text(label),
        does: Does::System(selector),
        mark: Mark::None,
        key,
    };
    let mut application = Vec::new();
    application.extend(about);
    application.push(Entry::Separator);
    application.extend(settings);
    application.push(Entry::Separator);
    application.push(system(
        TextKey::MenuHideApp,
        sel!(hide:),
        Some(KeyEquivalent::command("h")),
    ));
    application.push(system(
        TextKey::MenuHideOthers,
        sel!(hideOtherApplications:),
        Some(KeyEquivalent {
            key: "h".to_owned(),
            modifiers: (NSEventModifierFlags::Command | NSEventModifierFlags::Option).0,
        }),
    ));
    application.push(system(
        TextKey::MenuShowAll,
        sel!(unhideAllApplications:),
        None,
    ));
    application.push(Entry::Separator);
    if let Some(Entry::Item { does, .. }) = quit {
        // Named as a Mac names it, and on ⌘Q whatever the bindings say —
        // through the application's own Exit, which asks first when a
        // recording would be cut short.
        application.push(Entry::Item {
            label: text(TextKey::MenuQuitApp),
            does,
            mark: Mark::None,
            key: Some(KeyEquivalent::command("q")),
        });
    }
    // The title of the application menu is not shown — the system puts the
    // application's name there — but is set to that name all the same.
    bar.insert(0, ("obs-rs".to_owned(), tidy(application)));

    let window = vec![
        system(
            TextKey::MenuMinimize,
            sel!(performMiniaturize:),
            Some(KeyEquivalent::command("m")),
        ),
        system(TextKey::MenuZoom, sel!(performZoom:), None),
    ];
    let help = bar.len() - 1;
    bar.insert(help, (text(TextKey::MenuWindow), window));
    bar
}

/// A model entry as a native one.
fn entry(item: MenuItem, state: &UiState, i18n: &LocalizationManager, typing: bool) -> Entry {
    match item {
        MenuItem::Command {
            label,
            command,
            mark,
            shortcut,
        } => Entry::Item {
            label,
            does: Does::Command(command),
            mark,
            key: shortcut.filter(|_| !typing).and_then(key_equivalent),
        },
        MenuItem::Submenu { label, items } => Entry::Submenu {
            label,
            items: items
                .into_iter()
                .map(|item| entry(item, state, i18n, typing))
                .collect(),
        },
        // Listed every pass rather than when opened: the displays are a
        // cheap question on a Mac — see `capture::macos::monitors` — and
        // AppKit gives no moment between a click on the menu and its opening
        // at which the window's state could be read.
        MenuItem::Projectors { label } => Entry::Submenu {
            label,
            items: menu_bar::projector_items(&crate::capture::macos::monitors(), state, i18n)
                .into_iter()
                .map(|item| entry(item, state, i18n, typing))
                .collect(),
        },
        MenuItem::Separator => Entry::Separator,
    }
}

/// Without a separator first, last, or beside another: what moving entries
/// out of a menu leaves behind.
fn tidy(entries: Vec<Entry>) -> Vec<Entry> {
    let mut tidied: Vec<Entry> = Vec::with_capacity(entries.len());
    for entry in entries {
        let separator = matches!(entry, Entry::Separator);
        let after_separator = matches!(tidied.last(), None | Some(Entry::Separator));
        if separator && after_separator {
            continue;
        }
        tidied.push(entry);
    }
    if matches!(tidied.last(), Some(Entry::Separator)) {
        tidied.pop();
    }
    tidied
}

/// What AppKit takes for a chord, or `None` for a key it has no way to
/// write — which then stays the window's, working and unlisted.
///
/// A chord's Ctrl is Command on a Mac, as in the window (`Chord::modifiers`).
fn key_equivalent(chord: Chord) -> Option<KeyEquivalent> {
    use eframe::egui::Key;

    let name = chord.key.name();
    let mut letters = name.chars();
    let key = match (letters.next(), letters.next()) {
        (Some(only), None) if only.is_ascii_alphanumeric() => only.to_ascii_lowercase().to_string(),
        _ => {
            let function = |offset: u32| char::from_u32(0xF704 + offset).map(String::from);
            if let Some(number) = name.strip_prefix('F').and_then(|n| n.parse::<u32>().ok())
                && (1..=20).contains(&number)
            {
                function(number - 1)?
            } else {
                let key = match chord.key {
                    Key::ArrowUp => '\u{F700}',
                    Key::ArrowDown => '\u{F701}',
                    Key::ArrowLeft => '\u{F702}',
                    Key::ArrowRight => '\u{F703}',
                    Key::Insert => '\u{F727}',
                    Key::Delete => '\u{F728}',
                    Key::Home => '\u{F729}',
                    Key::End => '\u{F72B}',
                    Key::PageUp => '\u{F72C}',
                    Key::PageDown => '\u{F72D}',
                    Key::Enter => '\r',
                    Key::Tab => '\t',
                    Key::Escape => '\u{1B}',
                    Key::Backspace => '\u{8}',
                    Key::Space => ' ',
                    Key::Comma => ',',
                    Key::Period => '.',
                    Key::Minus => '-',
                    Key::Equals => '=',
                    Key::Semicolon => ';',
                    Key::Quote => '\'',
                    Key::Slash => '/',
                    Key::Backslash => '\\',
                    Key::Backtick => '`',
                    Key::OpenBracket => '[',
                    Key::CloseBracket => ']',
                    _ => return None,
                };
                key.to_string()
            }
        }
    };
    let mut modifiers = NSEventModifierFlags::empty();
    for (held, flag) in [
        (chord.ctrl, NSEventModifierFlags::Command),
        (chord.shift, NSEventModifierFlags::Shift),
        (chord.alt, NSEventModifierFlags::Option),
    ] {
        if held {
            modifiers |= flag;
        }
    }
    Some(KeyEquivalent {
        key,
        modifiers: modifiers.0,
    })
}

/// The installed menu bar, and what it was built from.
struct Bar {
    target: Retained<Target>,
    shown: Option<Vec<TopLevel>>,
    main_menu: Option<Retained<NSMenu>>,
}

impl Bar {
    fn new(main_thread: MainThreadMarker, ctx: egui::Context) -> Self {
        Self {
            target: Target::new(main_thread, ctx),
            shown: None,
            main_menu: None,
        }
    }

    fn sync(&mut self, main_thread: MainThreadMarker, tree: Vec<TopLevel>) {
        if self.shown.as_ref() == Some(&tree) {
            return;
        }
        let mut table = Vec::new();
        match (&self.main_menu, &self.shown) {
            (Some(main_menu), Some(shown)) if same_shape_top(shown, &tree) => {
                for (index, (title, entries)) in tree.iter().enumerate() {
                    let Some(item) = main_menu.itemAtIndex(index as isize) else {
                        continue;
                    };
                    item.setTitle(&NSString::from_str(title));
                    if let Some(menu) = item.submenu() {
                        menu.setTitle(&NSString::from_str(title));
                        self.update(&menu, entries, &mut table);
                    }
                }
            }
            _ => {
                let main_menu = NSMenu::new(main_thread);
                let application = NSApplication::sharedApplication(main_thread);
                let count = tree.len();
                for (index, (title, entries)) in tree.iter().enumerate() {
                    let menu = self.build(main_thread, title, entries, &mut table);
                    let item = NSMenuItem::new(main_thread);
                    item.setTitle(&NSString::from_str(title));
                    item.setSubmenu(Some(&menu));
                    main_menu.addItem(&item);
                    // The Window menu is second to last, before Help; the
                    // system lists the open windows in it and gives Help its
                    // search field.
                    if index + 2 == count {
                        application.setWindowsMenu(Some(&menu));
                    } else if index + 1 == count {
                        application.setHelpMenu(Some(&menu));
                    }
                }
                application.setMainMenu(Some(&main_menu));
                self.main_menu = Some(main_menu);
            }
        }
        *self.target.ivars().table.borrow_mut() = table;
        self.shown = Some(tree);
    }

    fn build(
        &self,
        main_thread: MainThreadMarker,
        title: &str,
        entries: &[Entry],
        table: &mut Vec<Option<MenuCommand>>,
    ) -> Retained<NSMenu> {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(main_thread), &NSString::from_str(title));
        // Enabled as the model says, not as AppKit would guess from who
        // answers each action.
        menu.setAutoenablesItems(false);
        for entry in entries {
            let item = match entry {
                Entry::Separator => NSMenuItem::separatorItem(main_thread),
                Entry::Submenu { label, items } => {
                    let item = NSMenuItem::new(main_thread);
                    item.setTitle(&NSString::from_str(label));
                    item.setSubmenu(Some(&self.build(main_thread, label, items, table)));
                    item
                }
                Entry::Item { .. } => {
                    let item = NSMenuItem::new(main_thread);
                    self.set(&item, entry, table);
                    item
                }
            };
            menu.addItem(&item);
        }
        menu
    }

    /// Brings a menu of the same shape up to date.
    fn update(&self, menu: &NSMenu, entries: &[Entry], table: &mut Vec<Option<MenuCommand>>) {
        for (index, entry) in entries.iter().enumerate() {
            let Some(item) = menu.itemAtIndex(index as isize) else {
                continue;
            };
            match entry {
                Entry::Separator => {}
                Entry::Submenu { label, items } => {
                    item.setTitle(&NSString::from_str(label));
                    if let Some(submenu) = item.submenu() {
                        submenu.setTitle(&NSString::from_str(label));
                        self.update(&submenu, items, table);
                    }
                }
                Entry::Item { .. } => self.set(&item, entry, table),
            }
        }
    }

    /// Makes `item` say and do what `entry` does. The item's tag is its
    /// place in `table`, which is how [`Target`] finds what it does.
    fn set(&self, item: &NSMenuItem, entry: &Entry, table: &mut Vec<Option<MenuCommand>>) {
        let Entry::Item {
            label,
            does,
            mark,
            key,
        } = entry
        else {
            return;
        };
        item.setTitle(&NSString::from_str(label));
        item.setState(match mark {
            Mark::Check(true) | Mark::Choice(true) => NSControlStateValueOn,
            _ => NSControlStateValueOff,
        });
        match key {
            Some(key) => {
                item.setKeyEquivalent(&NSString::from_str(&key.key));
                item.setKeyEquivalentModifierMask(NSEventModifierFlags(key.modifiers));
            }
            None => item.setKeyEquivalent(&NSString::from_str("")),
        }
        match does {
            Does::Command(command) => {
                item.setTag(table.len() as isize);
                item.setEnabled(command.is_some());
                table.push(command.clone());
                // SAFETY: `picked:` is `Target`'s, taking the sending item,
                // and the target outlives every item it is set on — both
                // live in `BAR` for the rest of the process.
                unsafe {
                    item.setAction(Some(sel!(picked:)));
                    item.setTarget(Some(&self.target));
                }
            }
            Does::System(selector) => {
                item.setEnabled(true);
                // SAFETY: a standard responder action, sent to the first
                // object in the responder chain that answers it.
                unsafe {
                    item.setAction(Some(*selector));
                    item.setTarget(None);
                }
            }
        }
    }
}

fn same_shape_top(a: &[TopLevel], b: &[TopLevel]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|((_, a), (_, b))| same_shape(a, b))
}

/// Whether two lists of entries differ only in what each says, not in what
/// entries there are.
fn same_shape(a: &[Entry], b: &[Entry]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|pair| match pair {
            (Entry::Separator, Entry::Separator) => true,
            (Entry::Item { does: a, .. }, Entry::Item { does: b, .. }) => {
                matches!(
                    (a, b),
                    (Does::Command(_), Does::Command(_)) | (Does::System(_), Does::System(_))
                )
            }
            (Entry::Submenu { items: a, .. }, Entry::Submenu { items: b, .. }) => same_shape(a, b),
            _ => false,
        })
}

struct TargetIvars {
    /// What each tagged item does, by tag.
    table: RefCell<Vec<Option<MenuCommand>>>,
    /// Picked since the last pass took them.
    picked: RefCell<Vec<MenuCommand>>,
    /// To wake the window, which otherwise draws its next pass only when
    /// something else asks it to.
    ctx: egui::Context,
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements, and `Target` does
    // not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = TargetIvars]
    /// What this application's menu items send their action to.
    struct Target;

    impl Target {
        #[unsafe(method(picked:))]
        fn picked(&self, item: &NSMenuItem) {
            let command = usize::try_from(item.tag())
                .ok()
                .and_then(|tag| self.ivars().table.borrow().get(tag).cloned().flatten());
            if let Some(command) = command {
                self.ivars().picked.borrow_mut().push(command);
                self.ivars().ctx.request_repaint();
            }
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

impl Target {
    fn new(main_thread: MainThreadMarker, ctx: egui::Context) -> Retained<Self> {
        let this = Self::alloc(main_thread).set_ivars(TargetIvars {
            table: RefCell::new(Vec::new()),
            picked: RefCell::new(Vec::new()),
            ctx,
        });
        // SAFETY: `NSObject`'s designated initialiser, on a fresh allocation
        // whose ivars are set.
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::Key;

    use super::*;

    fn item(label: &str) -> Entry {
        Entry::Item {
            label: label.to_owned(),
            does: Does::Command(None),
            mark: Mark::None,
            key: None,
        }
    }

    /// Moving About, Settings and Exit out of their menus leaves no
    /// separator hanging at either end or doubled in the middle.
    #[test]
    fn a_menu_emptied_of_entries_keeps_no_stray_separators() {
        let tidied = tidy(vec![
            Entry::Separator,
            item("a"),
            Entry::Separator,
            Entry::Separator,
            item("b"),
            Entry::Separator,
        ]);
        assert_eq!(tidied, [item("a"), Entry::Separator, item("b")]);
        assert_eq!(tidy(vec![Entry::Separator]), []);
    }

    /// A chord's Ctrl is Command, as in the window, and each key is written
    /// as AppKit reads it.
    #[test]
    fn a_chord_is_the_key_equivalent_a_mac_would_show() {
        let command_z = key_equivalent(Chord::ctrl(Key::Z)).unwrap();
        assert_eq!(command_z.key, "z");
        assert_eq!(command_z.modifiers, NSEventModifierFlags::Command.0);

        let all = key_equivalent(Chord {
            key: Key::Comma,
            ctrl: true,
            shift: true,
            alt: true,
        })
        .unwrap();
        assert_eq!(all.key, ",");
        assert_eq!(
            all.modifiers,
            (NSEventModifierFlags::Command
                | NSEventModifierFlags::Shift
                | NSEventModifierFlags::Option)
                .0
        );

        let f11 = key_equivalent(Chord::plain(Key::F11)).unwrap();
        assert_eq!(f11.key, "\u{F70E}");
        assert_eq!(f11.modifiers, 0);
        assert_eq!(key_equivalent(Chord::plain(Key::Num1)).unwrap().key, "1");
        assert_eq!(key_equivalent(Chord::plain(Key::Colon)), None);
    }

    /// Only a change of what entries there are rebuilds the bar; a label or
    /// a mark is changed where it stands.
    #[test]
    fn a_renamed_entry_is_the_same_shape_and_a_new_one_is_not() {
        let before = vec![item("Undo"), Entry::Separator];
        let renamed = vec![item("Undo Move"), Entry::Separator];
        let longer = vec![item("Undo"), Entry::Separator, item("Redo")];
        assert!(same_shape(&before, &renamed));
        assert!(!same_shape(&before, &longer));
    }
}
