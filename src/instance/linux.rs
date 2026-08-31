//! The claim as a `flock`, which is all this needs here.
//!
//! `flock` belongs to the open file description rather than to the process,
//! so a second open of the same path is refused even from inside the same
//! process — and the kernel drops it when the descriptor closes, a killed
//! process included.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

use x11rb::CURRENT_TIME;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, EventMask,
    InputFocus, StackMode, Window,
};
use x11rb::rust_connection::RustConnection;

pub(super) fn hold(path: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        // Not truncated on open, and that matters: this runs before the lock
        // is known to be ours, so truncating here would wipe the running
        // instance's pid on the way to discovering it is running — costing
        // the raise the pid is written for. The holder clears it itself once
        // it has won, in `record_pid`.
        .truncate(false)
        .open(path)?;
    // SAFETY: `file` owns the descriptor for the whole call, and `flock`
    // does nothing with it beyond the lock.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if locked == 0 {
        return Ok(Some(file));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // Held by someone else, which is an answer rather than a fault.
        Some(libc::EWOULDBLOCK) => Ok(None),
        _ => Err(error),
    }
}

/// Asks the window manager to bring `pid`'s window forward.
///
/// # Asked, not taken
///
/// Windows grants the foreground to a process the user has just launched.
/// X11 has no such call: `_NET_ACTIVE_WINDOW` is a request sent to whoever
/// is managing the screen, and a window manager is free to answer it by
/// marking the window urgent instead — a flashing task bar entry rather than
/// a raised window. That is still the running instance answering for itself,
/// so a delivered request counts as raised here; the alternative is printing
/// "already running" into a terminal nobody launched this from.
///
/// The request is sent as a pager rather than as an application, which is
/// what `wmctrl -a` does and for the same reason: focus-stealing prevention
/// exists to stop a background process taking the screen, and this is a
/// launch the user just performed. A window manager advertising no
/// `_NET_ACTIVE_WINDOW` at all is asked the old way, by stacking and focus,
/// which no one arbitrates.
///
/// # Wayland answers nothing
///
/// There is no request to send. Activation there is `xdg-activation-v1`, and
/// its token can only come from the client that currently has the user's
/// attention — a process that has just started has one only if whatever
/// launched it passed one down, and passing it on to the running instance
/// needs a channel between the two that this application does not have. So a
/// Wayland session finds no window (the running instance has an X11 window
/// only under XWayland) and this reports that honestly rather than pretending.
pub(super) fn raise(pid: u32) -> bool {
    let Ok((connection, screen)) = x11rb::connect(None) else {
        return false;
    };
    let Some(root) = connection
        .setup()
        .roots
        .get(screen)
        .map(|screen| screen.root)
    else {
        return false;
    };
    let Some(atoms) = Atoms::intern(&connection) else {
        return false;
    };
    let Some(window) = window_of(&connection, root, atoms, pid) else {
        return false;
    };
    activate(&connection, root, window, atoms).is_ok()
}

#[derive(Clone, Copy)]
struct Atoms {
    active_window: Atom,
    client_list: Atom,
    supported: Atom,
    wm_pid: Atom,
}

impl Atoms {
    fn intern(connection: &RustConnection) -> Option<Self> {
        Some(Self {
            active_window: intern(connection, b"_NET_ACTIVE_WINDOW")?,
            client_list: intern(connection, b"_NET_CLIENT_LIST")?,
            supported: intern(connection, b"_NET_SUPPORTED")?,
            wm_pid: intern(connection, b"_NET_WM_PID")?,
        })
    }
}

fn intern(connection: &RustConnection, name: &[u8]) -> Option<Atom> {
    connection
        .intern_atom(false, name)
        .ok()?
        .reply()
        .ok()
        .map(|reply| reply.atom)
}

/// The window `pid` put on the screen, if it has one.
///
/// `_NET_CLIENT_LIST` is asked first because it is already the list this
/// wants: the windows a window manager is managing, which is what the user
/// would call an application's window. The tree under the root is everything
/// else as well — menus, tooltips, the input method's helpers — so it is only
/// the fallback, for a screen with no window manager on it, and there the
/// override-redirect windows have to be dropped by hand.
///
/// Nothing is filtered on being mapped. An iconified window is *unmapped* on
/// X11, where Windows keeps a minimised one visible, and a minimised window
/// is precisely the case a second launch exists to fix.
fn window_of(connection: &RustConnection, root: Window, atoms: Atoms, pid: u32) -> Option<Window> {
    let managed = property_u32(connection, root, atoms.client_list);
    if !managed.is_empty() {
        return managed
            .into_iter()
            .find(|window| window_pid(connection, *window, atoms) == Some(pid));
    }
    let children = connection.query_tree(root).ok()?.reply().ok()?.children;
    children.into_iter().find(|window| {
        window_pid(connection, *window, atoms) == Some(pid)
            && connection
                .get_window_attributes(*window)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .is_some_and(|attributes| !attributes.override_redirect)
    })
}

fn window_pid(connection: &RustConnection, window: Window, atoms: Atoms) -> Option<u32> {
    property_u32(connection, window, atoms.wm_pid)
        .into_iter()
        .next()
}

fn property_u32(connection: &RustConnection, window: Window, property: Atom) -> Vec<u32> {
    connection
        .get_property(false, window, property, AtomEnum::ANY, 0, u32::MAX)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .and_then(|reply| reply.value32().map(Iterator::collect))
        .unwrap_or_default()
}

fn activate(
    connection: &RustConnection,
    root: Window,
    window: Window,
    atoms: Atoms,
) -> Result<(), Box<dyn std::error::Error>> {
    if property_u32(connection, root, atoms.supported).contains(&atoms.active_window) {
        // Source indication 2, "pager": see this function's own docs for why
        // an application asking on its own behalf is the wrong claim to make
        // here. The rest is the timestamp, the window losing the focus, and
        // two unused fields.
        let request =
            ClientMessageEvent::new(32, window, atoms.active_window, [2, CURRENT_TIME, 0, 0, 0]);
        connection.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            request,
        )?;
    } else {
        // No window manager, or one from before EWMH. Both halves are needed:
        // stacking a window above the rest does not give it the keyboard, and
        // focusing one that is behind another leaves the user typing into
        // something they cannot see.
        connection.map_window(window)?;
        connection.configure_window(
            window,
            &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )?;
        connection.set_input_focus(InputFocus::PARENT, window, CURRENT_TIME)?;
    }
    // The requests are queued until something flushes them, and this process
    // is about to exit — an unflushed connection would drop the whole point
    // of the launch on the floor.
    connection.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The honest answer matters more than the raise: `main` prints "already
    /// running" when this says no, so a `true` from a search that found
    /// nothing would leave a second launch looking like it did nothing at
    /// all. Process 0 owns no window on any system, and neither does the
    /// process running this test — and on a machine with no X server at all
    /// the connection fails, which has to be the same no rather than a panic.
    #[test]
    fn a_process_with_no_window_is_not_raised() {
        assert!(!raise(0), "no process owns a window under pid 0");
        assert!(
            !raise(std::process::id()),
            "the test harness has no window to raise"
        );
    }
}
