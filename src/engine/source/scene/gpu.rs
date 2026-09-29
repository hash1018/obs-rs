//! A Scene inside a Scene, on Linux and macOS — both composite through a
//! `backend::Gpu`.
//!
//! The Windows half's own docs say what this is for and why it is one
//! picture rather than a bundle of layers; what differs here is the
//! compositor. The Canvas is composed in NV12, which has no alpha at all,
//! so a composition meant to be laid over another asks for a BGRA canvas
//! instead — which every compositor behind a `Gpu`, CUDA, Vulkan and Metal,
//! can make. That is what lets an overlay Scene leave the Scene under it
//! showing.

use std::sync::Arc;

use media_pp::color::Color;
use media_pp::elements::{CompositorInput, VideoCompositorOptions, VideoLayer};
use media_pp::ffmpeg;

use crate::domain::{SceneId, SourceSettings};
use crate::engine::backend::{BackendError, Compositor, Gpu, RunningSource};
use crate::engine::source::shared::{Registry, Share, Shared, SharedCapture};
use crate::engine::source::{
    FilledRack, OpenOutcome, OpenSource, filled_rack, filters, input_name,
};
use crate::snapshots::SceneItemSnapshot;

/// Every Scene being composited for another Scene, by the id of the Scene it
/// draws.
#[derive(Default)]
pub(in crate::engine) struct SceneRegistry {
    pub(in crate::engine) open: Registry<Compositor>,
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
    gpu: &Gpu,
    handle: &Compositor,
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
    let CompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

    let key = key(settings.scene_id);
    let mut kept = None;
    let (share, _) = scenes.open.attach(
        &key,
        || compose(&settings.scene_name, gpu, fps, canvas),
        |builder, _size| {
            // No converter in front of it, as a Text Source has none: what
            // arrives is BGRA and the alpha is the point.
            let FilledRack { rack, filters } =
                filled_rack(&name, gpu, filters::ChainFormat::Bgra, item)?;
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
    gpu: &Gpu,
    fps: u32,
    canvas: [u32; 2],
) -> Result<Shared<Compositor>, BackendError> {
    let name = format!("scene-{scene_name}");
    let (compositor, handle) = gpu.compositor(
        name.clone(),
        VideoCompositorOptions {
            mode: media_pp::elements::RenderMode::Live,
            width: canvas[0],
            height: canvas[1],
            frame_rate: ffmpeg::Rational::new(fps as i32, 1),
            // Nothing at all where the Scene drew nothing, so what reaches
            // the Scene holding this is an overlay rather than a rectangle
            // over it.
            background: Color::BLACK,
            background_alpha: 0,
        },
        filters::ChainFormat::Bgra,
    )?;

    let tee_name = format!("{name}-tee");
    let (pipeline, tee) =
        compositor.pipeline(name, |context| context.tee(tee_name).build_dynamic())?;
    pipeline.run()?;

    Ok(Shared::new(pipeline, tee, canvas, handle))
}
