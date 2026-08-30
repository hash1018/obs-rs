//! The compositor, the capture Sources that feed it, and the frame it hands
//! to the Preview — one unit, chosen per platform.
//!
//! These three are not separable. What a capture element produces decides
//! which compositor can accept it, and what that compositor emits decides how
//! the frame reaches wgpu:
//!
//! ```text
//! CUDA    PipeWire open_gpu   → CudaConverter → CudaVideoCompositor  → shared buffer → NV12 resolve
//! D3D11   DxgiCaptureSource   → (no convert)  → D3d11VideoCompositor → shared texture
//! ```
//!
//! Wiring a D3D11 capture into a CUDA compositor is not merely slow, it is
//! rejected: `media-pp` compares memory domains when a branch is built, and no
//! element converts between the two. So a platform picks all three together or
//! none of them.
//!
//! Everything around this — reconciling Sources against the project snapshot,
//! layer geometry, when the Preview branch sleeps — is the same whichever
//! backend is in use, and lives in `super`.
//!
//! # Writing a backend
//!
//! Add a file here, point the `cfg_attr` below at it, and provide:
//!
//! - `Backend::start` — takes two rates and must not confuse them. `fps` is
//!   what the compositor is built for and what an output would be recorded
//!   at; `preview_fps` is only how often the frame reaching wgpu is refreshed.
//!   Build the compositor and the branch that publishes
//!   frames, and register one texture with egui. Call `on_frame` for *every*
//!   frame the compositor produced, passing the texture id only for the ones
//!   actually drawn into it: the rate of calls is the compositor's, which is
//!   what a recording would be made of, while the Preview is redrawn less
//!   often than that. The texture is registered once and overwritten;
//!   registering per frame takes the egui renderer's write lock every frame.
//! - `Backend::{pause, resume, stop}` — the Preview branch sleeps whenever no
//!   shown Source is running, so these are called often and must be cheap.
//! - `Backend::open_source` — start one SceneItem's Source and register its
//!   compositor input. Return [`OpenSource`].
//! - `Backend::remove_source` — drop a registration by name.
//! - `Layer` — runtime control for one registered input, with `set_layer` and
//!   `set_visible`. A platform whose handle already has both can alias it.
//! - `RunningSource` — `pause`, `resume`, and `stop` for one open Source.
//!   Not the pipeline itself, because one Source is not always one pipeline:
//!   desktop duplication refuses to open the same display twice on one
//!   device, so two SceneItems showing that display share one capture and
//!   this is each item's own share of it. Stopping one must leave the other
//!   running, and a shared capture may only pause once nothing shows it.
//!
//! The Preview branch must sit behind a dropping queue. A Preview that cannot
//! keep up has to drop frames rather than slow the compositor, which every
//! other branch will be built from.

use std::error::Error;
use std::sync::Arc;

use media_pp::color::Color;
use media_pp::elements::VideoCodec;

use crate::snapshots::SceneItemSnapshot;

#[cfg_attr(target_os = "linux", path = "cuda/mod.rs")]
#[cfg_attr(target_os = "windows", path = "d3d11/mod.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows")),
    path = "unsupported.rs"
)]
mod platform;

pub(super) use platform::{Backend, Layer, PreparedRecording, RunningSource};

pub(super) type BackendError = Box<dyn Error + Send + Sync>;

/// What is behind every layer: the Canvas itself, where no Source covers it.
///
/// Part of what this module offers a backend rather than something every
/// backend must take, which is why it can be unused on one.
#[allow(dead_code)]
pub(super) const BACKGROUND: Color = Color::BLACK;

/// Which of `VideoCodec`'s H.264 entries a software choice maps to.
///
/// The hardware entries never reach here — neither is a software encoder and
/// neither has a `VideoCodec` at all — so they are folded into the one this
/// crate would rather have if they somehow did.
pub(super) fn software_codec(encoder: crate::settings::RecordingEncoder) -> VideoCodec {
    use crate::settings::RecordingEncoder;

    match encoder {
        RecordingEncoder::X264 => VideoCodec::H264,
        RecordingEncoder::OpenH264
        | RecordingEncoder::Nvenc
        | RecordingEncoder::MediaFoundation => VideoCodec::OpenH264,
    }
}

/// The rate the encoder probe opens at.
///
/// Only the frame-rate metadata an encoder is configured with, and no encoder
/// refuses a size because of it — so this is a plausible number rather than a
/// meaningful one, and probing at the rate a recording would really use would
/// tell us nothing extra.
pub(super) const PROBE_FPS: u32 = 60;

/// Frames the recording branch may fall behind by before the compositor is
/// made to wait — at 60 fps, about an eighth of a second of slack for an
/// encoder that hiccups.
#[allow(dead_code)]
pub(super) const RECORDING_QUEUE_DEPTH: usize = 8;

/// How long the compositor waits for room in that queue before giving up on
/// a frame.
///
/// Deliberately far longer than any real backpressure: the queue above
/// absorbs an encoder that is merely behind, so reaching this at all means
/// one is genuinely stuck. Finite rather than unbounded because an unbounded
/// wait here would wedge the compositor, and with it the Preview and every
/// other branch. A timeout arrives on the bus as an error naming this
/// branch, which is what makes an overloaded encoder visible instead of
/// silent.
#[allow(dead_code)]
pub(super) const RECORDING_SEND_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(500);

/// The recording's video branch while one is running.
///
/// Platform-independent even though what feeds it is not: both backends end
/// the same way, at a `PauseGate` and a branch on their compositor's `Tee`.
pub(super) struct VideoTrack {
    pub(super) branch: media_pp::graph::BranchId,
    pub(super) pause: media_pp::elements::PauseGateHandle,
}

/// A Drawing's way back to the compositor.
///
/// Kept because a Drawing is the one Source whose pixels this side produces:
/// everything else has a capture or a file behind it, and a Drawing has a
/// list of strokes that only changes when someone draws.
pub(super) struct DrawingSurface {
    pub(super) pusher: media_pp::elements::AppSourceHandle,
    pub(super) size: [u32; 2],
    /// What was last drawn into it. A Scene change that left the strokes
    /// alone — a move, a rename, anything else in the Scene at all — must not
    /// cost a full redraw and re-upload.
    pub(super) drawn: Vec<crate::domain::Stroke>,
}

/// A Source that is running, and the controls for its layer.
pub(super) struct OpenSource {
    pub(super) source: RunningSource,
    pub(super) layer: Layer,
    pub(super) name: String,
    /// The token the portal handed back, when it differs from the one it was
    /// given. `None` means the stored token is still current.
    pub(super) refreshed_token: Option<Option<String>>,
    /// Whether the Source is in the Scene being shown. One whose item left the
    /// Scene stays open but stops running, so coming back is a resume rather
    /// than another portal round trip.
    pub(super) showing: bool,
    /// Set only for a Drawing — see [`DrawingSurface`].
    pub(super) drawing: Option<DrawingSurface>,
}

/// The name a SceneItem's compositor input is registered under.
#[allow(dead_code)]
pub(super) fn input_name(item: &SceneItemSnapshot) -> String {
    format!("scene-item-{}", item.id.0)
}

/// Convenience for a backend that has no Source of a given kind yet.
pub(super) fn unsupported_kind(item: &SceneItemSnapshot) -> BackendError {
    format!("{:?} is not connected to the compositor yet", item.kind).into()
}

#[allow(dead_code)]
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
pub(super) fn drawing_bgra(
    width: u32,
    height: u32,
    strokes: &[crate::domain::Stroke],
) -> media_pp::buffer::MediaBuffer {
    use media_pp::{buffer::MediaBuffer, ffmpeg, pool::UnboundObjectPool};

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
    MediaBuffer::Video(std::sync::Arc::new(slot))
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

/// One BGRA frame filled with a single colour, ready for a backend's upload
/// element. Backend-independent: both compositors take their Color Source
/// this way, differing only in which upload carries it to the GPU.
#[allow(dead_code)]
pub(super) fn flat_bgra(width: u32, height: u32, rgba: [u8; 4]) -> media_pp::buffer::MediaBuffer {
    use media_pp::{buffer::MediaBuffer, ffmpeg, pool::UnboundObjectPool};

    let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::BGRA, width, height);
    let stride = frame.stride(0);
    // Opaque: the item's own alpha is the layer's opacity, and applying it
    // twice would darken the colour against the Canvas.
    let pixel = [rgba[2], rgba[1], rgba[0], 255];
    let row: Vec<u8> = pixel
        .iter()
        .copied()
        .cycle()
        .take(width as usize * 4)
        .collect();
    let data = frame.data_mut(0);
    for line in 0..height as usize {
        data[line * stride..line * stride + row.len()].copy_from_slice(&row);
    }

    // `MediaBuffer::Video` carries pooled frames; this one has no pool behind
    // it and never returns to one, which an unbound pool of zero expresses.
    let pool = UnboundObjectPool::new(0, ffmpeg::frame::Video::empty, |_| {});
    let mut slot = pool.get();
    *slot = frame;
    MediaBuffer::Video(Arc::new(slot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Stroke;

    /// Reads one pixel back as BGRA, which is the order the frame is in.
    fn pixel_at(buffer: &media_pp::buffer::MediaBuffer, x: usize, y: usize) -> [u8; 4] {
        let media_pp::buffer::MediaBuffer::Video(frame) = buffer else {
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
