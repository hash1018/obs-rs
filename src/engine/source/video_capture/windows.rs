//! A camera on Windows: Media Foundation, by symbolic link.

use media_pp::elements::{
    D3d11Upload, D3d11VideoCompositorHandle, D3d11VideoCompositorInput, MfCaptureFormat,
    MfCaptureOptions, MfCaptureSource, MfDevice, VideoLayer,
};
use std::sync::{Arc, Mutex};

use media_pp::ffmpeg;
use media_pp::pipeline::Pipeline;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};

use crate::domain::{SourceSettings, VideoCaptureSettings};
use crate::engine::backend::{BackendError, RunningSource};
use crate::engine::source::{OpenOutcome, OpenSource, filters, input_name};
use crate::snapshots::SceneItemSnapshot;

/// Frames held between the camera and the upload.
///
/// Two, and deliberately: one being uploaded and one waiting. A camera has no
/// timeline to replay, so a deeper queue would only be latency — the
/// compositor draws the newest frame either way, and older ones are work
/// nobody sees.
const QUEUE_DEPTH: usize = 2;

/// `Absent` when the camera is not there to open — see this module's
/// parent.
pub(in crate::engine) fn open(
    device: &ID3D11Device,
    d3d_context: Arc<Mutex<ID3D11DeviceContext>>,
    handle: &D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
) -> Result<OpenOutcome, BackendError> {
    let SourceSettings::VideoCapture(settings) = &item.settings else {
        return Err("scene item is not a video capture".into());
    };

    let name = input_name(item);
    let (source, format) = match start(&name, settings, &item.name) {
        Ok(opened) => opened,
        Err(absent) => return Ok(OpenOutcome::Absent(absent)),
    };

    // NV12 in, and the compositor converts it on the GPU exactly as it does
    // for a hardware-decoded video file, so an unfiltered camera converts
    // nothing on the way. One with filters does: they work in BGRA, and the
    // rack puts the conversion at the head of what it holds.
    let upload = D3d11Upload::new(
        format!("{name}-upload"),
        device,
        format.width,
        format.height,
    );
    let (rack, filter_rack) = filters::rack(
        &name,
        device,
        d3d_context,
        filters::ChainFormat::Nv12,
        format.width,
        format.height,
    );
    // Filled before the pipeline runs, so the first frame is already keyed:
    // a rack picks its contents up on the next buffer, and there is not one
    // yet.
    let filters = filter_rack.refill(&item.filters)?;

    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context
            .branch()
            .queue("camera", QUEUE_DEPTH)
            .pipe(upload)
            .pipe(rack)
            .to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok(OpenOutcome::Open(OpenSource {
        media_file: None,
        source: RunningSource::Owned(pipeline),
        layer,
        name,
        refreshed_token: None,
        filters,
        filter_rack,
        // What the camera negotiated, which is not always the mode that was
        // asked for — see `start`, where a stored mode the device no longer
        // offers falls back to its own.
        negotiated_size: Some([format.width, format.height]),
        showing: true,
        running: true,
        pushed: None,
    }))
}

/// Opens the camera, or answers why it is not available.
///
/// Every failure to open is read as "not there", which is what makes an
/// unplugged camera a state rather than an error: a device that was removed,
/// one a video call is already holding, and a stored link that no longer
/// names anything are indistinguishable from here, and treating any of them
/// as fatal would leave a Source that never comes back on its own. What the
/// device said is the difference, and it is what the Sources list shows.
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
) -> Result<(MfCaptureSource, media_pp::elements::VideoFormat), String> {
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
        Ok(opened) => return Ok(opened),
        Err(error) => error,
    };
    if requested.is_none() {
        eprintln!("\"{item_name}\": the camera is not available: {error}");
        return Err(format!("the camera is not available: {error}"));
    }

    eprintln!(
        "\"{item_name}\": the stored mode is not on offer ({error}); taking the camera's own"
    );
    MfCaptureSource::open(
        name,
        MfCaptureOptions {
            device,
            format: None,
        },
    )
    .map_err(|error| {
        eprintln!("\"{item_name}\": the camera is not available: {error}");
        format!("the camera is not available: {error}")
    })
}
