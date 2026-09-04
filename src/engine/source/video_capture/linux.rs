//! A camera on Linux: V4L2, by device node.

use media_pp::elements::{
    CudaDevice, CudaUpload, CudaVideoCompositorHandle, CudaVideoCompositorInput, V4l2CaptureFormat,
    V4l2CaptureOptions, V4l2CaptureSource, V4l2Device, VideoLayer,
};
use media_pp::ffmpeg;
use media_pp::pipeline::Pipeline;

use crate::domain::{SourceSettings, VideoCaptureSettings};
use crate::engine::backend::{BackendError, RunningSource};
use crate::engine::source::{OpenSource, input_name};
use crate::snapshots::SceneItemSnapshot;

/// Frames held between the camera and the upload — see the Windows half,
/// which keeps the same two for the same reason: one being uploaded and one
/// waiting, and anything deeper is only latency on a source that has no
/// timeline to replay.
const QUEUE_DEPTH: usize = 2;

/// `Ok(None)` when the camera is not there to open — see this module's
/// parent.
pub(in crate::engine) fn open(
    device: &CudaDevice,
    handle: &CudaVideoCompositorHandle,
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

    // NV12 in system memory from the camera, straight into a CUDA surface —
    // the same shape the Windows half has, and the reason the element
    // converts rather than handing on whatever the device speaks.
    let upload = CudaUpload::new(
        format!("{name}-upload"),
        device,
        media_pp::elements::CudaFrameFormat::Nv12,
        format.width,
        format.height,
    )?;

    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;
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
        source: RunningSource(pipeline),
        layer,
        name,
        refreshed_token: None,
        showing: true,
        running: true,
        pushed: None,
    }))
}

/// Opens the camera, or answers `None` for one that is not available.
///
/// Every failure to open is read as "not there", which is what makes an
/// unplugged camera a state rather than an error: a device that was removed,
/// one a video call is already holding, and a stored node that no longer
/// names anything are indistinguishable from here, and treating any of them
/// as fatal would leave a Source that never comes back on its own. What the
/// log says is the difference.
///
/// A mode the camera no longer offers is the one case worth a second try —
/// see the Windows half, which makes the same allowance for the same reason:
/// a camera can be replaced by a similar one under the same node, and
/// refusing to show it because it dropped a resolution is worse than showing
/// it at whichever mode it does have.
fn start(
    name: &str,
    settings: &VideoCaptureSettings,
    item_name: &str,
) -> Option<(V4l2CaptureSource, media_pp::elements::VideoFormat)> {
    let device = V4l2Device {
        id: settings.device.clone(),
        name: settings.device_name.clone(),
    };
    let requested = settings.mode.map(|mode| V4l2CaptureFormat {
        width: mode.width,
        height: mode.height,
        framerate: ffmpeg::Rational::new(
            mode.framerate_numerator as i32,
            mode.framerate_denominator as i32,
        ),
    });

    let first = V4l2CaptureSource::open(
        name,
        V4l2CaptureOptions {
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
    match V4l2CaptureSource::open(
        name,
        V4l2CaptureOptions {
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
