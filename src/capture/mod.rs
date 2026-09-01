//! What the user can pick as a capture source, and how they pick it.
//!
//! The two halves are deliberately separate. `media-pp` captures a target the
//! application hands it — `WgcCaptureSource` takes an `HWND` and explicitly
//! does not show a picker — so choosing one is this crate's job.
//!
//! How that choice is made is not the same on every platform, and it is not a
//! detail that can be hidden behind one list-shaped API:
//!
//! - **Windows** lets a process enumerate windows and monitors, so obs-rs
//!   builds the list and draws it.
//! - **Linux/X11** permits enumeration through EWMH and XRandR. **Wayland**
//!   does not: `xdg-desktop-portal` shows *its own* picker and hands back only
//!   what the user chose. That is the security model, not a missing API.
//! - **macOS** does return a list (`SCShareableContent`), but only after the
//!   user has granted screen-recording permission, so the list can be empty
//!   for a reason that is not "nothing to capture".
//!
//! [`SourcePicker`] is that fork, named once here so the UI can branch on it
//! instead of a Windows-shaped list leaking into the rest of the app.
//!
//! Audio is the easy case and lives here too — see [`audio_devices`]. It has
//! no portal and no permission prompt: every platform answers with a list,
//! and both read it through `media-pp`, which already enumerates each backend
//! for its own capture sources.
//!
//! Being *told* the list changed does fork, though — see
//! [`watch_audio_devices`]. Windows raises endpoint notifications a process
//! can subscribe to, so it does; PipeWire's equivalent would mean a second
//! connection and loop of this crate's own, so Linux re-enumerates on a timer
//! instead. Both answer the same question, and neither is `media-pp`'s: it
//! captures the endpoint it is handed and has no opinion on which exist.

// Display-target enumeration is wired into the Sources dock. Window targets
// are retained for the upcoming Window Capture picker, so part of this shared
// platform API is still intentionally unused.
#![allow(dead_code)]

use std::path::Path;

use crate::domain::AudioSourceKind;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "windows")]
pub mod windows;

/// How this platform lets the user choose a capture source.
pub enum SourcePicker {
    /// This process may enumerate targets and present them itself.
    Enumerated {
        windows: Vec<WindowTarget>,
        monitors: Vec<MonitorTarget>,
    },
    /// The system shows its own picker and returns the selection. There is no
    /// list to draw, and asking for one is the wrong shape of request.
    SystemDialog,
}

/// One capturable top-level window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowTarget {
    /// The raw platform window identifier, as an integer so this type stays
    /// `Send`. It is an `HWND` on Windows and an X11 window ID on Linux/X11.
    pub handle: isize,
    /// The window's own title, as the user sees it in the task bar.
    pub title: String,
    /// The owning executable's file name — the only thing that tells two
    /// identically titled windows apart in a list.
    pub process: String,
    /// Current outer size in pixels. Shown to help pick between windows;
    /// it is not what the capture will be, since the window can be resized.
    pub size: (u32, u32),
}

/// One capturable display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorTarget {
    /// The stable display name, e.g. `\\.\DISPLAY1` or `DP-1`.
    pub name: String,
    /// Position and size in the virtual desktop's coordinates. On Windows it
    /// is the same space `DxgiCaptureOptions::area` uses; on X11 it is the
    /// XRandR root-window coordinate space.
    pub rect: MonitorRect,
    /// Whether this is the primary display.
    pub is_primary: bool,
}

/// One audio endpoint the user can pick for a mixer source.
///
/// Unlike a capture target, this really is a list on every platform: audio
/// needs no portal and no permission prompt, so both backends answer
/// immediately and without showing anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceTarget {
    /// What gets stored, and what is handed back to `media-pp` to open this
    /// endpoint again.
    ///
    /// Not the same field on both platforms, deliberately. Windows' endpoint
    /// id is opaque and stable, so that is what this holds. PipeWire's node
    /// id is only valid while the node is, and survives neither a replug nor
    /// a restart — so on Linux this is the node *name*, which does. Nothing
    /// above here has to know which, as long as nothing above here tries to
    /// interpret it.
    pub id: String,
    /// What the user reads in the picker.
    pub name: String,
    /// Which side of the sound card this is, matching the mixer source it can
    /// be chosen for: an [`AudioSourceKind::Output`] source lists playback
    /// endpoints, captured by listening to what they play.
    pub kind: AudioSourceKind,
    /// Whether this was the system's default for its own kind when the list
    /// was taken. Shown in the picker; it is not what "no device" means — a
    /// source with no device follows the default as it changes, rather than
    /// being pinned to whichever one this was.
    pub is_default: bool,
}

/// Every audio endpoint this platform can capture from, or an empty list on
/// one that has no backend.
///
/// Reads through `media-pp`, which already enumerates both WASAPI endpoints
/// and PipeWire nodes for its own capture sources. That is a dependency the
/// screen-capture half of this module deliberately does not have — see this
/// module's own docs — and the reason does not apply here: this is a static
/// call that shows nothing and needs no pipeline.
pub fn audio_devices() -> Vec<AudioDeviceTarget> {
    #[cfg(target_os = "windows")]
    {
        windows::audio_devices()
    }
    #[cfg(target_os = "linux")]
    {
        linux::audio_devices()
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        Vec::new()
    }
}

/// What a media file turns out to hold, as far as a new Source needs to know.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaFileStreams {
    /// The video stream's pixel size, or `None` when there is none to read.
    pub size: Option<[u32; 2]>,
    pub has_audio: bool,
    /// How long the container says it is, or `None` when it does not say.
    pub duration: Option<std::time::Duration>,
}

/// Reads what a picked file holds, so a new Source starts at its own shape
/// and its sound gets a channel.
///
/// Reads through `media-pp` for the same reason [`audio_devices`] does: a
/// static call that opens no pipeline and shows nothing. There is no picker
/// to write here either — the file dialog belongs to the system, and it hands
/// back a path.
///
/// Nothing here is a failure and nothing is reported as one. A file this
/// machine cannot demux still becomes a Source, which reports that where it
/// happens; an empty answer here only means the SceneItem starts at Canvas
/// size and gets no audio channel.
pub fn media_file_streams(path: &Path) -> MediaFileStreams {
    use media_pp::ffmpeg;

    let Ok(input) = ffmpeg::format::input(path) else {
        return MediaFileStreams::default();
    };
    let has_audio = input
        .streams()
        .any(|stream| stream.parameters().medium() == ffmpeg::media::Type::Audio);
    MediaFileStreams {
        size: video_size(&input),
        has_audio,
        // `AV_NOPTS_VALUE` and anything else non-positive is a container that
        // does not know, which is not an error — a stream saved to disk often
        // does not.
        duration: u64::try_from(input.duration())
            .ok()
            .filter(|micros| *micros > 0)
            .map(std::time::Duration::from_micros),
    }
}

/// What a live stream announced when it was asked, or why it could not be.
///
/// Asked once, when the Source is added, and not again: what this decides is
/// the shape a new SceneItem starts at and whether its sound gets a channel
/// in the mixer. A camera that is switched off *afterwards* is an ordinary
/// state the Source waits out — see `engine::source::rtsp` — but one that
/// cannot be reached when it is being added is more likely a typed address
/// that is wrong, and saying so beats a Source that sits there reconnecting
/// to nothing.
///
/// The timeout is short for the same reason: somebody is waiting for the
/// dialog to answer.
pub fn network_stream(
    url: &str,
    transport: crate::domain::RtspTransport,
) -> Result<NetworkStream, String> {
    use media_pp::ffmpeg;

    let mut options = ffmpeg::Dictionary::new();
    options.set(
        "rtsp_transport",
        match transport {
            crate::domain::RtspTransport::Tcp => "tcp",
            crate::domain::RtspTransport::Udp => "udp",
        },
    );
    options.set("timeout", &PROBE_TIMEOUT.as_micros().to_string());
    let input =
        ffmpeg::format::input_with_dictionary(url, options).map_err(|error| error.to_string())?;
    Ok(NetworkStream {
        size: video_size(&input),
        has_audio: input
            .streams()
            .any(|stream| stream.parameters().medium() == ffmpeg::media::Type::Audio),
    })
}

/// What one is, as far as adding it needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkStream {
    pub size: Option<[u32; 2]>,
    pub has_audio: bool,
}

/// How long the dialog waits for an address before saying it is not there.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The pixel size of a still picture, or `None` when it will not open.
///
/// The same reading as a media file's, and through the same library: an image
/// file is a one-frame container as far as FFmpeg is concerned, so there is no
/// second decoder here and no image crate to add.
pub fn image_size(path: &Path) -> Option<[u32; 2]> {
    video_size(&media_pp::ffmpeg::format::input(path).ok()?)
}

fn video_size(input: &media_pp::ffmpeg::format::context::Input) -> Option<[u32; 2]> {
    use media_pp::ffmpeg;

    let parameters = input
        .streams()
        .find(|stream| stream.parameters().medium() == ffmpeg::media::Type::Video)?
        .parameters();
    let video = ffmpeg::codec::context::Context::from_parameters(parameters)
        .ok()?
        .decoder()
        .video()
        .ok()?;
    match (video.width(), video.height()) {
        (0, _) | (_, 0) => None,
        size => Some([size.0, size.1]),
    }
}

/// Calls `on_change` whenever the set of audio endpoints is not what it was —
/// one plugged in, one gone, or a different one now default.
///
/// A wake-up rather than an event: it says to look again, not what changed.
/// It can arrive several times for what a person did once, and it arrives on
/// a thread this crate does not own, so the callback must be cheap and safe
/// to run twice.
///
/// `None` means this platform will not report changes and the caller sees
/// whatever was there when it last enumerated. The reason is reported where
/// it is known.
///
/// Watching stops when the returned value is dropped, and it has to be held
/// for exactly as long as the callback should keep firing.
pub fn watch_audio_devices(
    on_change: impl Fn() + Send + Sync + 'static,
) -> Option<AudioDeviceWatch> {
    #[cfg(target_os = "windows")]
    {
        windows::watch_audio_devices(on_change)
            .inspect_err(|error| eprintln!("could not watch audio devices: {error}"))
            .ok()
    }
    #[cfg(target_os = "linux")]
    {
        linux::watch_audio_devices(on_change)
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = on_change;
        None
    }
}

/// What holds an endpoint watch open. Dropping it stops the notifications.
///
/// Each platform's own type, aliased rather than wrapped: the two have
/// nothing in common but their lifetime, and there is nothing to ask them
/// once they exist.
#[cfg(target_os = "windows")]
pub type AudioDeviceWatch = windows::AudioDeviceWatch;
#[cfg(target_os = "linux")]
pub type AudioDeviceWatch = linux::AudioDeviceWatch;
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub struct AudioDeviceWatch;

/// Where this session's displays are, for deciding whether a remembered
/// window position still lands on one.
///
/// Not the same question the picker answers, and deliberately not routed
/// through it: [`source_picker`] also enumerates windows, and on Wayland it
/// puts a portal dialog on screen — neither of which belongs in a startup
/// check the user did not ask for.
///
/// An empty list means "could not say", not "no displays". Wayland does not
/// let a process enumerate them at all, so a caller must read empty as a
/// reason to trust whatever it already had rather than as a reason to
/// discard it.
pub fn displays() -> Vec<MonitorRect> {
    #[cfg(target_os = "windows")]
    {
        windows::monitors()
            .into_iter()
            .map(|monitor| monitor.rect)
            .collect()
    }
    #[cfg(target_os = "linux")]
    {
        linux::displays()
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        Vec::new()
    }
}

/// A monitor's place in the virtual desktop. Signed origin: a display left of
/// or above the primary one has negative coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Enumerates what this platform allows, or reports that it allows nothing.
///
/// Returning [`SourcePicker::SystemDialog`] is not a failure — it is the
/// answer on platforms where the system owns the picker.
pub fn source_picker() -> SourcePicker {
    #[cfg(target_os = "windows")]
    {
        SourcePicker::Enumerated {
            windows: windows::windows(),
            monitors: windows::monitors(),
        }
    }
    #[cfg(target_os = "linux")]
    {
        linux::source_picker()
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        SourcePicker::SystemDialog
    }
}
