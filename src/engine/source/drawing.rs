//! A Drawing: a Source that carries marks instead of a capture.
//!
//! The one Source that carries transparency. It reaches the compositor as
//! BGRA rather than through a converter, so what was never drawn on lets the
//! scene beneath it through — which is also why the two backends differ less
//! here than for a Color Source: neither converts.

use std::sync::Arc;

use media_pp::{buffer::MediaBuffer, ffmpeg, pipeline::Pipeline, pool::UnboundObjectPool};

use crate::domain::{SourceSettings, Stroke};
use crate::snapshots::SceneItemSnapshot;

use super::super::backend::{BackendError, Layer, RunningSource};
use super::{OpenSource, PushedContent, PushedSurface, input_name};

/// Draws a Drawing's strokes into a BGRA frame the compositor can take.
///
/// # Transparent where nothing was drawn
///
/// A Drawing is an overlay, so its alpha is the marks themselves: the frame
/// starts fully transparent and only the strokes write into it. That is what
/// lets one sit over a capture without a rectangle around it.
///
/// # Straight-alpha, and drawn without smoothing
///
/// Each segment is a run of stamped discs, which is the cheapest thing that
/// gives round ends and round joins for free — a stroke is a chain of them and
/// its corners look drawn rather than mitred. The discs are hard-edged: the
/// compositor scales this frame with `scale_cuda`'s bilinear filter on the way
/// to the Canvas, which softens the edges anyway, and anti-aliasing here would
/// be paid on every point of every stroke for something that is filtered again
/// downstream.
///
/// # Why a stroke is drawn in two passes
///
/// A stroke marks its own coverage plane first, and only then reaches the
/// frame — once per pixel, whatever the discs did. Stamping straight into the
/// frame would be fine while every stroke was opaque, and wrong the moment one
/// is not: the discs overlap heavily along a segment, so a translucent stroke
/// blended per disc would build up into an opaque, blotchy line and be no
/// highlighter at all.
///
/// Between strokes it is ordinary "over" compositing, so a translucent stroke
/// laid across an earlier one lets it show through instead of cutting a hole
/// in it. Straight alpha rather than premultiplied, because that is what both
/// compositors read: D3D11 blends `SRC_ALPHA`/`INV_SRC_ALPHA` and CUDA lifts
/// the alpha byte out into a plane of its own.
pub(in crate::engine) fn drawing_bgra(width: u32, height: u32, strokes: &[Stroke]) -> MediaBuffer {
    let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::BGRA, width, height);
    let stride = frame.stride(0);
    let data = frame.data_mut(0);
    data.fill(0);

    // Reused across strokes and left clear behind each one, so this is
    // allocated once for a whole drawing rather than per stroke.
    let mut coverage = vec![0u8; (width as usize) * (height as usize)];

    for stroke in strokes {
        let radius = (stroke.width / 2.0).max(0.5);
        // What the stroke touched, so compositing walks its own marks rather
        // than the whole canvas once per stroke.
        let marked = {
            let mut marked: Option<[usize; 4]> = None;
            let mut stamp = |x: f32, y: f32| {
                let left = ((x - radius).floor() as i64).max(0) as usize;
                let top = ((y - radius).floor() as i64).max(0) as usize;
                let right = ((x + radius).ceil() as i64).clamp(0, width as i64) as usize;
                let bottom = ((y + radius).ceil() as i64).clamp(0, height as i64) as usize;
                for row in top..bottom {
                    for column in left..right {
                        let dx = column as f32 + 0.5 - x;
                        let dy = row as f32 + 0.5 - y;
                        if dx * dx + dy * dy > radius * radius {
                            continue;
                        }
                        coverage[row * width as usize + column] = 1;
                        marked = Some(match marked {
                            None => [column, row, column + 1, row + 1],
                            Some([l, t, r, b]) => {
                                [l.min(column), t.min(row), r.max(column + 1), b.max(row + 1)]
                            }
                        });
                    }
                }
            };
            match stroke.points.as_slice() {
                // A press with no movement is a dot, which is what a click draws.
                [] => {}
                [[x, y]] => stamp(*x, *y),
                points => {
                    for pair in points.windows(2) {
                        let ([x0, y0], [x1, y1]) = (pair[0], pair[1]);
                        // One stamp per half-radius along the segment, which is
                        // close enough that the discs overlap into a line and far
                        // enough that a long stroke is not stamped per pixel.
                        let span = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
                        let steps = (span / radius.max(0.5) * 2.0).ceil().max(1.0);
                        for step in 0..=steps as u32 {
                            let along = step as f32 / steps;
                            stamp(x0 + (x1 - x0) * along, y0 + (y1 - y0) * along);
                        }
                    }
                }
            }
            marked
        };

        let Some([left, top, right, bottom]) = marked else {
            continue;
        };
        for row in top..bottom {
            for column in left..right {
                let mark = &mut coverage[row * width as usize + column];
                if *mark == 0 {
                    continue;
                }
                // Cleared as it is read, so the plane is blank again for the
                // next stroke without a second pass over the region.
                *mark = 0;
                let at = row * stride + column * 4;
                over(&mut data[at..at + 4], stroke.rgba);
            }
        }
    }

    // `MediaBuffer::Video` carries pooled frames; this one has no pool behind
    // it and never returns to one, which an unbound pool of zero expresses.
    let pool = UnboundObjectPool::new(0, ffmpeg::frame::Video::empty, |_| {});
    let mut slot = pool.get();
    *slot = frame;
    MediaBuffer::Video(Arc::new(slot))
}

/// Composites one RGBA colour over one BGRA pixel, both straight-alpha.
///
/// The opaque case is the common one and is a write, which is also what keeps
/// a pen stroke exact: rounding a colour through the general form and back
/// would move it by a bit or two for no reason.
fn over(dst: &mut [u8], rgba: [u8; 4]) {
    let source_alpha = rgba[3] as f32 / 255.0;
    if rgba[3] == u8::MAX {
        dst.copy_from_slice(&[rgba[2], rgba[1], rgba[0], u8::MAX]);
        return;
    }
    let dest_alpha = dst[3] as f32 / 255.0;
    let out_alpha = source_alpha + dest_alpha * (1.0 - source_alpha);
    if out_alpha <= 0.0 {
        dst.fill(0);
        return;
    }
    for (index, source) in [rgba[2], rgba[1], rgba[0]].into_iter().enumerate() {
        let blended = (source as f32 * source_alpha
            + dst[index] as f32 * dest_alpha * (1.0 - source_alpha))
            / out_alpha;
        dst[index] = blended.round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = (out_alpha * 255.0).round() as u8;
}

/// The surface size, which is the Drawing's own: strokes are recorded in it,
/// and the layer is what scales the result onto the Canvas.
fn surface(item: &SceneItemSnapshot) -> Result<([u32; 2], Vec<Stroke>), BackendError> {
    let SourceSettings::Drawing(settings) = &item.settings else {
        return Err("scene item is not a drawing source".into());
    };
    Ok((
        [
            (settings.size[0].round() as u32).max(2) & !1,
            (settings.size[1].round() as u32).max(2) & !1,
        ],
        settings.strokes.clone(),
    ))
}

/// What both implementations return, so the difference between them stays the
/// pipeline and nothing else.
fn opened(
    name: String,
    source: RunningSource,
    layer: Layer,
    pusher: media_pp::elements::AppSourceHandle,
    size: [u32; 2],
    strokes: Vec<Stroke>,
) -> OpenSource {
    OpenSource {
        media_file: None,
        source,
        layer,
        name,
        refreshed_token: None,
        showing: true,
        pushed: Some(PushedSurface {
            pusher,
            size,
            content: PushedContent::Drawing(strokes),
        }),
    }
}

#[cfg(target_os = "windows")]
pub(in crate::engine) fn open(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    handle: &media_pp::elements::D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenSource, BackendError> {
    use media_pp::elements::{AppSource, D3d11Upload, D3d11VideoCompositorInput};

    let ([width, height], strokes) = surface(item)?;
    let name = input_name(item);
    // One frame of capacity: a drawing gesture produces one per UI frame and
    // only the newest matters, so a deeper queue would only add latency
    // between the pointer and the picture.
    let (source, pusher) = AppSource::new(name.clone(), 1);
    let upload = D3d11Upload::new(format!("{name}-upload"), device, width, height);

    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(upload).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;
    pusher.push(drawing_bgra(width, height, &strokes))?;

    Ok(opened(
        name,
        RunningSource::Owned(pipeline),
        layer,
        pusher,
        [width, height],
        strokes,
    ))
}

#[cfg(target_os = "linux")]
pub(in crate::engine) fn open(
    device: &media_pp::elements::CudaDevice,
    handle: &media_pp::elements::CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenSource, BackendError> {
    use media_pp::elements::{AppSource, CudaFrameFormat, CudaUpload, CudaVideoCompositorInput};

    let ([width, height], strokes) = surface(item)?;
    let name = input_name(item);
    let (source, pusher) = AppSource::new(name.clone(), 1);
    let upload = CudaUpload::new(
        format!("{name}-upload"),
        device,
        CudaFrameFormat::Bgra,
        width,
        height,
    )?;

    // No converter, unlike a Color Source here. A Drawing is an overlay: its
    // alpha is the marks themselves, and NV12 has nowhere to keep one, so
    // converting first would put opaque black over everything nobody drew on.
    // The compositor takes BGRA for exactly this and blends per pixel.
    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(upload).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;
    pusher.push(drawing_bgra(width, height, &strokes))?;

    Ok(opened(
        name,
        RunningSource(pipeline),
        layer,
        pusher,
        [width, height],
        strokes,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Stroke;
    use media_pp::buffer::MediaBuffer;

    /// Reads one pixel back as BGRA, which is the order the frame is in.
    fn pixel_at(buffer: &MediaBuffer, x: usize, y: usize) -> [u8; 4] {
        let MediaBuffer::Video(frame) = buffer else {
            panic!("expected a video frame");
        };
        let stride = frame.stride(0);
        let data = frame.data(0);
        let at = y * stride + x * 4;
        [data[at], data[at + 1], data[at + 2], data[at + 3]]
    }

    /// A Drawing is an overlay, so what nobody drew on has to come out
    /// transparent — an opaque black frame would hide whatever it sits over.
    #[test]
    fn an_undrawn_drawing_is_fully_transparent() {
        let frame = drawing_bgra(64, 64, &[]);
        for (x, y) in [(0, 0), (31, 31), (63, 63)] {
            assert_eq!(
                pixel_at(&frame, x, y),
                [0, 0, 0, 0],
                "({x}, {y}) should be untouched"
            );
        }
    }

    /// A press without a drag is a dot, and it lands where the pointer was.
    #[test]
    fn a_single_point_draws_a_dot_in_its_own_colour() {
        let strokes = [Stroke {
            points: vec![[32.0, 32.0]],
            rgba: [200, 100, 50, 255],
            width: 8.0,
        }];
        let frame = drawing_bgra(64, 64, &strokes);
        // BGRA, so the red the caller asked for is the third byte.
        assert_eq!(pixel_at(&frame, 32, 32), [50, 100, 200, 255]);
        assert_eq!(
            pixel_at(&frame, 32, 20),
            [0, 0, 0, 0],
            "well outside the dot stays transparent"
        );
    }

    /// The gap between two points is filled rather than left as two dots —
    /// the stamps have to overlap along the segment or a fast gesture comes
    /// out dotted.
    #[test]
    fn a_segment_is_continuous_between_its_points() {
        let strokes = [Stroke {
            points: vec![[8.0, 32.0], [56.0, 32.0]],
            rgba: [255, 255, 255, 255],
            width: 4.0,
        }];
        let frame = drawing_bgra(64, 64, &strokes);
        for x in 8..=56 {
            assert_eq!(
                pixel_at(&frame, x, 32)[3],
                255,
                "the line should be unbroken at x = {x}"
            );
        }
    }

    /// A stroke that runs off the surface is clipped, not a panic: the
    /// pointer can leave the Drawing mid-gesture and often does.
    #[test]
    fn a_stroke_leaving_the_surface_is_clipped() {
        let strokes = [Stroke {
            points: vec![[-40.0, 32.0], [100.0, 32.0]],
            rgba: [255, 255, 255, 255],
            width: 6.0,
        }];
        let frame = drawing_bgra(64, 64, &strokes);
        assert_eq!(pixel_at(&frame, 0, 32)[3], 255, "it crosses the left edge");
        assert_eq!(pixel_at(&frame, 63, 32)[3], 255, "and the right one");
    }

    /// The property a highlighter exists for: it does not get darker where it
    /// crosses itself.
    ///
    /// A stroke is stamped from discs a half-radius apart, so every pixel
    /// along it is covered several times over, and a corner is covered
    /// several times more than that. Blended per disc, a translucent stroke
    /// would come out opaque along its length and blotchy at its turns — a
    /// thick faint line, not a highlighter. Marking coverage first is what
    /// makes each pixel take the colour exactly once.
    #[test]
    fn a_translucent_stroke_does_not_build_up_where_it_overlaps_itself() {
        let strokes = vec![Stroke {
            // Doubles back on itself, so the corner is drawn over twice on
            // top of the overlap every straight run already has.
            points: vec![[10.0, 10.0], [50.0, 10.0], [50.0, 50.0], [20.0, 10.0]],
            rgba: [250, 220, 60, 90],
            width: 8.0,
        }];

        let frame = drawing_bgra(64, 64, &strokes);

        let straight = pixel_at(&frame, 30, 10);
        let corner = pixel_at(&frame, 50, 10);
        assert_eq!(
            straight[3], 90,
            "a pixel under one stroke must carry that stroke's own alpha"
        );
        assert_eq!(
            corner, straight,
            "the corner is covered many times over and must come out the same \
             as a single pass, or the stroke is darker where it doubles back"
        );
    }

    /// And it lets what was drawn before it show through, rather than cutting
    /// a hole in it.
    ///
    /// Stamping straight into the frame overwrote whatever was already there,
    /// alpha and all. That was invisible while every stroke was opaque and is
    /// the difference between marking a line and erasing it once one is not.
    #[test]
    fn a_translucent_stroke_blends_with_what_is_under_it() {
        let pen = Stroke {
            points: vec![[0.0, 20.0], [64.0, 20.0]],
            rgba: [255, 0, 0, 255],
            width: 8.0,
        };
        let highlighter = Stroke {
            points: vec![[32.0, 0.0], [32.0, 64.0]],
            rgba: [0, 0, 255, 128],
            width: 8.0,
        };

        let frame = drawing_bgra(64, 64, &[pen, highlighter]);

        // Where only the pen is, it is untouched.
        assert_eq!(pixel_at(&frame, 8, 20), [0, 0, 255, 255]);
        // Where only the highlighter is, it is itself over nothing.
        assert_eq!(pixel_at(&frame, 32, 50), [255, 0, 0, 128]);
        // Where they cross, both are in it: blue over red at half, on an
        // opaque pixel, so the result is opaque and neither pure colour.
        let crossing = pixel_at(&frame, 32, 20);
        assert_eq!(crossing[3], 255, "an opaque mark stays opaque underneath");
        assert!(
            crossing[0] > 100 && crossing[2] > 100,
            "the crossing must hold both colours, got {crossing:?}"
        );
    }
}
