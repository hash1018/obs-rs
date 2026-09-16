//! A camera on Linux: V4L2, by device node.
//!
//! # One camera, however many items show it
//!
//! Opened once and shared, as on Windows — see the Windows half and
//! [`crate::engine::source::shared`]. The failure it prevents is not the
//! Windows one: V4L2 refuses the second reader outright, as busy, so the
//! second item of one camera never showed at all and was tried again every
//! second for as long as the first held it. Measured with one camera twice in
//! one Scene. The upload sits before the `Tee`, so N items cost one camera,
//! one upload, and a rack each, and the camera runs at the mode the first
//! item to open it asked for.

use media_pp::elements::{
    CudaDevice, CudaUpload, CudaVideoCompositorHandle, CudaVideoCompositorInput, TeeBuilder,
    V4l2CaptureFormat, V4l2CaptureOptions, V4l2CaptureSource, V4l2Device, VideoLayer,
};
use std::sync::Arc;

use media_pp::ffmpeg;
use media_pp::graph::BranchId;
use media_pp::pipeline::Pipeline;

use crate::domain::{SourceSettings, VideoCaptureSettings};
use crate::engine::backend::{BackendError, RunningSource, pipeline_ended};
use crate::engine::source::shared::{Registry, Shared, SharedCapture};
use crate::engine::source::{
    FilledRack, OpenOutcome, OpenSource, filled_rack, filters, input_name,
};
use crate::snapshots::SceneItemSnapshot;

/// Frames held between the camera and the upload — see the Windows half,
/// which keeps the same two for the same reason: one being uploaded and one
/// waiting, and anything deeper is only latency on a source that has no
/// timeline to replay.
const QUEUE_DEPTH: usize = 2;

/// Every camera this backend has open, by the device node each one is of —
/// see the Windows twin.
#[derive(Default)]
pub(in crate::engine) struct CameraRegistry {
    open: Registry<()>,
}

impl SharedCapture for CameraRegistry {
    fn detach(&self, device: &str, branch: BranchId) {
        self.open.detach(device, branch);
    }

    fn set_showing(&self, device: &str, branch: BranchId, showing: bool) {
        self.open.set_showing(device, branch, showing);
    }

    fn stats(&self, device: &str, branch: BranchId) -> Option<media_pp::stats::PipelineStats> {
        self.open.stats(device, branch)
    }

    /// A camera is unplugged, or taken by something else. Its pipeline ends,
    /// and every item drawing from it is put back to be opened again — the
    /// first of them reopens the camera and the rest join it.
    fn ended(&self, device: &str) -> bool {
        self.open
            .with(device, |camera| pipeline_ended(camera.pipeline()))
            .unwrap_or(false)
    }
}

/// `Absent` when the camera is not there to open — see this module's
/// parent.
pub(in crate::engine) fn open(
    device: &Arc<CudaDevice>,
    handle: &CudaVideoCompositorHandle,
    cameras: &Arc<CameraRegistry>,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
) -> Result<OpenOutcome, BackendError> {
    let SourceSettings::VideoCapture(settings) = &item.settings else {
        return Err("scene item is not a video capture".into());
    };

    let name = input_name(item);
    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

    // As on Windows: a camera that is not there is a state rather than a
    // failure, and the attempt is the only way to find out — so what it said
    // is kept apart from a failure to build the branch around it.
    let mut unavailable = None;
    let mut kept = None;
    let attached = cameras.open.attach(
        &settings.device,
        || {
            open_camera(settings, &item.name, device).map_err(|absent| {
                unavailable = Some(absent.clone());
                BackendError::from(absent)
            })
        },
        |builder, size| {
            // NV12 on the GPU, uploaded once for everything drawing this
            // camera. Filters work in BGRA, so a rack with any in it puts one
            // conversion at its head; an empty one leaves the picture NV12
            // all the way to the compositor.
            let FilledRack { rack, filters } =
                filled_rack(&name, device, filters::ChainFormat::Nv12, size, item)?;
            kept = Some(filters);
            Ok(builder.pipe(rack).to(sink)?)
        },
    );
    let (branch, size) = match attached {
        Ok(attached) => attached,
        Err(error) => {
            // The input was added before the camera was asked for; one that
            // will not be drawn into is taken back out rather than left.
            handle.remove_source(&name);
            return match unavailable {
                Some(absent) => Ok(OpenOutcome::Absent(absent)),
                None => Err(error),
            };
        }
    };
    let filters = kept.ok_or("the camera answered without finishing the branch")?;

    Ok(OpenOutcome::Open(OpenSource {
        media_file: None,
        page: None,
        // What the camera negotiated, which is not always the mode that was
        // asked for — and on a camera another item opened first, it is that
        // item's mode.
        negotiated_size: Some(size),
        source: RunningSource::Shared {
            capture: Arc::clone(cameras) as Arc<dyn SharedCapture>,
            key: settings.device.clone(),
            branch,
        },
        layer,
        name,
        refreshed_token: None,
        filters: filters.open,
        filter_rack: filters.filter_rack,
        showing: true,
        running: true,
        pushed: None,
    }))
}

/// Starts one camera into a `Tee` nothing is attached to yet, with the upload
/// between the two: every item drawing this camera draws the same surface,
/// so it is carried to the GPU once.
fn open_camera(
    settings: &VideoCaptureSettings,
    item_name: &str,
    device: &Arc<CudaDevice>,
) -> Result<Shared<()>, String> {
    // The camera's own name rather than any item's: the capture outlives each
    // of them, and this is what the log and the Stats dock show it as.
    let name = format!("camera-{}", settings.device_name);
    let (source, format) = start(&name, settings, item_name)?;
    // NV12 in system memory from the camera, straight into a CUDA surface —
    // the reason the element converts rather than handing on whatever the
    // device speaks.
    let upload = CudaUpload::new(
        format!("{name}-upload"),
        device,
        media_pp::elements::CudaFrameFormat::Nv12,
        format.width,
        format.height,
    )
    .map_err(|error| format!("the camera's upload could not be made: {error}"))?;

    let mut handle = None;
    let pipeline = Pipeline::new(name.clone(), source, |source, context| {
        let (tee, tee_handle) =
            TeeBuilder::new(format!("{name}-tee"), context.clone()).build_dynamic()?;
        let branch = context
            .branch()
            .queue("camera", QUEUE_DEPTH)
            .pipe(upload)
            .to_branch(tee)?;
        context.attach(source, 0, branch)?;
        handle = Some(tee_handle);
        Ok(())
    })
    .map_err(|error| format!("the camera could not be wired up: {error}"))?;
    let tee = handle.expect("the wire closure always produces the TeeHandle");
    pipeline
        .run()
        .map_err(|error| format!("the camera could not be started: {error}"))?;

    Ok(Shared::new(
        pipeline,
        tee,
        [format.width, format.height],
        (),
    ))
}

/// Opens the camera, or answers why it is not available.
///
/// Every failure to open is read as "not there", which is what makes an
/// unplugged camera a state rather than an error: a device that was removed,
/// one a video call is already holding, and a stored node that no longer
/// names anything are indistinguishable from here, and treating any of them
/// as fatal would leave a Source that never comes back on its own. What the
/// device said is the difference, and it is what the Sources list shows.
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
) -> Result<(V4l2CaptureSource, media_pp::elements::VideoFormat), String> {
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
        Ok(opened) => return Ok(opened),
        Err(error) => error,
    };
    if requested.is_none() {
        tracing::warn!("\"{item_name}\": the camera is not available: {error}");
        return Err(format!("the camera is not available: {error}"));
    }

    tracing::warn!(
        "\"{item_name}\": the stored mode is not on offer ({error}); taking the camera's own"
    );
    V4l2CaptureSource::open(
        name,
        V4l2CaptureOptions {
            device,
            format: None,
        },
    )
    .map_err(|error| {
        tracing::warn!("\"{item_name}\": the camera is not available: {error}");
        format!("the camera is not available: {error}")
    })
}
