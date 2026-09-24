//! A Scene inside a Scene, on Windows.
//!
//! # One picture, not a bundle of layers
//!
//! The Scene being shown is composited on its own — its own
//! `D3d11VideoCompositor`, at Canvas size and the Canvas rate — and what
//! arrives here is that one picture. So the item holding it moves, scales,
//! crops and fades as one thing, and a filter on it is a filter on the whole
//! overlay. The alternative, drawing the Scene's items into the Canvas
//! alongside everything else, could do none of those.
//!
//! # One composition, however many Scenes show it
//!
//! The composition is registered by the Scene it draws, so an overlay placed
//! in five Scenes is composited once and handed to five branches of a `Tee` —
//! the arrangement a display or a camera already has, and for the better
//! reason here: two compositions of one Scene would be two of every Source in
//! it.

use std::sync::Arc;

use media_pp::color::Color;
use media_pp::elements::{
    D3d11Gpu, D3d11VideoCompositor, D3d11VideoCompositorHandle, D3d11VideoCompositorInput,
    VideoCompositorOptions, VideoLayer,
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
    pub(in crate::engine) open: Registry<D3d11VideoCompositorHandle>,
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

pub(in crate::engine) fn open(
    gpu: &D3d11Gpu,
    handle: &D3d11VideoCompositorHandle,
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
    let D3d11VideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

    let key = key(settings.scene_id);
    let mut kept = None;
    let (share, _) = scenes.open.attach(
        &key,
        || compose(&settings.scene_name, gpu, fps, canvas),
        |builder, _size| {
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
        // Composited at Canvas size, which the item already knows: there is
        // nothing to write back.
        negotiated_size: None,
        // Set by the engine where it is opened into a Scene's own
        // composition — see `Target`.
        nested_in: None,
        showing: true,
        running: true,
        pushed: None,
    }))
}

/// Starts compositing one Scene into a `Tee` nothing is attached to yet.
///
/// Transparent where the Scene draws nothing, unlike the Canvas, which is
/// opaque black: an overlay is placed over what is under it, and a Scene that
/// blacked out whatever it was laid on would be no use as one.
fn compose(
    scene_name: &str,
    gpu: &D3d11Gpu,
    fps: u32,
    canvas: [u32; 2],
) -> Result<Shared<D3d11VideoCompositorHandle>, BackendError> {
    let name = format!("scene-{scene_name}");
    let (compositor, handle) = D3d11VideoCompositor::new(
        name.clone(),
        gpu,
        VideoCompositorOptions {
            width: canvas[0],
            height: canvas[1],
            frame_rate: ffmpeg::Rational::new(fps as i32, 1),
            // Nothing at all where the Scene drew nothing, so what reaches
            // the Scene holding this is an overlay rather than a rectangle
            // over it — see `VideoCompositorOptions::background_alpha`.
            background: Color::BLACK,
            background_alpha: 0,
        },
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
