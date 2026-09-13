//! Hotkeys that work while another application has focus.
//!
//! What a recorder needs most from a key is to be reachable from inside the
//! thing being recorded: a game has the keyboard, and a push-to-talk key
//! that only works while obs-rs is in front is no push-to-talk at all.
//!
//! # Asked, not hooked
//!
//! On Windows a thread of its own asks the system whether each bound key is
//! down, a hundred times a second, and turns what changed into [`Edge`]s —
//! the way OBS does it. The two alternatives each lose something this
//! needs. `RegisterHotKey` reports a press and never a release, so it cannot
//! hold push-to-talk open; a low-level keyboard hook sees both, but runs
//! inside every keystroke the machine makes, and a hook that is slow to
//! answer is one Windows quietly removes. Asking costs a few calls every
//! ten milliseconds and cannot slow anybody's typing down.
//!
//! One thing it cannot see: keys pressed in a window running as
//! administrator, which Windows keeps from an ordinary process's view. A
//! game run elevated needs obs-rs run elevated too, as it does for OBS.
//!
//! # Linux: asked for, and allowed
//!
//! Wayland lets no client read another's keys, which is the point of it, so
//! nothing can be asked the way Windows is asked. What it offers instead is
//! the `GlobalShortcuts` portal: an application names the shortcuts it wants
//! and the key it would like for each, the desktop asks the person at the
//! keyboard whether to allow them — once, in a dialog of its own — and from
//! then on says when each goes down and when it comes back up. Both, which
//! is what push-to-talk needs, and why this is the portal rather than a key
//! grab on X11, which would see nothing a native Wayland window has focus
//! for.
//!
//! Three things follow from the desktop owning the keys.
//!
//! The key it uses is the one it was allowed with, which the dialog lets
//! somebody change and the desktop's own keyboard settings let them change
//! later. What the Settings page binds is a *preference*; it is honoured
//! when the dialog is simply accepted.
//!
//! A shortcut the desktop holds is taken from every application, this one
//! included — a plain letter bound for push-to-talk is a letter nobody can
//! type while obs-rs runs. That is how every desktop's own shortcuts behave,
//! and the reason a push-to-talk key is usually one nothing else wants.
//!
//! And the portal only answers an application it can name. A process not
//! run from a sandbox names itself through `org.freedesktop.host.portal.
//! Registry`, which accepts [`crate::APP_ID`] only once the desktop entry
//! of that name is installed. Without it nothing is bound, [`GlobalHotkeys::
//! spawn`] answers `None`, and every hotkey stays the window's, as before.
//!
//! # Elsewhere
//!
//! Anywhere else — or on a Linux desktop without the portal — `spawn`
//! answers `None`, which leaves every hotkey to the window.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use super::tracker::Edge;
use super::{Chord, Hotkey};

/// One global hotkey, as the listener is told about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Bound {
    pub hotkey: Hotkey,
    pub chord: Chord,
    /// What to call it where the system shows it: the dialog asking to allow
    /// it, and the desktop's own list of shortcuts. The Settings page's name
    /// for it — see `ui::hotkey_label`. Unused where nothing is shown.
    pub description: String,
}

/// The listener, and the way to tell it what to listen for.
///
/// Dropping it stops the thread and waits for it.
pub struct GlobalHotkeys {
    shared: Arc<Shared>,
    edges: mpsc::Receiver<Edge>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct Shared {
    bound: Mutex<Vec<Bound>>,
    /// Moves every time `bound` does, so a listener that only needs to know
    /// *whether* anything changed can tell without comparing.
    generation: AtomicU64,
    /// Whether this application's own window has the keyboard for typing,
    /// in which case nothing may go down — see `tracker::Tracker::update`.
    typing: AtomicBool,
    stop: AtomicBool,
    /// Whether this application's window has focus — see [`GlobalHotkeys::
    /// set_focused`].
    focused: AtomicBool,
    /// The hotkeys whose keys reach this listener now — see
    /// [`GlobalHotkeys::taken`].
    taken: Mutex<HashSet<Hotkey>>,
}

impl GlobalHotkeys {
    /// Starts listening, or answers `None` where there is no way to.
    ///
    /// `window` is this application's own, for the one platform that asks
    /// somebody before a shortcut is bound: the dialog doing the asking is
    /// shown over it, rather than behind whatever happens to have focus —
    /// measured, a dialog with no parent opened unseen behind the editor in
    /// front. Unused elsewhere.
    ///
    /// `wake` is called whenever something went down or came up, from the
    /// listener's thread; the edges themselves wait in [`Self::edges`].
    #[cfg(target_os = "windows")]
    pub fn spawn(
        _window: &(impl HasWindowHandle + HasDisplayHandle),
        wake: impl Fn() + Send + 'static,
    ) -> Option<Self> {
        let shared = Arc::new(Shared::default());
        let (sender, edges) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("global-hotkeys".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || {
                    let mut tracker = super::tracker::Tracker::default();
                    while !shared.stop.load(Ordering::Acquire) {
                        // Copied out rather than held: a settings change
                        // waiting on this lock for a whole look would be a
                        // dialog that stalls.
                        let bound: Vec<(Hotkey, Chord)> = shared
                            .bound
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .iter()
                            .map(|bound| (bound.hotkey, bound.chord))
                            .collect();
                        let typing = shared.typing.load(Ordering::Acquire);
                        let changed = tracker.update(&bound, &windows::SystemKeyboard, !typing);
                        if !changed.is_empty() {
                            for edge in changed {
                                let _ = sender.send(edge);
                            }
                            wake();
                        }
                        std::thread::sleep(POLL_INTERVAL);
                    }
                }
            })
            .inspect_err(|error| eprintln!("could not start global hotkeys: {error}"))
            .ok()?;
        Some(Self {
            shared,
            edges,
            worker: Some(worker),
        })
    }

    #[cfg(target_os = "linux")]
    pub fn spawn(
        window: &(impl HasWindowHandle + HasDisplayHandle),
        wake: impl Fn() + Send + 'static,
    ) -> Option<Self> {
        let listener = portal::Listener::connect(window)?;
        let shared = Arc::new(Shared::default());
        let (sender, edges) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("global-hotkeys".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || listener.run(&shared, &sender, &wake)
            })
            .inspect_err(|error| eprintln!("could not start global hotkeys: {error}"))
            .ok()?;
        Some(Self {
            shared,
            edges,
            worker: Some(worker),
        })
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    pub fn spawn(
        _window: &(impl HasWindowHandle + HasDisplayHandle),
        _wake: impl Fn() + Send + 'static,
    ) -> Option<Self> {
        None
    }

    /// Listens for `bound` from now on — the global hotkeys among them,
    /// which is all of them but the window's own.
    ///
    /// Cheap to call with what it already has, which is how it is meant to
    /// be called: every pass, so a Scene renamed renames its shortcut too.
    pub fn set_bindings(&self, bound: Vec<Bound>) {
        let bound: Vec<Bound> = bound
            .into_iter()
            .filter(|bound| bound.hotkey.is_global())
            .collect();
        let mut current = self
            .shared
            .bound
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *current != bound {
            // Nothing to allow on Windows: every key on the machine can be
            // asked about, so every global hotkey is held the moment it is
            // bound.
            if cfg!(target_os = "windows") {
                *lock(&self.shared.taken) = bound.iter().map(|bound| bound.hotkey).collect();
            }
            *current = bound;
            self.shared.generation.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Whether this application's window is taking typed input right now.
    pub fn set_typing(&self, typing: bool) {
        self.shared.typing.store(typing, Ordering::Release);
    }

    /// Whether this application's window has focus.
    ///
    /// Used on Linux, where binding a new shortcut brings up a dialog shown
    /// over this window: bound while somebody is looking at another, it
    /// opens behind that one where nobody knows to look for it — measured,
    /// behind the editor in front. So a binding waits for the window to
    /// have focus, which at startup is at once.
    pub fn set_focused(&self, focused: bool) {
        self.shared.focused.store(focused, Ordering::Release);
    }

    /// The hotkeys whose keys reach this listener now. The window hears
    /// every other one itself.
    ///
    /// Every global hotkey, on Windows. On Linux only those the desktop has
    /// allowed *with a key*: none until it answers, none for good if it is
    /// told no, and not one it allowed with no key assigned — each of which
    /// would otherwise be a hotkey that works nowhere, where leaving it to
    /// the window is one that works while obs-rs has focus.
    pub fn taken(&self) -> HashSet<Hotkey> {
        lock(&self.shared.taken).clone()
    }

    /// Everything that went down or came up since the last call.
    pub fn edges(&self) -> Vec<Edge> {
        self.edges.try_iter().collect()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Drop for GlobalHotkeys {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// How often the listener looks: often enough that push-to-talk opens
/// before the first syllable is out, rarely enough to cost nothing.
#[cfg(target_os = "windows")]
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

#[cfg(target_os = "windows")]
mod windows {
    use eframe::egui::Key;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
        VK_F1, VK_HOME, VK_INSERT, VK_LEFT, VK_MENU, VK_NEXT, VK_NUMPAD0, VK_OEM_1, VK_OEM_2,
        VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA, VK_OEM_MINUS,
        VK_OEM_PERIOD, VK_OEM_PLUS, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
        VK_UP,
    };

    use crate::hotkey::tracker::Keyboard;

    /// The whole machine's keyboard, as Windows reports it to any process.
    pub(super) struct SystemKeyboard;

    impl Keyboard for SystemKeyboard {
        fn is_down(&self, key: Key) -> bool {
            virtual_keys(key).iter().any(|&code| down(code))
        }

        fn modifiers(&self) -> (bool, bool, bool) {
            (down(VK_CONTROL), down(VK_SHIFT), down(VK_MENU))
        }
    }

    fn down(code: VIRTUAL_KEY) -> bool {
        // The high bit is "down now"; the low one, "pressed since the last
        // call", belongs to whoever called last and is no use here.
        // SAFETY: a plain query taking a key code by value.
        (unsafe { GetAsyncKeyState(i32::from(code.0)) } as u16) & 0x8000 != 0
    }

    /// The keys that stand for `key` — two for a digit, which is on the row
    /// above the letters and on the number pad, and egui calls both the same.
    /// Empty for a key with no fixed place on a keyboard, which is then never
    /// down: a character like `:` is Shift and another key, and which one
    /// depends on the layout.
    fn virtual_keys(key: Key) -> Vec<VIRTUAL_KEY> {
        let name = key.name();
        let mut letters = name.chars();
        if let (Some(only), None) = (letters.next(), letters.next()) {
            if only.is_ascii_uppercase() {
                return vec![VIRTUAL_KEY(only as u16)];
            }
            if let Some(digit) = only.to_digit(10) {
                return vec![
                    VIRTUAL_KEY(u16::from(b'0') + digit as u16),
                    VIRTUAL_KEY(VK_NUMPAD0.0 + digit as u16),
                ];
            }
        }
        if let Some(number) = name.strip_prefix('F').and_then(|n| n.parse::<u16>().ok())
            && (1..=24).contains(&number)
        {
            return vec![VIRTUAL_KEY(VK_F1.0 + number - 1)];
        }
        let code = match key {
            Key::ArrowDown => VK_DOWN,
            Key::ArrowLeft => VK_LEFT,
            Key::ArrowRight => VK_RIGHT,
            Key::ArrowUp => VK_UP,
            Key::Escape => VK_ESCAPE,
            Key::Tab => VK_TAB,
            Key::Backspace => VK_BACK,
            Key::Enter => VK_RETURN,
            Key::Space => VK_SPACE,
            Key::Insert => VK_INSERT,
            Key::Delete => VK_DELETE,
            Key::Home => VK_HOME,
            Key::End => VK_END,
            Key::PageUp => VK_PRIOR,
            Key::PageDown => VK_NEXT,
            Key::Comma => VK_OEM_COMMA,
            Key::Period => VK_OEM_PERIOD,
            Key::Minus => VK_OEM_MINUS,
            // One key, `=` unshifted and `+` shifted, on the layouts this
            // is written for.
            Key::Plus | Key::Equals => VK_OEM_PLUS,
            Key::Semicolon => VK_OEM_1,
            Key::Slash => VK_OEM_2,
            Key::Backtick => VK_OEM_3,
            Key::OpenBracket => VK_OEM_4,
            Key::Backslash => VK_OEM_5,
            Key::CloseBracket => VK_OEM_6,
            Key::Quote => VK_OEM_7,
            _ => return Vec::new(),
        };
        vec![code]
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The keys a binding is most likely to use, where Windows keeps
        /// them — a letter or digit off by one would be a hotkey on the key
        /// beside the one chosen.
        #[test]
        fn a_key_is_asked_for_by_its_own_code() {
            assert_eq!(virtual_keys(Key::R), [VIRTUAL_KEY(0x52)]);
            assert_eq!(virtual_keys(Key::V), [VIRTUAL_KEY(0x56)]);
            assert_eq!(
                virtual_keys(Key::Num1),
                [VIRTUAL_KEY(0x31), VIRTUAL_KEY(0x61)]
            );
            assert_eq!(virtual_keys(Key::F11), [VIRTUAL_KEY(0x7A)]);
            assert_eq!(virtual_keys(Key::F24), [VIRTUAL_KEY(0x87)]);
            assert_eq!(virtual_keys(Key::Comma), [VK_OEM_COMMA]);
            assert!(virtual_keys(Key::Colon).is_empty(), "layout-dependent");
        }
    }
}

#[cfg(target_os = "linux")]
mod portal {
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Sender;
    use std::time::{Duration, Instant};

    use ashpd::WindowIdentifier;
    use ashpd::desktop::Session;
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use eframe::egui::Key;
    use futures_lite::{FutureExt, StreamExt};
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

    use super::{Bound, Shared, lock};
    use crate::hotkey::tracker::Edge;
    use crate::hotkey::{Chord, Hotkey, HotkeyAction};

    /// How long a change to the bindings has to stand before it is bound.
    ///
    /// Binding is not free — a new set can bring the desktop's dialog up —
    /// and the first passes after startup name every channel `?` until the
    /// project's names arrive. Waiting this long binds what the names settle
    /// on rather than asking somebody to allow a list of question marks.
    const SETTLE: Duration = Duration::from_millis(500);

    /// How often the listener stops waiting on the portal to look at what
    /// the application asked of it — a change of bindings, or to stop.
    const TICK: Duration = Duration::from_millis(50);

    /// A connection to the portal that answered, ready to be run.
    pub(super) struct Listener {
        portal: GlobalShortcuts,
        /// This window, exported for the desktop to show its dialog over.
        /// Held for as long as the listener runs: on Wayland, the handle is
        /// withdrawn when this is dropped.
        parent: Option<WindowIdentifier>,
        /// How the desktop reads a key — see [`Spelling`].
        spelling: Spelling,
    }

    impl Listener {
        /// Registers with the portal and checks it will answer, or says why
        /// not and answers `None` — which leaves the hotkeys to the window.
        ///
        /// Done here, before the thread exists, so that `None` is decided
        /// while there is still a window to fall back to. Every step is a
        /// round trip on the session bus and nothing waits on a person.
        ///
        /// Registration failing is not tried past. GNOME's portal drops a
        /// request from an application it cannot name without answering it
        /// at all — measured, unanswered for as long as it was waited on —
        /// and a dialog provider left holding one was seen to hold every
        /// request after it too.
        pub(super) fn connect(window: &(impl HasWindowHandle + HasDisplayHandle)) -> Option<Self> {
            let connected = async_io::block_on(async {
                // A connection of its own rather than the one `ashpd` shares
                // across the process. Registering names the *connection*,
                // and must come before anything else is asked on it; the
                // shared one has already carried the display picker's
                // requests, and naming it now would also change whose
                // saved screen-sharing permissions those requests find.
                let connection = ashpd::zbus::Connection::session().await?;
                let app_id: ashpd::AppID = crate::APP_ID
                    .parse()
                    .map_err(|_| ashpd::Error::NoResponse)?;
                ashpd::register_host_app_with_connection(connection.clone(), app_id).await?;
                GlobalShortcuts::with_connection(connection).await
            });
            let portal = match connected {
                Ok(portal) => portal,
                Err(error) => {
                    eprintln!(
                        "global hotkeys are not available, so hotkeys work only while this \
                         window has focus: {error}. If the desktop entry is not installed, \
                         assets/linux/install-desktop-entry.sh installs it."
                    );
                    return None;
                }
            };
            let parent = match (window.window_handle(), window.display_handle()) {
                (Ok(window), Ok(display)) => async_io::block_on(WindowIdentifier::from_raw_handle(
                    &window.as_raw(),
                    Some(&display.as_raw()),
                )),
                _ => None,
            };
            Some(Self {
                portal,
                parent,
                spelling: Spelling::of_this_desktop(),
            })
        }

        /// Keeps the portal's shortcuts in step with the bindings and turns
        /// what it reports into edges, until told to stop.
        pub(super) fn run(self, shared: &Shared, sender: &Sender<Edge>, wake: &impl Fn()) {
            async_io::block_on(self.listen(shared, sender, wake));
            lock(&shared.taken).clear();
        }

        async fn listen(self, shared: &Shared, sender: &Sender<Edge>, wake: &impl Fn()) {
            let (Ok(mut activated), Ok(mut deactivated), Ok(mut reassigned)) = (
                self.portal.receive_activated().await,
                self.portal.receive_deactivated().await,
                self.portal.receive_shortcuts_changed().await,
            ) else {
                eprintln!("global hotkeys: the portal would not report its shortcuts");
                return;
            };
            let mut state = State::default();
            let mut session: Option<Session<GlobalShortcuts>> = None;
            // The generation last bound, and the one seen changing and when.
            let mut bound = None;
            let mut pending = (shared.generation.load(Ordering::Acquire), Instant::now());

            while !shared.stop.load(Ordering::Acquire) {
                let generation = shared.generation.load(Ordering::Acquire);
                if generation != pending.0 {
                    pending = (generation, Instant::now());
                }
                if bound != Some(generation)
                    && pending.1.elapsed() >= SETTLE
                    && shared.focused.load(Ordering::Acquire)
                {
                    bound = Some(generation);
                    // Whatever was held is let go with the session it was
                    // held through, as the tracker lets go of a hotkey whose
                    // binding changed under it.
                    close(&mut session).await;
                    send(sender, wake, state.unbind());
                    lock(&shared.taken).clear();

                    let wanted = lock(&shared.bound).clone();
                    if let Some((opened, shortcuts, keyed)) =
                        self.bind(&wanted, shared, generation).await
                    {
                        session = Some(opened);
                        state.bound = Some(shortcuts);
                        *lock(&shared.taken) = state.keyed(&keyed);
                    }
                    continue;
                }

                let event = async { activated.next().await.map(Event::from) }
                    .or(async { deactivated.next().await.map(Event::from) })
                    .or(async { reassigned.next().await.map(Event::from) })
                    .or(async {
                        async_io::Timer::after(TICK).await;
                        Some(Event::Tick)
                    })
                    .await;
                let Some(event) = event else {
                    eprintln!("global hotkeys: the portal went away");
                    break;
                };
                if let Event::Reassigned { session, shortcuts } = &event {
                    // Somebody changed a key in the desktop's own settings:
                    // one given a key is this listener's from now on, and
                    // one whose key was taken away goes back to the window.
                    if state.is_current(session) {
                        log(shortcuts);
                        *lock(&shared.taken) = state.keyed(shortcuts);
                    }
                    continue;
                }
                let typing = shared.typing.load(Ordering::Acquire);
                send(
                    sender,
                    wake,
                    state.take(event, typing).into_iter().collect(),
                );
            }
            close(&mut session).await;
            send(sender, wake, state.unbind());
        }

        /// Asks for `wanted` in a session of its own, and waits — for as
        /// long as somebody takes to answer the desktop's dialog, if it
        /// shows one, or until the bindings change again or the listener is
        /// told to stop, either of which withdraws the question. `None` for
        /// nothing to bind, a refusal, a failure or a withdrawal, each of
        /// which leaves the window its own hotkeys.
        ///
        /// What comes back is the session, which of its ids is which hotkey,
        /// and the shortcuts the desktop allowed with the key it gave each.
        async fn bind(
            &self,
            wanted: &[Bound],
            shared: &Shared,
            generation: u64,
        ) -> Option<(Session<GlobalShortcuts>, Shortcuts, Vec<(String, String)>)> {
            if wanted.is_empty() {
                return None;
            }
            let session = self
                .portal
                .create_session(Default::default())
                .await
                .inspect_err(|error| eprintln!("global hotkeys: no session: {error}"))
                .ok()?;
            let shortcuts: Vec<NewShortcut> = wanted
                .iter()
                .map(|bound| {
                    NewShortcut::new(id(bound.hotkey), bound.description.as_str())
                        .preferred_trigger(self.spelling.trigger(bound.chord).as_deref())
                })
                .collect();
            let asking = async {
                Some(
                    self.portal
                        .bind_shortcuts(
                            &session,
                            &shortcuts,
                            self.parent.as_ref(),
                            Default::default(),
                        )
                        .await
                        .and_then(|request| request.response()),
                )
            };
            let withdrawn = async {
                while !shared.stop.load(Ordering::Acquire)
                    && shared.generation.load(Ordering::Acquire) == generation
                {
                    async_io::Timer::after(TICK).await;
                }
                None
            };
            let answer = asking.or(withdrawn).await;
            let allowed = match answer {
                Some(Ok(allowed)) => allowed,
                Some(Err(error)) => {
                    eprintln!(
                        "global hotkeys were not allowed, so they work only while this \
                         window has focus: {error}"
                    );
                    let _ = session.close().await;
                    return None;
                }
                None => {
                    let _ = session.close().await;
                    return None;
                }
            };
            let keyed: Vec<(String, String)> = allowed
                .shortcuts()
                .iter()
                .map(|shortcut| {
                    (
                        shortcut.id().to_owned(),
                        shortcut.trigger_description().to_owned(),
                    )
                })
                .collect();
            log(&keyed);
            let Some(path) = path_of(&session) else {
                eprintln!("global hotkeys: the portal's session has no path");
                let _ = session.close().await;
                return None;
            };
            let ids = wanted
                .iter()
                .map(|bound| (id(bound.hotkey), bound.hotkey))
                .collect();
            Some((session, Shortcuts { path, ids }, keyed))
        }
    }

    /// Says which key the desktop gave each shortcut — the one thing about
    /// these that a person may need to go and look for, since the desktop
    /// can give a different key from the one asked for.
    fn log(keyed: &[(String, String)]) {
        if keyed.is_empty() {
            eprintln!("global hotkeys: the desktop gave none of them a key");
        }
        for (id, trigger) in keyed {
            match trigger.as_str() {
                "" => eprintln!("global hotkey `{id}` has no key"),
                trigger => eprintln!("global hotkey `{id}` is {trigger}"),
            }
        }
    }

    /// How a desktop reads the key a shortcut asks for.
    ///
    /// The portal's specification points at the XDG shortcuts
    /// specification — `CTRL+SHIFT+r` — and GNOME does not follow it: its
    /// provider reads the preferred trigger with GTK's own accelerator
    /// parser, which takes `<Control><Shift>r` and nothing else. Read from
    /// its source rather than guessed at, after every spelling of the other
    /// kind came back with no key.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Spelling {
        Specification,
        Gtk,
    }

    impl Spelling {
        fn of_this_desktop() -> Self {
            let gnome = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|desktops| {
                desktops
                    .split(':')
                    .any(|desktop| desktop.eq_ignore_ascii_case("gnome"))
            });
            if gnome {
                Self::Gtk
            } else {
                Self::Specification
            }
        }

        /// A chord as this desktop reads one, or `None` for a key it has no
        /// name for, which leaves the key to be chosen in the dialog.
        fn trigger(self, chord: Chord) -> Option<String> {
            let key = keysym(chord.key)?;
            let held = [
                (chord.ctrl, "CTRL", "<Control>"),
                (chord.alt, "ALT", "<Alt>"),
                (chord.shift, "SHIFT", "<Shift>"),
            ];
            let mut trigger = String::new();
            for (down, specification, gtk) in held {
                if down {
                    match self {
                        Self::Specification => {
                            trigger.push_str(specification);
                            trigger.push('+');
                        }
                        Self::Gtk => trigger.push_str(gtk),
                    }
                }
            }
            trigger.push_str(&key);
            Some(trigger)
        }
    }

    /// What is bound through the current session: where the portal's
    /// reports about it come from, and which of its shortcuts is which
    /// hotkey.
    struct Shortcuts {
        path: String,
        ids: HashMap<String, Hotkey>,
    }

    /// The object path a session lives at, which is what the portal's
    /// reports name it by. `ashpd` keeps its own accessor to itself, but a
    /// session serialises as exactly that path — it is how one is sent on
    /// the bus — so this reads it back the way the portal would.
    fn path_of(session: &Session<GlobalShortcuts>) -> Option<String> {
        use ashpd::zvariant::{LE, OwnedObjectPath, serialized::Context, to_bytes};
        let bytes = to_bytes(Context::new_dbus(LE, 0), session).ok()?;
        let (path, _): (OwnedObjectPath, _) = bytes.deserialize().ok()?;
        Some(path.to_string())
    }

    async fn close(session: &mut Option<Session<GlobalShortcuts>>) {
        if let Some(session) = session.take() {
            let _ = session.close().await;
        }
    }

    /// What the portal reported, or that it is time to look up.
    enum Event {
        Down {
            session: String,
            id: String,
        },
        Up {
            session: String,
            id: String,
        },
        /// Keys given or taken away in the desktop's own settings.
        Reassigned {
            session: String,
            shortcuts: Vec<(String, String)>,
        },
        Tick,
    }

    impl From<ashpd::desktop::global_shortcuts::ShortcutsChanged> for Event {
        fn from(changed: ashpd::desktop::global_shortcuts::ShortcutsChanged) -> Self {
            Self::Reassigned {
                session: changed.session_handle().to_string(),
                shortcuts: changed
                    .shortcuts()
                    .iter()
                    .map(|shortcut| {
                        (
                            shortcut.id().to_owned(),
                            shortcut.trigger_description().to_owned(),
                        )
                    })
                    .collect(),
            }
        }
    }

    impl From<ashpd::desktop::global_shortcuts::Activated> for Event {
        fn from(activated: ashpd::desktop::global_shortcuts::Activated) -> Self {
            Self::Down {
                session: activated.session_handle().to_string(),
                id: activated.shortcut_id().to_owned(),
            }
        }
    }

    impl From<ashpd::desktop::global_shortcuts::Deactivated> for Event {
        fn from(deactivated: ashpd::desktop::global_shortcuts::Deactivated) -> Self {
            Self::Up {
                session: deactivated.session_handle().to_string(),
                id: deactivated.shortcut_id().to_owned(),
            }
        }
    }

    /// What is bound, and what is held through it.
    #[derive(Default)]
    struct State {
        bound: Option<Shortcuts>,
        down: HashSet<Hotkey>,
    }

    impl State {
        /// What one report means — the same rule the tracker keeps: nothing
        /// goes down while the window is taking typed input, a hotkey goes
        /// down once however long it is held, and only what went down comes
        /// up.
        ///
        /// A report from any session but the current one is ignored. One
        /// closed a moment ago can still have a press in flight, and taken,
        /// it would be a push-to-talk held open with nothing left to report
        /// its release.
        fn take(&mut self, event: Event, typing: bool) -> Option<Edge> {
            let (session, id, pressed) = match event {
                Event::Down { session, id } => (session, id, true),
                Event::Up { session, id } => (session, id, false),
                Event::Reassigned { .. } | Event::Tick => return None,
            };
            let bound = self.bound.as_ref().filter(|bound| bound.path == session)?;
            let hotkey = *bound.ids.get(&id)?;
            let changed = if pressed {
                !typing && self.down.insert(hotkey)
            } else {
                self.down.remove(&hotkey)
            };
            changed.then_some(Edge { hotkey, pressed })
        }

        /// Whether `session` is the one bound now.
        fn is_current(&self, session: &str) -> bool {
            self.bound
                .as_ref()
                .is_some_and(|bound| bound.path == session)
        }

        /// The hotkeys among `shortcuts` the desktop gave a key to — the
        /// ones this listener hears, leaving the rest to the window.
        fn keyed(&self, shortcuts: &[(String, String)]) -> HashSet<Hotkey> {
            let Some(bound) = &self.bound else {
                return HashSet::new();
            };
            shortcuts
                .iter()
                .filter(|(_, trigger)| !trigger.is_empty())
                .filter_map(|(id, _)| bound.ids.get(id).copied())
                .collect()
        }

        /// Forgets what was bound, letting go of everything held through it.
        fn unbind(&mut self) -> Vec<Edge> {
            self.bound = None;
            self.down
                .drain()
                .map(|hotkey| Edge {
                    hotkey,
                    pressed: false,
                })
                .collect()
        }
    }

    fn send(sender: &Sender<Edge>, wake: &impl Fn(), edges: Vec<Edge>) {
        if edges.is_empty() {
            return;
        }
        for edge in edges {
            let _ = sender.send(edge);
        }
        wake();
    }

    /// The name a hotkey is bound under — fixed for as long as what it is
    /// about exists, since it is what the desktop remembers it by, and a
    /// shortcut allowed once should not have to be allowed again.
    fn id(hotkey: Hotkey) -> String {
        match hotkey {
            Hotkey::Action(action) => match action {
                HotkeyAction::ToggleRecording => "toggle-recording".to_owned(),
                HotkeyAction::TogglePause => "toggle-pause".to_owned(),
                HotkeyAction::ToggleStreaming => "toggle-streaming".to_owned(),
                HotkeyAction::Screenshot => "screenshot".to_owned(),
                HotkeyAction::Fullscreen => "fullscreen".to_owned(),
                HotkeyAction::OpenSettings => "open-settings".to_owned(),
            },
            Hotkey::PushToTalk(channel) => format!("push-to-talk-{}", channel.0),
            Hotkey::PushToMute(channel) => format!("push-to-mute-{}", channel.0),
            Hotkey::ToggleMute(channel) => format!("toggle-mute-{}", channel.0),
            Hotkey::Scene(scene) => format!("scene-{}", scene.0),
        }
    }

    /// The X keysym name for `key`, which is what the specification names
    /// keys by. Letters are the lower-case keysym: `R` with Shift is written
    /// `SHIFT+r`, and `SHIFT+R` would be a different key.
    fn keysym(key: Key) -> Option<String> {
        let name = key.name();
        let mut letters = name.chars();
        if let (Some(only), None) = (letters.next(), letters.next())
            && only.is_ascii_alphanumeric()
        {
            return Some(only.to_ascii_lowercase().to_string());
        }
        if let Some(number) = name.strip_prefix('F').and_then(|n| n.parse::<u8>().ok())
            && (1..=35).contains(&number)
        {
            return Some(format!("F{number}"));
        }
        let name = match key {
            Key::ArrowDown => "Down",
            Key::ArrowLeft => "Left",
            Key::ArrowRight => "Right",
            Key::ArrowUp => "Up",
            Key::Escape => "Escape",
            Key::Tab => "Tab",
            Key::Backspace => "BackSpace",
            Key::Enter => "Return",
            Key::Space => "space",
            Key::Insert => "Insert",
            Key::Delete => "Delete",
            Key::Home => "Home",
            Key::End => "End",
            Key::PageUp => "Page_Up",
            Key::PageDown => "Page_Down",
            Key::Comma => "comma",
            Key::Period => "period",
            Key::Minus => "minus",
            // One key, `=` unshifted and `+` shifted, as on Windows.
            Key::Plus | Key::Equals => "equal",
            Key::Semicolon => "semicolon",
            Key::Slash => "slash",
            Key::Backtick => "grave",
            Key::OpenBracket => "bracketleft",
            Key::Backslash => "backslash",
            Key::CloseBracket => "bracketright",
            Key::Quote => "apostrophe",
            _ => return None,
        };
        Some(name.to_owned())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::domain::{AudioSourceId, SceneId};

        /// What the desktop remembers a shortcut by must tell every hotkey
        /// apart — two sharing one would be allowed as one and do both.
        #[test]
        fn every_hotkey_is_bound_under_a_name_of_its_own() {
            let hotkeys = [
                Hotkey::Action(HotkeyAction::ToggleRecording),
                Hotkey::Action(HotkeyAction::TogglePause),
                Hotkey::Action(HotkeyAction::ToggleStreaming),
                Hotkey::Action(HotkeyAction::Screenshot),
                Hotkey::PushToTalk(AudioSourceId(1)),
                Hotkey::PushToMute(AudioSourceId(1)),
                Hotkey::ToggleMute(AudioSourceId(1)),
                Hotkey::PushToTalk(AudioSourceId(12)),
                Hotkey::Scene(SceneId(1)),
                Hotkey::Scene(SceneId(12)),
            ];
            let ids: HashSet<String> = hotkeys.iter().map(|hotkey| id(*hotkey)).collect();
            assert_eq!(ids.len(), hotkeys.len());
            assert_eq!(id(Hotkey::PushToTalk(AudioSourceId(12))), "push-to-talk-12");
        }

        /// The keys a binding is most likely to use, as the specification
        /// spells them — a letter must be the lower-case keysym, since the
        /// upper-case one is Shift and the letter.
        #[test]
        fn a_chord_is_written_as_the_shortcuts_specification_writes_it() {
            let spell = |chord| Spelling::Specification.trigger(chord);
            assert_eq!(spell(Chord::ctrl(Key::R)).as_deref(), Some("CTRL+r"));
            assert_eq!(spell(Chord::plain(Key::F9)).as_deref(), Some("F9"));
            assert_eq!(
                spell(Chord {
                    key: Key::Num1,
                    ctrl: true,
                    shift: true,
                    alt: true,
                })
                .as_deref(),
                Some("CTRL+ALT+SHIFT+1")
            );
            assert_eq!(
                spell(Chord::ctrl(Key::Comma)).as_deref(),
                Some("CTRL+comma")
            );
            assert_eq!(spell(Chord::plain(Key::PageUp)).as_deref(), Some("Page_Up"));
            assert!(
                spell(Chord::plain(Key::Colon)).is_none(),
                "layout-dependent"
            );
        }

        /// And as GNOME reads one, which is GTK's accelerator syntax: sent
        /// the specification's, every chord came back with no key at all.
        #[test]
        fn a_chord_is_written_for_gnome_as_gtk_writes_it() {
            let spell = |chord| Spelling::Gtk.trigger(chord);
            assert_eq!(spell(Chord::ctrl(Key::R)).as_deref(), Some("<Control>r"));
            assert_eq!(
                spell(Chord {
                    key: Key::F9,
                    ctrl: true,
                    shift: true,
                    alt: true,
                })
                .as_deref(),
                Some("<Control><Alt><Shift>F9")
            );
            assert_eq!(spell(Chord::plain(Key::Space)).as_deref(), Some("space"));
        }

        const SESSION: &str = "/org/freedesktop/portal/desktop/session/1_7/obsrs";
        const TALK: Hotkey = Hotkey::PushToTalk(AudioSourceId(1));

        fn bound() -> State {
            State {
                bound: Some(Shortcuts {
                    path: SESSION.to_owned(),
                    ids: HashMap::from([(id(TALK), TALK)]),
                }),
                down: HashSet::new(),
            }
        }

        fn down(session: &str) -> Event {
            Event::Down {
                session: session.to_owned(),
                id: id(TALK),
            }
        }

        fn up(session: &str) -> Event {
            Event::Up {
                session: session.to_owned(),
                id: id(TALK),
            }
        }

        #[test]
        fn a_press_and_its_release_are_one_edge_each() {
            let mut state = bound();
            assert_eq!(
                state.take(down(SESSION), false),
                Some(Edge {
                    hotkey: TALK,
                    pressed: true
                })
            );
            // The desktop repeats nothing, but a second report must not be
            // a second press either.
            assert_eq!(state.take(down(SESSION), false), None);
            assert_eq!(
                state.take(up(SESSION), false),
                Some(Edge {
                    hotkey: TALK,
                    pressed: false
                })
            );
            assert_eq!(state.take(up(SESSION), false), None);
        }

        /// The race the session check is for: a press from a session closed
        /// a moment ago, taken, would be a push-to-talk left open — its
        /// release would come from that session too, or never.
        #[test]
        fn a_report_from_a_session_already_closed_is_ignored() {
            let mut state = bound();
            let closed = "/org/freedesktop/portal/desktop/session/1_7/older";
            assert_eq!(state.take(down(closed), false), None);
            assert!(state.down.is_empty());
        }

        /// The window's first rule, kept here too: renaming a Scene must not
        /// start a recording. A release still comes through, so a key held
        /// into a text field lets go when it comes up.
        #[test]
        fn nothing_goes_down_while_the_window_is_taking_typed_input() {
            let mut state = bound();
            assert_eq!(state.take(down(SESSION), true), None);

            let mut state = bound();
            state.take(down(SESSION), false);
            assert_eq!(
                state.take(up(SESSION), true),
                Some(Edge {
                    hotkey: TALK,
                    pressed: false
                })
            );
        }

        /// Bindings changing under a held push-to-talk end it, as the
        /// tracker ends a hotkey whose binding changed.
        #[test]
        fn what_is_held_is_let_go_when_the_bindings_change() {
            let mut state = bound();
            state.take(down(SESSION), false);
            assert_eq!(
                state.unbind(),
                vec![Edge {
                    hotkey: TALK,
                    pressed: false
                }]
            );
            assert_eq!(state.take(up(SESSION), false), None, "already let go");
        }

        /// Only what the desktop gave a key is this listener's; a shortcut
        /// allowed with none stays the window's, or it would work nowhere.
        #[test]
        fn only_a_shortcut_given_a_key_is_taken_from_the_window() {
            let state = bound();
            assert_eq!(
                state.keyed(&[(id(TALK), "Ctrl+Alt+Shift+F9".to_owned())]),
                HashSet::from([TALK])
            );
            assert!(state.keyed(&[(id(TALK), String::new())]).is_empty());
            // One the desktop reports that this never asked for.
            assert!(
                state
                    .keyed(&[("scene-99".to_owned(), "F5".to_owned())])
                    .is_empty()
            );
        }

        #[test]
        fn a_report_with_nothing_bound_is_ignored() {
            let mut state = State::default();
            assert_eq!(state.take(down(SESSION), false), None);
        }
    }
}
