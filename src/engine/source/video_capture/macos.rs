//! A camera on macOS: AVFoundation, by the device's unique id.
//!
//! # One camera, however many items show it
//!
//! Opened once and shared, as on the other platforms — see the Linux half
//! and [`crate::engine::source::shared`]: N items cost one camera and a rack
//! each, and the camera runs at the mode the first item to open it asked
//! for. Unlike them there is no upload: the camera's own pixel buffers are
//! handed on as VideoToolbox frames, NV12, which is what the compositor
//! draws from.
//!
//! Opening asks the user for the camera where nobody has yet, and waits for
//! the answer. A camera refused is one that is not available, which the
//! Source shows as it shows an unplugged one.

use media_pp::elements::{
    AvFoundationCaptureFormat, AvFoundationCaptureOptions, AvFoundationCaptureSource,
    CompositorInput, VideoLayer,
};
use std::sync::Arc;

use media_pp::ffmpeg;
use media_pp::pipeline::Pipeline;

use crate::domain::{SourceSettings, VideoCaptureSettings};
use crate::engine::backend::{BackendError, Compositor, Gpu, RunningSource, pipeline_ended};
use crate::engine::source::shared::{CaptureEnded, Registry, Share, Shared, SharedCapture};
use crate::engine::source::{
    FilledRack, OpenOutcome, OpenSource, filled_rack, filters, input_name,
};
use crate::snapshots::SceneItemSnapshot;

/// Frames held between the camera and the `Tee` — the two the other
/// platforms keep, and for their reason: anything deeper is only latency on
/// a source that has no timeline to replay.
const QUEUE_DEPTH: usize = 2;

/// Every camera this backend has open, by the device id each one is of —
/// see the Linux twin.
#[derive(Default)]
pub(in crate::engine) struct CameraRegistry {
    open: Registry<()>,
}

impl SharedCapture for CameraRegistry {
    fn detach(&self, device: &str, share: Share) {
        self.open.detach(device, share);
    }

    fn set_showing(&self, device: &str, share: Share, showing: bool) {
        self.open.set_showing(device, share, showing);
    }

    fn stats(&self, device: &str, share: Share) -> Option<media_pp::stats::PipelineStats> {
        self.open.stats(device, share)
    }

    /// A camera is unplugged, or taken by something else. Its pipeline ends,
    /// and every item drawing from it is put back to be opened again — the
    /// first of them opens the camera anew and the rest join it.
    fn ended(&self, device: &str, share: Share) -> bool {
        self.open
            .with_share(device, share, |camera| pipeline_ended(camera.pipeline()))
            // Gone from the registry is gone.
            .unwrap_or(true)
    }
}

/// `Absent` when the camera is not there to open — see this module's
/// parent.
pub(in crate::engine) fn open(
    gpu: &Gpu,
    handle: &Compositor,
    cameras: &Arc<CameraRegistry>,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
) -> Result<OpenOutcome, BackendError> {
    let SourceSettings::VideoCapture(settings) = &item.settings else {
        return Err("scene item is not a video capture".into());
    };

    let name = input_name(item);
    let CompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

    // As on Windows: a camera that is not there is a state rather than a
    // failure, and the attempt is the only way to find out — so what it said
    // is kept apart from a failure to build the branch around it.
    let mut unavailable = None;
    let mut kept = None;
    let attached = cameras.open.attach(
        &settings.device,
        || {
            open_camera(settings, &item.name, gpu).map_err(|absent| {
                unavailable = Some(absent.clone());
                BackendError::from(absent)
            })
        },
        |builder, _size| {
            // NV12 pixel buffers, the camera's own. Filters work in BGRA, so
            // a rack with any in it puts one conversion at its head; an empty
            // one leaves the picture NV12 all the way to the compositor.
            let FilledRack { rack, filters } =
                filled_rack(&name, gpu, filters::ChainFormat::Nv12, item)?;
            kept = Some(filters);
            Ok(builder.pipe(rack).to(sink)?)
        },
    );
    let (share, size) = match attached {
        Ok(attached) => attached,
        Err(error) => {
            // The input was added before the camera was asked for; one that
            // will not be drawn into is taken back out rather than left.
            handle.remove_source(&name);
            return match unavailable {
                Some(absent) => Ok(OpenOutcome::Absent(absent)),
                // Opened, and gone again before this item could join: the
                // same state as a camera unplugged while shown, looked for
                // again the same way.
                None if error.is::<CaptureEnded>() => Ok(OpenOutcome::Absent(
                    "the camera stopped sending pictures".to_owned(),
                )),
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
            share,
        },
        layer,
        name,
        refreshed_token: None,
        filters: filters.open,
        filter_rack: filters.filter_rack,
        // Set by the engine where it is opened into a Scene's own
        // composition — see `Target`.
        nested_in: None,
        showing: true,
        running: true,
        pushed: None,
    }))
}

/// Starts one camera into a `Tee` nothing is attached to yet: every item
/// drawing this camera draws the same pixel buffers.
fn open_camera(
    settings: &VideoCaptureSettings,
    item_name: &str,
    gpu: &Gpu,
) -> Result<Shared<()>, String> {
    // The camera's own name rather than any item's: the capture outlives each
    // of them, and this is what the log and the Stats dock show it as.
    let name = format!("camera-{}", settings.device_name);
    let (source, format) = start(&name, settings, item_name, gpu)?;

    let mut handle = None;
    let (pipeline, ()) = Pipeline::new(name.clone(), source, |source, context| {
        let (tee, tee_handle) = context.tee(format!("{name}-tee")).build_dynamic()?;
        let branch = context
            .branch()
            .queue("camera", QUEUE_DEPTH)
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

/// Opens the camera, or answers why it is not available — every failure
/// read as "not there", and a mode the camera no longer offers given a
/// second try at the camera's own, for the reasons the Linux twin gives.
fn start(
    name: &str,
    settings: &VideoCaptureSettings,
    item_name: &str,
    gpu: &Gpu,
) -> Result<(AvFoundationCaptureSource, media_pp::elements::VideoFormat), String> {
    let Some(device) = AvFoundationCaptureSource::list_devices()
        .into_iter()
        .find(|device| device.id == settings.device)
    else {
        tracing::warn!("\"{item_name}\": the camera is not attached");
        return Err("the camera is not attached".to_owned());
    };
    let requested = settings.mode.map(|mode| AvFoundationCaptureFormat {
        width: mode.width,
        height: mode.height,
        frame_rate: ffmpeg::Rational::new(
            mode.framerate_numerator as i32,
            mode.framerate_denominator as i32,
        ),
    });
    let open = |format| {
        AvFoundationCaptureSource::open_videotoolbox(
            name,
            AvFoundationCaptureOptions {
                device: device.clone(),
                format,
            },
            gpu.device(),
        )
    };

    let error = match open(requested) {
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
    open(None).map_err(|error| {
        tracing::warn!("\"{item_name}\": the camera is not available: {error}");
        format!("the camera is not available: {error}")
    })
}
