//! Linux capture-target discovery.
//!
//! X11 exposes windows through EWMH and displays through XRandR. Wayland
//! deliberately exposes neither list to clients, so those sessions must use
//! the desktop portal's system-owned picker.

use std::{
    collections::HashSet,
    env, fs, io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::Duration,
};

use ashpd::desktop::{
    PersistMode, ResponseError,
    screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType},
};
use x11rb::{
    connection::Connection,
    protocol::{
        randr::ConnectionExt as _,
        xproto::{Atom, AtomEnum, ConnectionExt as _, MapState, Window},
    },
    rust_connection::RustConnection,
};

use media_pp::elements::{PipeWireAudioCaptureSource, PipeWireAudioDeviceKind};

use crate::domain::AudioSourceKind;

use super::{AudioDeviceTarget, MonitorRect, MonitorTarget, SourcePicker, WindowTarget};
use crate::domain::{DisplayCaptureSettings, DisplayCaptureTarget, SceneId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickerBackend {
    X11,
    Portal,
}

/// Selects the platform mechanism and enumerates immediately on X11.
pub(super) fn source_picker() -> SourcePicker {
    if picker_backend(
        env::var("WINIT_UNIX_BACKEND").ok().as_deref(),
        env::var("WAYLAND_DISPLAY").ok().as_deref(),
        env::var("XDG_SESSION_TYPE").ok().as_deref(),
        env::var("DISPLAY").ok().as_deref(),
    ) == PickerBackend::Portal
    {
        return SourcePicker::SystemDialog;
    }

    enumerate_x11().map_or(SourcePicker::SystemDialog, |(windows, monitors)| {
        SourcePicker::Enumerated { windows, monitors }
    })
}

fn picker_backend(
    winit_backend: Option<&str>,
    wayland_display: Option<&str>,
    session_type: Option<&str>,
    x11_display: Option<&str>,
) -> PickerBackend {
    match winit_backend {
        Some(value) if value.eq_ignore_ascii_case("x11") => return PickerBackend::X11,
        Some(value) if value.eq_ignore_ascii_case("wayland") => return PickerBackend::Portal,
        _ => {}
    }
    if wayland_display.is_some_and(|value| !value.is_empty())
        || session_type.is_some_and(|value| value.eq_ignore_ascii_case("wayland"))
    {
        PickerBackend::Portal
    } else if x11_display.is_some_and(|value| !value.is_empty()) {
        PickerBackend::X11
    } else {
        PickerBackend::Portal
    }
}

fn enumerate_x11() -> Option<(Vec<WindowTarget>, Vec<MonitorTarget>)> {
    let (connection, screen_number) = x11rb::connect(None).ok()?;
    let root = connection.setup().roots.get(screen_number)?.root;
    let monitors = monitors(&connection, root)?;
    let windows = windows(&connection, root);
    Some((windows, monitors))
}

fn monitors(connection: &RustConnection, root: Window) -> Option<Vec<MonitorTarget>> {
    let reply = connection
        .randr_get_monitors(root, true)
        .ok()?
        .reply()
        .ok()?;
    let mut found = Vec::with_capacity(reply.monitors.len());
    for monitor in reply.monitors {
        if monitor.width == 0 || monitor.height == 0 {
            continue;
        }
        let name = connection
            .get_atom_name(monitor.name)
            .ok()?
            .reply()
            .ok()
            .map(|reply| String::from_utf8_lossy(&reply.name).into_owned())?;
        if name.is_empty() {
            continue;
        }
        found.push(MonitorTarget {
            name,
            rect: MonitorRect {
                x: i32::from(monitor.x),
                y: i32::from(monitor.y),
                width: u32::from(monitor.width),
                height: u32::from(monitor.height),
            },
            is_primary: monitor.primary,
        });
    }
    (!found.is_empty()).then_some(found)
}

#[derive(Clone, Copy)]
struct WindowAtoms {
    client_list_stacking: Atom,
    net_wm_name: Atom,
    utf8_string: Atom,
    net_wm_pid: Atom,
}

fn windows(connection: &RustConnection, root: Window) -> Vec<WindowTarget> {
    let Some(atoms) = window_atoms(connection) else {
        return Vec::new();
    };
    let mut ids = property_u32(connection, root, atoms.client_list_stacking);
    if ids.is_empty() {
        ids = connection
            .query_tree(root)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map_or_else(Vec::new, |reply| reply.children);
    }

    let own_pid = std::process::id();
    let mut seen = HashSet::new();
    ids.into_iter()
        .rev()
        .filter(|window| seen.insert(*window))
        .filter_map(|window| describe_window(connection, window, atoms, own_pid))
        .collect()
}

fn window_atoms(connection: &RustConnection) -> Option<WindowAtoms> {
    Some(WindowAtoms {
        client_list_stacking: intern_atom(connection, b"_NET_CLIENT_LIST_STACKING")?,
        net_wm_name: intern_atom(connection, b"_NET_WM_NAME")?,
        utf8_string: intern_atom(connection, b"UTF8_STRING")?,
        net_wm_pid: intern_atom(connection, b"_NET_WM_PID")?,
    })
}

fn intern_atom(connection: &RustConnection, name: &[u8]) -> Option<Atom> {
    connection
        .intern_atom(false, name)
        .ok()?
        .reply()
        .ok()
        .map(|reply| reply.atom)
}

fn describe_window(
    connection: &RustConnection,
    window: Window,
    atoms: WindowAtoms,
    own_pid: u32,
) -> Option<WindowTarget> {
    let attributes = connection
        .get_window_attributes(window)
        .ok()?
        .reply()
        .ok()?;
    if attributes.map_state != MapState::VIEWABLE || attributes.override_redirect {
        return None;
    }

    let geometry = connection.get_geometry(window).ok()?.reply().ok()?;
    if geometry.width == 0 || geometry.height == 0 {
        return None;
    }

    let title =
        text_property(connection, window, atoms.net_wm_name, atoms.utf8_string).or_else(|| {
            text_property(
                connection,
                window,
                AtomEnum::WM_NAME.into(),
                AtomEnum::STRING.into(),
            )
        })?;
    if title.trim().is_empty() {
        return None;
    }

    let pid = property_u32(connection, window, atoms.net_wm_pid)
        .into_iter()
        .next();
    if pid == Some(own_pid) {
        return None;
    }

    Some(WindowTarget {
        handle: window as isize,
        title,
        process: pid.and_then(process_name).unwrap_or_default(),
        size: (u32::from(geometry.width), u32::from(geometry.height)),
    })
}

fn text_property(
    connection: &RustConnection,
    window: Window,
    property: Atom,
    property_type: Atom,
) -> Option<String> {
    let reply = connection
        .get_property(false, window, property, property_type, 0, u32::MAX)
        .ok()?
        .reply()
        .ok()?;
    (!reply.value.is_empty()).then(|| String::from_utf8_lossy(&reply.value).into_owned())
}

fn property_u32(connection: &RustConnection, window: Window, property: Atom) -> Vec<u32> {
    connection
        .get_property(false, window, property, AtomEnum::ANY, 0, u32::MAX)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .and_then(|reply| reply.value32().map(Iterator::collect))
        .unwrap_or_default()
}

fn process_name(pid: u32) -> Option<String> {
    let path = fs::read_link(PathBuf::from("/proc").join(pid.to_string()).join("exe")).ok()?;
    path.file_name()?.to_str().map(ToOwned::to_owned)
}

/// Result of one system-owned Wayland display selection.
pub(crate) enum SystemDisplayPickerUpdate {
    Selected {
        scene_id: SceneId,
        settings: DisplayCaptureSettings,
    },
    Cancelled,
    Error(String),
}

/// Runs the desktop portal's dialog away from the UI thread.
pub(crate) struct SystemDisplayPicker {
    requests: Option<Sender<SceneId>>,
    updates: Receiver<SystemDisplayPickerUpdate>,
}

impl SystemDisplayPicker {
    pub(crate) fn spawn(wake_ui: impl Fn() + Send + 'static) -> io::Result<Self> {
        let (request_sender, requests) = mpsc::channel::<SceneId>();
        let (update_sender, updates) = mpsc::channel();
        // The handle is dropped rather than kept: see `Drop`.
        thread::Builder::new()
            .name("system-display-picker".to_owned())
            .spawn(move || {
                for scene_id in requests {
                    let update = pollster::block_on(pick_system_display(scene_id));
                    if update_sender.send(update).is_err() {
                        break;
                    }
                    wake_ui();
                }
            })?;

        Ok(Self {
            requests: Some(request_sender),
            updates,
        })
    }

    pub(crate) fn open(&self, scene_id: SceneId) {
        if let Some(requests) = &self.requests {
            let _ = requests.send(scene_id);
        }
    }

    pub(crate) fn latest(&self) -> Option<SystemDisplayPickerUpdate> {
        self.updates.try_iter().last()
    }
}

impl Drop for SystemDisplayPicker {
    fn drop(&mut self) {
        // Dropping the sender ends the worker's loop once it finishes whatever
        // it is doing. It is deliberately not joined: the portal call it may be
        // inside does not return until the user answers a dialog, and waiting
        // for that would hang shutdown behind a window they have already
        // walked away from.
        self.requests.take();
    }
}

async fn pick_system_display(scene_id: SceneId) -> SystemDisplayPickerUpdate {
    match try_pick_system_display().await {
        Ok(settings) => SystemDisplayPickerUpdate::Selected { scene_id, settings },
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => {
            SystemDisplayPickerUpdate::Cancelled
        }
        Err(error) => SystemDisplayPickerUpdate::Error(error.to_string()),
    }
}

/// Runs the portal's picker and returns the token that reproduces the choice.
///
/// Wayland gives a client no way to name a display, so the selection itself
/// cannot be stored: the stream and its node id belong to this session and
/// mean nothing to a later one. The restore token is the only value that
/// outlives the session, which is why persisting it is the whole point of
/// this function rather than an optimization.
///
/// The compositor may decline to issue one. That returns `Ok(None)` rather
/// than an error, because the selection is still valid for right now — it is
/// only the *next* run that has to prompt again.
async fn try_pick_system_display() -> ashpd::Result<DisplayCaptureSettings> {
    let portal = Screencast::new().await?;
    let session = portal.create_session(Default::default()).await?;
    portal
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Hidden)
                .set_sources(Some(SourceType::Monitor.into()))
                .set_multiple(false)
                .set_persist_mode(PersistMode::ExplicitlyRevoked),
        )
        .await?;
    let response = portal
        .start(&session, None, Default::default())
        .await?
        .response()?;
    let stream = response.streams().first().ok_or(ashpd::Error::NoResponse)?;
    // The portal reports a size for monitor streams. It is only a hint — the
    // compositor may scale the stream to something it never named — but it is
    // what lets the new SceneItem start at the display's own shape.
    let size_hint = stream.size().and_then(|(width, height)| {
        Some([u32::try_from(width).ok()?, u32::try_from(height).ok()?])
            .filter(|[width, height]| *width > 0 && *height > 0)
    });
    let restore_token = response.restore_token().map(ToOwned::to_owned);

    // This session existed only to run the picker; nothing reads its stream.
    // The capture layer opens its own session from `restore_token` later, so
    // holding this one open would pin a PipeWire node no one consumes.
    let _ = session.close().await;

    Ok(DisplayCaptureSettings {
        target: DisplayCaptureTarget::Portal { restore_token },
        size_hint,
    })
}

/// Watches for PipeWire nodes appearing or going, calling `on_change` each
/// time the set is not what it was.
///
/// A poll rather than a registry subscription. PipeWire does publish node
/// events, but reaching them means this process opening a second connection
/// and running a loop of its own beside the one `media-pp` already has —
/// where re-enumerating is a round trip every couple of seconds and answers
/// the only question asked of it. If the enumeration ever costs enough to
/// notice, the subscription is what replaces this.
///

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_uses_the_portal_even_when_xwayland_is_available() {
        assert_eq!(
            picker_backend(None, Some("wayland-0"), Some("wayland"), Some(":0")),
            PickerBackend::Portal
        );
    }

    #[test]
    fn x11_session_is_enumerated() {
        assert_eq!(
            picker_backend(None, None, Some("x11"), Some(":0")),
            PickerBackend::X11
        );
    }

    #[test]
    fn explicit_winit_backend_wins_for_hybrid_sessions() {
        assert_eq!(
            picker_backend(Some("x11"), Some("wayland-0"), Some("wayland"), Some(":0")),
            PickerBackend::X11
        );
    }
}

/// Every PipeWire audio node, both sinks and sources.
///
/// The node *name* is what gets stored, not the id: a PipeWire id is valid
/// only while its node is, so unplugging and reattaching a device yields a
/// new one and a saved choice would stop resolving. `media-pp`'s own
/// `PipeWireAudioDevice` says as much on the field.
///
/// An enumeration that fails is an empty list rather than an error. The
/// picker's own "default device" entry still works without one — it is the
/// absence of a choice, not one of the entries — so a caller has something to
/// show either way.
pub fn audio_devices() -> Vec<AudioDeviceTarget> {
    let devices = match PipeWireAudioCaptureSource::list_devices() {
        Ok(devices) => devices,
        Err(error) => {
            eprintln!("could not list audio devices: {error}");
            return Vec::new();
        }
    };
    devices
        .into_iter()
        .map(|device| AudioDeviceTarget {
            id: device.name,
            name: device.description,
            kind: match device.kind {
                // A sink is captured through its monitor ports, so what
                // arrives is whatever the machine is playing.
                PipeWireAudioDeviceKind::Sink => AudioSourceKind::Output,
                PipeWireAudioDeviceKind::Source => AudioSourceKind::Input,
            },
            is_default: device.is_default,
        })
        .collect()
}

pub fn watch_audio_devices(on_change: impl Fn() + Send + 'static) -> Option<AudioDeviceWatch> {
    let stop = Arc::new(AtomicBool::new(false));
    let worker = thread::Builder::new()
        .name("audio-devices".to_owned())
        .spawn({
            let stop = Arc::clone(&stop);
            move || {
                // The set this started from, so the first change is reported
                // and the state it started in is not.
                let mut known = device_identity();
                while !stop.load(Ordering::Relaxed) {
                    thread::sleep(POLL_INTERVAL);
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let current = device_identity();
                    if current != known {
                        known = current;
                        on_change();
                    }
                }
            }
        })
        .inspect_err(|error| eprintln!("could not watch audio devices: {error}"))
        .ok()?;
    Some(AudioDeviceWatch {
        stop,
        worker: Some(worker),
    })
}

/// How long a device can be plugged in before the mixer notices. Long enough
/// that the enumeration is free, short enough that plugging something in and
/// looking at the dock feels like one action.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// What the poll compares. Node names and which one is default, in
/// enumeration order — everything the mixer decides anything from, and
/// nothing else, so a description changing does not read as a device
/// arriving.
fn device_identity() -> Vec<(String, bool)> {
    audio_devices()
        .into_iter()
        .map(|device| (device.id, device.is_default))
        .collect()
}

/// Keeps the polling thread running.
pub struct AudioDeviceWatch {
    stop: Arc<AtomicBool>,
    /// `Option` only so `Drop` can take the handle to join it.
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for AudioDeviceWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            // Up to one interval, because the thread checks the flag either
            // side of its sleep rather than waiting on a condition variable.
            let _ = worker.join();
        }
    }
}
