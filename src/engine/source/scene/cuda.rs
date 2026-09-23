//! A Scene inside a Scene, on Linux.
//!
//! The Windows half's own docs say what this is for and why it is one
//! picture rather than a bundle of layers; what differs here is the
//! compositor. `CudaVideoCompositor` composes a Canvas in NV12, which has no
//! alpha at all, so a composition meant to be laid over another asks for a
//! BGRA canvas instead — see `CudaVideoCompositor::with_format`. That is
//! what lets an overlay Scene leave the Scene under it showing.

use std::sync::Arc;

use media_pp::color::Color;
use media_pp::elements::{
    CudaDevice, CudaFrameFormat, CudaVideoCompositor, CudaVideoCompositorHandle,
    CudaVideoCompositorInput, VideoCompositorOptions, VideoLayer,
};
use media_pp::ffmpeg;
use media_pp::pipeline::Pipeline;

use crate::domain::{SceneId, SourceSettings};
use crate::engine::backend::{BackendError, RunningSource};
use crate::engine::source::shared::{Registry, Share, Shared, SharedCapture};
use crate::engine::source::{
    FilledRack, OpenOutcome, OpenSource, filled_rack, filters, input_name,
};
use crate::snapshots::SceneItemSnapshot;

/// Every Scene being composited for another Scene, by the id of the Scene it
/// draws.
#[derive(Default)]
pub(in crate::engine) struct SceneRegistry {
    pub(in crate::engine) open: Registry<CudaVideoCompositorHandle>,
}

impl SharedCapture for SceneRegistry {
    fn detach(&self, scene: &str, share: Share) {
        self.open.detach(scene, share);
    }

    fn set_showing(&self, scene: &str, share: Share, showing: bool) {
        self.open.set_showing(scene, share, showing);
    }

    fn stats(&self, scene: &str, share: Share) -> Option<media_pp::stats::PipelineStats> {
        self.open.stats(scene, share)
    }

    /// A composition does not end by itself: it is this application's own
    /// compositor, running until the last item showing that Scene lets go.
    fn ended(&self, _scene: &str, _share: Share) -> bool {
        false
    }
}

/// The key a Scene's composition is registered under.
pub(in crate::engine) fn key(scene: SceneId) -> String {
    scene.0.to_string()
}

#[allow(clippy::too_many_arguments)]
pub(in crate::engine) fn open(
    device: &Arc<CudaDevice>,
    handle: &CudaVideoCompositorHandle,
    scenes: &Arc<SceneRegistry>,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
    canvas: [u32; 2],
) -> Result<OpenOutcome, BackendError> {
    let SourceSettings::Scene(settings) = &item.settings else {
        return Err("scene item is not a scene source".into());
    };

    let name = input_name(item);
    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

    let key = key(settings.scene_id);
    let mut kept = None;
    let (share, _) = scenes.open.attach(
        &key,
        || compose(&settings.scene_name, device, fps, canvas),
        |builder, size| {
            // No converter in front of it, as a Text Source has none: what
            // arrives is BGRA and the alpha is the point.
            let FilledRack { rack, filters } =
                filled_rack(&name, device, filters::ChainFormat::Bgra, item)?;
            kept = Some(filters);
            Ok(builder.pipe(rack).to(sink)?)
        },
    )?;
    let filters = kept.ok_or("the composition answered without finishing the branch")?;

    Ok(OpenOutcome::Open(OpenSource {
        page: None,
        media_file: None,
        source: RunningSource::Shared {
            capture: Arc::clone(scenes) as Arc<dyn SharedCapture>,
            key,
            share,
        },
        layer,
        name,
        refreshed_token: None,
        filters: filters.open,
        filter_rack: filters.filter_rack,
        negotiated_size: None,
        nested_in: None,
        showing: true,
        running: true,
        pushed: None,
    }))
}

/// Starts compositing one Scene into a `Tee` nothing is attached to yet.
fn compose(
    scene_name: &str,
    device: &Arc<CudaDevice>,
    fps: u32,
    canvas: [u32; 2],
) -> Result<Shared<CudaVideoCompositorHandle>, BackendError> {
    let name = format!("scene-{scene_name}");
    let (compositor, handle) = CudaVideoCompositor::with_format(
        name.clone(),
        device,
        VideoCompositorOptions {
            width: canvas[0],
            height: canvas[1],
            frame_rate: ffmpeg::Rational::new(fps as i32, 1),
            // Nothing at all where the Scene drew nothing, so what reaches
            // the Scene holding this is an overlay rather than a rectangle
            // over it.
            background: Color::BLACK,
            background_alpha: 0,
        },
        CudaFrameFormat::Bgra,
    )?;

    let mut tee = None;
    let (pipeline, ()) = Pipeline::new(name.clone(), compositor, |source, context| {
        let (branch, tee_handle) = context.tee(format!("{name}-tee")).build_dynamic()?;
        context.attach(source, 0, branch)?;
        tee = Some(tee_handle);
        Ok(())
    })?;
    let tee = tee.expect("the wire closure always produces the TeeHandle");
    pipeline.run()?;

    Ok(Shared::new(pipeline, tee, canvas, handle))
}
