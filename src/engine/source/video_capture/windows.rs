//! A camera on Windows: Media Foundation, by symbolic link.

use media_pp::elements::{
    D3d11Upload, D3d11VideoCompositorHandle, D3d11VideoCompositorInput, MfCaptureFormat,
    MfCaptureOptions, MfCaptureSource, MfDevice, VideoLayer,
};
use media_pp::ffmpeg;
use media_pp::pipeline::Pipeline;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

use crate::domain::{SourceSettings, VideoCaptureSettings};
use crate::engine::backend::{BackendError, RunningSource};
use crate::engine::source::{OpenSource, input_name};
use crate::snapshots::SceneItemSnapshot;

/// Frames held between the camera and the upload.
///
/// Two, and deliberately: one being uploaded and one waiting. A camera has no
/// timeline to replay, so a deeper queue would only be latency — the
/// compositor draws the newest frame either way, and older ones are work
/// nobody sees.
const QUEUE_DEPTH: usize = 2;

/// `Ok(None)` when the camera is not there to open — see this module's
/// parent.
pub(in crate::engine) fn open(
    device: &ID3D11Device,
    handle: &D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
) -> Result<Option<OpenSource>, BackendError> {
    let SourceSettings::VideoCapture(settings) = &item.settings else {
        return Err("scene item is not a video capture".into());
    };

    let name = input_name(item);
    let Some((source, format)) = start(&name, settings, &item.name) else {
        return Ok(None);
    };

    // NV12 in, and the compositor converts it on the GPU exactly as it does
    // for a hardware-decoded video file, so nothing converts on the way.
    let upload = D3d11Upload::new(
        format!("{name}-upload"),
        device,
        format.width,
        format.height,
    );

    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context
            .branch()
            .queue("camera", QUEUE_DEPTH)
            .pipe(upload)
            .to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok(Some(OpenSource {
        media_file: None,
        source: RunningSource::Owned(pipeline),
        layer,
        name,
        refreshed_token: None,
        // What the camera negotiated, which is not always the mode that was
        // asked for — see `start`, where a stored mode the device no longer
        // offers falls back to its own.
        negotiated_size: Some([format.width, format.height]),
        showing: true,
        running: true,
        pushed: None,
    }))
}

/// Opens the camera, or answers `None` for one that is not available.
///
/// Every failure to open is read as "not there", which is what makes an
/// unplugged camera a state rather than an error: a device that was removed,
/// one a video call is already holding, and a stored link that no longer
/// names anything are indistinguishable from here, and treating any of them
/// as fatal would leave a Source that never comes back on its own. What the
/// log says is the difference.
///
/// A mode that the camera no longer offers is the one case worth a second
/// try: a device can be replaced by a similar one under the same link, and
/// refusing to show it at all because it dropped a resolution is worse than
/// showing it at whichever mode it does have. The stored mode is left alone,
/// so plugging the original back in restores it.
fn start(
    name: &str,
    settings: &VideoCaptureSettings,
    item_name: &str,
) -> Option<(MfCaptureSource, media_pp::elements::VideoFormat)> {
    let device = MfDevice {
        id: settings.device.clone(),
        name: settings.device_name.clone(),
    };
    let requested = settings.mode.map(|mode| MfCaptureFormat {
        width: mode.width,
        height: mode.height,
        framerate: ffmpeg::Rational::new(
            mode.framerate_numerator as i32,
            mode.framerate_denominator as i32,
        ),
    });

    let first = MfCaptureSource::open(
        name,
        MfCaptureOptions {
            device: device.clone(),
            format: requested,
        },
    );
    let error = match first {
        Ok(opened) => return Some(opened),
        Err(error) => error,
    };
    if requested.is_none() {
        eprintln!("\"{item_name}\": the camera is not available: {error}");
        return None;
    }

    eprintln!(
        "\"{item_name}\": the stored mode is not on offer ({error}); taking the camera's own"
    );
    match MfCaptureSource::open(
        name,
        MfCaptureOptions {
            device,
            format: None,
        },
    ) {
        Ok(opened) => Some(opened),
        Err(error) => {
            eprintln!("\"{item_name}\": the camera is not available: {error}");
            None
        }
    }
}
