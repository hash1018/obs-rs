//! A Window Capture on Linux: the portal's screen cast, filtered to windows.
//!
//! The same element a Display Capture uses, told to list windows rather than
//! monitors. Nothing is resolved here — the portal names no window, so there
//! is no "not found" state and this never answers `Ok(None)`.

use media_pp::elements::{
    CaptureSourceKind, CudaConverter, CudaDevice, CudaVideoCompositorHandle,
    CudaVideoCompositorInput, FrameRateHandle, PipeWireScreenCaptureOptions,
    PipeWireScreenCaptureSource, VideoLayer,
};
use media_pp::pipeline::Pipeline;

use crate::domain::{SourceSettings, WindowCaptureTarget};
use crate::engine::backend::{BackendError, RunningSource};
use crate::engine::source::{OpenSource, input_name};
use crate::snapshots::SceneItemSnapshot;

pub(in crate::engine) fn open(
    device: &CudaDevice,
    handle: &CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<Option<(OpenSource, FrameRateHandle)>, BackendError> {
    let SourceSettings::WindowCapture(settings) = &item.settings else {
        return Err("scene item is not a window capture".into());
    };
    let WindowCaptureTarget::Portal { restore_token } = &settings.target else {
        return Err("a named window cannot be resolved through the portal".into());
    };

    let name = input_name(item);
    let opened = PipeWireScreenCaptureSource::open(
        name.clone(),
        PipeWireScreenCaptureOptions {
            fps,
            restore_token: restore_token.clone(),
            source_kind: CaptureSourceKind::Window,
            ..Default::default()
        },
    )?;
    let rate = opened.source.frame_rate();
    let refreshed_token = opened.restore_token.clone();
    let converter = CudaConverter::new(format!("{name}-convert"), device, 1, 1)?;

    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;
    let pipeline = Pipeline::new(name.clone(), opened.source, move |source, context| {
        let branch = context.branch().pipe(converter).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok(Some((
        OpenSource {
            source: RunningSource(pipeline),
            layer,
            name,
            refreshed_token: Some(refreshed_token),
            showing: true,
            pushed: None,
        },
        rate,
    )))
}
