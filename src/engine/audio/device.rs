//! Which endpoint a source opens, and opening it.
//!
//! The only part of the audio graph that is different on each platform, and
//! the difference is confined to one function with three bodies: WASAPI on
//! Windows, PipeWire on Linux, and a refusal anywhere else.
//!
//! # Falling back rather than failing
//!
//! A source stores the endpoint it was pointed at, and that endpoint can be
//! unplugged. Both [`pick`] and [`device_available`] answer the same question
//! — the stored one if it is still there, otherwise this kind's default —
//! and they have to agree: one deciding a source is unopenable that the other
//! would have opened is a channel missing from the dock for no reason.

use crate::capture::AudioDeviceTarget;
use crate::domain::AudioSourceKind;
use crate::snapshots::AudioSourceSnapshot;

use super::BackendError;

/// Whether opening this source could find an endpoint at all.
///
/// The same question [`pick`] answers, asked of a list instead of by opening
/// one: the endpoint it stored if that is still there, and otherwise the
/// default for its kind, which is what `pick` falls back to. The two have to
/// agree — this deciding a source is unopenable that `pick` would have opened
/// is a channel missing from the dock for no reason.
pub(super) fn device_available(
    devices: &[AudioDeviceTarget],
    source: &AudioSourceSnapshot,
) -> bool {
    devices.iter().any(|device| {
        device.kind == source.kind
            && (source.device.as_deref() == Some(device.id.as_str()) || device.is_default)
    })
}

/// Opens the endpoint a source names, or the system default when it names
/// none.
#[cfg(target_os = "windows")]
pub(super) fn open_capture(
    name: &str,
    kind: AudioSourceKind,
    device: Option<&str>,
) -> Result<media_pp::elements::WasapiCaptureSource, BackendError> {
    use media_pp::elements::{
        WasapiCaptureOptions, WasapiCaptureSource, WasapiDevice, WasapiDeviceKind,
    };

    let wanted = match kind {
        AudioSourceKind::Output => WasapiDeviceKind::Render,
        AudioSourceKind::Input => WasapiDeviceKind::Capture,
    };
    let devices = WasapiCaptureSource::list_devices()?;
    let device: WasapiDevice = pick(
        devices,
        device,
        |device| &device.id,
        |device| device.kind == wanted && device.is_default,
    )?;
    Ok(WasapiCaptureSource::open(name, WasapiCaptureOptions { device })?.0)
}

#[cfg(target_os = "linux")]
pub(super) fn open_capture(
    name: &str,
    kind: AudioSourceKind,
    device: Option<&str>,
) -> Result<media_pp::elements::PipeWireAudioCaptureSource, BackendError> {
    use media_pp::elements::{
        PipeWireAudioCaptureOptions, PipeWireAudioCaptureSource, PipeWireAudioDevice,
        PipeWireAudioDeviceKind,
    };

    let wanted = match kind {
        AudioSourceKind::Output => PipeWireAudioDeviceKind::Sink,
        AudioSourceKind::Input => PipeWireAudioDeviceKind::Source,
    };
    let devices = PipeWireAudioCaptureSource::list_devices()?;
    // The node *name*, not the id: an id is valid only while its node is, so
    // a stored one would stop resolving after a replug. See
    // `capture::AudioDeviceTarget::id`.
    let device: PipeWireAudioDevice = pick(
        devices,
        device,
        |device| &device.name,
        |device| device.kind == wanted && device.is_default,
    )?;
    Ok(PipeWireAudioCaptureSource::open(name, PipeWireAudioCaptureOptions { device })?.0)
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub(super) fn open_capture(
    _name: &str,
    _kind: AudioSourceKind,
    _device: Option<&str>,
) -> Result<media_pp::elements::TestAudioSource, BackendError> {
    Err("no audio capture is written for this platform yet".into())
}

/// The stored endpoint if it is still there, otherwise the system default for
/// this kind.
///
/// Falling back rather than failing: a device that was unplugged should leave
/// the source working on whatever replaced it, which is what somebody who
/// never opened the picker would already have.
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn pick<T>(
    devices: Vec<T>,
    stored: Option<&str>,
    identity: impl Fn(&T) -> &String,
    is_default: impl Fn(&T) -> bool,
) -> Result<T, BackendError> {
    if let Some(stored) = stored
        && let Some(found) = devices.iter().position(|device| identity(device) == stored)
    {
        let mut devices = devices;
        return Ok(devices.swap_remove(found));
    }
    devices
        .into_iter()
        .find(is_default)
        .ok_or_else(|| "this machine reports no default audio device of that kind".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::AudioSourceId;
    use crate::domain::AudioSourceKind;

    fn source(kind: AudioSourceKind, device: Option<&str>) -> AudioSourceSnapshot {
        AudioSourceSnapshot {
            id: AudioSourceId(1),
            name: "test".to_owned(),
            kind,
            device: device.map(str::to_owned),
            gain_db: 0.0,
            muted: false,
            peak_db: None,
            running: true,
        }
    }

    fn device(id: &str, kind: AudioSourceKind, is_default: bool) -> AudioDeviceTarget {
        AudioDeviceTarget {
            id: id.to_owned(),
            name: id.to_owned(),
            kind,
            is_default,
        }
    }

    /// A source that named nothing follows the default, so it is openable
    /// exactly while one of its kind exists.
    #[test]
    fn a_source_with_no_device_needs_a_default_of_its_kind() {
        let mic = device("mic", AudioSourceKind::Input, true);
        let speakers = device("speakers", AudioSourceKind::Output, true);

        assert!(device_available(
            &[mic.clone(), speakers.clone()],
            &source(AudioSourceKind::Input, None)
        ));
        // Only playback endpoints: an input source has nothing to open.
        assert!(!device_available(
            &[speakers],
            &source(AudioSourceKind::Input, None)
        ));
        assert!(!device_available(
            &[],
            &source(AudioSourceKind::Input, None)
        ));
    }

    /// The stored endpoint counts whether or not it is the default one —
    /// that is the whole point of having stored it.
    #[test]
    fn a_stored_device_counts_without_being_the_default() {
        let devices = [
            device("built-in", AudioSourceKind::Input, true),
            device("usb", AudioSourceKind::Input, false),
        ];
        assert!(device_available(
            &devices,
            &source(AudioSourceKind::Input, Some("usb"))
        ));
    }

    /// `pick` falls back to the default when the stored endpoint is gone, so
    /// this has to call that source openable. The two disagreeing is a
    /// channel missing from the dock that would have opened fine.
    #[test]
    fn a_stored_device_that_is_gone_falls_back_like_pick_does() {
        let devices = [device("built-in", AudioSourceKind::Input, true)];
        assert!(device_available(
            &devices,
            &source(AudioSourceKind::Input, Some("unplugged"))
        ));

        // ...and with nothing of that kind left, neither of them can.
        let devices = [device("speakers", AudioSourceKind::Output, true)];
        assert!(!device_available(
            &devices,
            &source(AudioSourceKind::Input, Some("unplugged"))
        ));
    }

    /// A machine can report endpoints while calling none of them default.
    /// `pick` fails there unless the stored one matches, and this has to say
    /// the same.
    #[test]
    fn endpoints_with_no_default_are_only_openable_by_name() {
        let devices = [device("usb", AudioSourceKind::Input, false)];
        assert!(device_available(
            &devices,
            &source(AudioSourceKind::Input, Some("usb"))
        ));
        assert!(!device_available(
            &devices,
            &source(AudioSourceKind::Input, None)
        ));
    }
}
