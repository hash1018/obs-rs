//! A camera on Windows: Media Foundation, by symbolic link.
//!
//! # One camera, however many items show it
//!
//! A camera opened twice does not refuse the second reader — it is worse than
//! that. Both readers get a stream that keeps ending, so both Sources sit in
//! a loop of stopping and being opened again, about once every two seconds,
//! and neither says why. Measured with one camera twice in one Scene; with
//! only one Source it is quiet.
//!
//! So a camera is opened once and shared, the way a display's duplication is
//! — see [`crate::engine::source::shared`]. The upload sits before the `Tee`
//! as well, so N items cost one camera, one upload, and a rack each.
//!
//! # Which mode a shared camera runs at
//!
//! Its own: a device has one, whatever the Sources drawing from it each
//! stored. The first item to open it decides, and setting a mode writes it to
//! every Source of that camera, so each one's Properties dock says the same
//! thing — the mode belongs to the camera rather than to the Source.

use std::sync::{Arc, Mutex};

use media_pp::elements::{
    D3d11Upload, D3d11VideoCompositorHandle, D3d11VideoCompositorInput, MfCaptureFormat,
    MfCaptureOptions, MfCaptureSource, MfDevice, TeeBuilder, VideoLayer,
};
use media_pp::ffmpeg;
use media_pp::graph::BranchId;
use media_pp::pipeline::Pipeline;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};

use crate::domain::{SourceSettings, VideoCaptureSettings};
use crate::engine::backend::pipeline_ended;
use crate::engine::backend::{BackendError, RunningSource};
use crate::engine::source::shared::{Registry, Shared, SharedCapture};
use crate::engine::source::{
    FilledRack, OpenOutcome, OpenSource, filled_rack, filters, input_name,
};
use crate::snapshots::SceneItemSnapshot;

/// Frames held between the camera and the upload.
///
/// Two, and deliberately: one being uploaded and one waiting. A camera has no
/// timeline to replay, so a deeper queue would only be latency — the
/// compositor draws the newest frame either way, and older ones are work
/// nobody sees.
const QUEUE_DEPTH: usize = 2;

/// Every camera this backend has open, by the device link each one is of.
///
/// The machinery is [`Registry`]'s; what is a camera's own is how one is
/// opened, and that it can end by itself — which a display cannot.
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

    /// A camera is unplugged, or taken by a video call. Its pipeline ends,
    /// and every item drawing from it is put back to be opened again — the
    /// first of them reopens the camera and the rest join it.
    fn ended(&self, device: &str) -> bool {
        self.open
            .with(device, |camera| pipeline_ended(camera.pipeline()))
            .unwrap_or(false)
    }
}

/// `Absent` when the camera is not there to open — see this module's parent.
pub(in crate::engine) fn open(
    device: &ID3D11Device,
    d3d_context: Arc<Mutex<ID3D11DeviceContext>>,
    handle: &D3d11VideoCompositorHandle,
    cameras: &Arc<CameraRegistry>,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
) -> Result<OpenOutcome, BackendError> {
    let SourceSettings::VideoCapture(settings) = &item.settings else {
        return Err("scene item is not a video capture".into());
    };

    let name = input_name(item);
    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;

    // A camera that is not there is a state rather than a failure, and the
    // only way to find out is to open it — so what the attempt said is kept
    // here, to be told apart from a failure to build the branch around it.
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
            // NV12 in, uploaded once for everything drawing this camera, and
            // the compositor converts it on the GPU exactly as it does for a
            // hardware-decoded video file. A branch with filters converts:
            // they work in BGRA, and the rack puts that at the head of what
            // it holds.
            let FilledRack { rack, filters } = filled_rack(
                &name,
                device,
                d3d_context,
                filters::ChainFormat::Nv12,
                size,
                item,
            )?;
            kept = Some(filters);
            Ok(builder.pipe(rack).to(sink)?)
        },
    );
    let (branch, size) = match attached {
        Ok(attached) => attached,
        Err(error) => {
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
        // What the camera negotiated, which is not always the mode that was
        // asked for — see `start`, where a stored mode the device no longer
        // offers falls back to its own. On a camera another item opened
        // first, it is that item's mode.
        negotiated_size: Some(size),
        showing: true,
        running: true,
        pushed: None,
    }))
}

/// Starts one camera into a `Tee` nothing is attached to yet, with the upload
/// between the two: every item drawing this camera draws the same texture, so
/// it is carried to the GPU once.
fn open_camera(
    settings: &VideoCaptureSettings,
    item_name: &str,
    device: &ID3D11Device,
) -> Result<Shared<()>, String> {
    // The camera's own name rather than any item's: the capture outlives each
    // of them, and this is what the log and the Stats dock show it as.
    let name = format!("camera-{}", settings.device_name);
    let (source, format) = start(&name, settings, item_name)?;
    let upload = D3d11Upload::new(
        format!("{name}-upload"),
        device,
        format.width,
        format.height,
    );

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
        tracing::warn!("\"{item_name}\": the camera is not available: {error}");
        return Err(format!("the camera is not available: {error}"));
    }

    tracing::warn!(
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
        tracing::warn!("\"{item_name}\": the camera is not available: {error}");
        format!("the camera is not available: {error}")
    })
}
