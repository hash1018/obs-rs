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
//! # Elsewhere
//!
//! Linux has no one answer — the `GlobalShortcuts` portal on Wayland, a key
//! grab on X11 — and [`GlobalHotkeys::spawn`] answers `None` there, which
//! leaves every hotkey to the window as before.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use super::tracker::Edge;
use super::{Chord, Hotkey, HotkeySettings};

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
    bound: Mutex<Vec<(Hotkey, Chord)>>,
    /// Whether this application's own window has the keyboard for typing,
    /// in which case nothing may go down — see `tracker::Tracker::update`.
    typing: std::sync::atomic::AtomicBool,
    stop: std::sync::atomic::AtomicBool,
}

impl GlobalHotkeys {
    /// Starts listening, or answers `None` where there is no way to.
    ///
    /// `wake` is called whenever something went down or came up, from the
    /// listener's thread; the edges themselves wait in [`Self::edges`].
    #[cfg(target_os = "windows")]
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Option<Self> {
        use std::sync::atomic::Ordering;

        let shared = Arc::new(Shared::default());
        let (sender, edges) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("global-hotkeys".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || {
                    let mut tracker = super::tracker::Tracker::default();
                    while !shared.stop.load(Ordering::Acquire) {
                        // Cloned out rather than held: a settings change
                        // waiting on this lock for a whole look would be a
                        // dialog that stalls.
                        let bound = shared
                            .bound
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .clone();
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

    #[cfg(not(target_os = "windows"))]
    pub fn spawn(_wake: impl Fn() + Send + 'static) -> Option<Self> {
        None
    }

    /// Listens for what `settings` binds from now on — the global hotkeys
    /// among them, which is all of them but the window's own.
    pub fn set_bindings(&self, settings: &HotkeySettings) {
        let bound: Vec<(Hotkey, Chord)> = settings
            .bound()
            .into_iter()
            .filter(|(hotkey, _)| hotkey.is_global())
            .collect();
        *self
            .shared
            .bound
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = bound;
    }

    /// Whether this application's window is taking typed input right now.
    pub fn set_typing(&self, typing: bool) {
        self.shared
            .typing
            .store(typing, std::sync::atomic::Ordering::Release);
    }

    /// Everything that went down or came up since the last call.
    pub fn edges(&self) -> Vec<Edge> {
        self.edges.try_iter().collect()
    }
}

impl Drop for GlobalHotkeys {
    fn drop(&mut self) {
        self.shared
            .stop
            .store(true, std::sync::atomic::Ordering::Release);
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
