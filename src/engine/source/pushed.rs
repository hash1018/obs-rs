//! What a Source that is *one picture* is made of: a Color, a Drawing, an
//! Image and a Text Source.
//!
//! Four kinds, and between them one arrangement. Each works out a picture on
//! the CPU — a flat colour, some strokes, a decoded file, a line of text —
//! pushes it once, and leaves it on the compositor until something changes
//! it. So each is an [`AppSource`] with an upload behind it, its own filter
//! rack, and a handle kept for pushing the next version; what they differ in
//! is where the pixels come from and what has to be remembered to draw them
//! again — see [`super::PushedContent`].
//!
//! That difference is all a kind's own module should hold, so the rest is
//! here: [`wire`] builds the pipeline and [`opened`] pushes the first
//! picture and says what was opened. Both had been written out four times
//! over, and every change to the wiring — the rack losing its size, a `Tee`
//! starting from the context, the library taking sizes off the frames — had
//! to be made in all four, twice, once per backend.
//!
//! [`AppSource`]: media_pp::elements::AppSource

#[cfg(target_os = "linux")]
use std::sync::Arc;

use media_pp::elements::AppSourceHandle;

use crate::engine::backend::{BackendError, Layer, RunningSource};
use crate::engine::source::{
    FilledRack, OpenSource, PushedContent, PushedSurface, SourceFilters, filled_rack, filters,
};
use crate::snapshots::SceneItemSnapshot;

/// A pushed Source's pipeline, running and waiting for its first picture.
pub(in crate::engine) struct Wired {
    pub(in crate::engine) source: RunningSource,
    pub(in crate::engine) layer: Layer,
    pub(in crate::engine) pusher: AppSourceHandle,
    pub(in crate::engine) filters: SourceFilters,
}

/// The picture a kind worked out, and what it takes to work it out again.
pub(in crate::engine) struct Picture {
    pub(in crate::engine) size: [u32; 2],
    pub(in crate::engine) content: PushedContent,
    pub(in crate::engine) frame: media_pp::buffer::MediaBuffer,
}

/// Starts the pipeline one pushed picture travels: an `AppSource`, the
/// upload onto the device, this item's filters, and the compositor input.
///
/// BGRA the whole way, on both backends. The compositor takes a BGRA layer
/// and blends it itself, and a conversion to NV12 anywhere in here is where
/// an alpha would be lost — which for three of the four kinds *is* the
/// picture: the marks of a Drawing, the letters of a Text Source, whatever a
/// PNG left transparent.
#[cfg(target_os = "windows")]
pub(in crate::engine) fn wire(
    name: &str,
    gpu: &media_pp::elements::D3d11Gpu,
    handle: &media_pp::elements::D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<Wired, BackendError> {
    use media_pp::elements::{AppSource, D3d11Upload, D3d11VideoCompositorInput};
    use media_pp::pipeline::Pipeline;

    // One frame of capacity: only the newest picture matters, and a deeper
    // queue would put what is on the compositor behind the field being typed
    // into.
    let (source, pusher) = AppSource::new(name.to_owned(), 1);
    let upload = D3d11Upload::new(format!("{name}-upload"), gpu);
    let FilledRack { rack, filters } = filled_rack(name, gpu, filters::ChainFormat::Bgra, item)?;

    let D3d11VideoCompositorInput { sink, layer } = handle.add_source(name.to_owned(), layer)?;
    let (pipeline, ()) = Pipeline::new(name.to_owned(), source, move |source, context| {
        let branch = context.branch().pipe(upload).pipe(rack).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok(Wired {
        source: RunningSource::Owned(pipeline),
        layer,
        pusher,
        filters,
    })
}

/// The same on the CUDA backend — see the Direct3D half for what it is and
/// why it is BGRA throughout.
#[cfg(target_os = "linux")]
pub(in crate::engine) fn wire(
    name: &str,
    device: &Arc<media_pp::elements::CudaDevice>,
    handle: &media_pp::elements::CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<Wired, BackendError> {
    use media_pp::elements::{AppSource, CudaFrameFormat, CudaUpload, CudaVideoCompositorInput};
    use media_pp::pipeline::Pipeline;

    let (source, pusher) = AppSource::new(name.to_owned(), 1);
    let upload = CudaUpload::new(format!("{name}-upload"), device, CudaFrameFormat::Bgra);
    let FilledRack { rack, filters } = filled_rack(name, device, filters::ChainFormat::Bgra, item)?;

    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.to_owned(), layer)?;
    let (pipeline, ()) = Pipeline::new(name.to_owned(), source, move |source, context| {
        let branch = context.branch().pipe(upload).pipe(rack).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    Ok(Wired {
        source: RunningSource::Owned(pipeline),
        layer,
        pusher,
        filters,
    })
}

/// Pushes the first picture and says what was opened.
///
/// The handle that pushed it is kept rather than dropped here: an
/// `AppSource` runs only while a handle to it exists, and one let go in the
/// same breath as its only frame would end the layer it had just drawn.
pub(in crate::engine) fn opened(
    name: String,
    wired: Wired,
    picture: Picture,
) -> Result<OpenSource, BackendError> {
    let Wired {
        source,
        layer,
        pusher,
        filters,
    } = wired;
    pusher.push(picture.frame.clone())?;
    Ok(OpenSource {
        media_file: None,
        page: None,
        // Its size is its own rather than something a device answered with,
        // so there is nothing to correct.
        negotiated_size: None,
        source,
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
        pushed: Some(PushedSurface {
            pusher,
            size: picture.size,
            content: picture.content,
            // Kept for pushing again when this item's filters change — see
            // [`super::repush`], which pushes this same buffer rather than a
            // redraw of it.
            frame: picture.frame,
        }),
    })
}
