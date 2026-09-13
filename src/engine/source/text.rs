//! A Text Source: a line this application draws rather than captures.
//!
//! Structurally a [`Drawing`](super::drawing) — glyph coverage instead of
//! stroke coverage, blended into a transparent BGRA frame and pushed through
//! an `AppSource`. It carries transparency for the same reason and reaches
//! both compositors the same way: BGRA, with no converter in front of it, so
//! everything the glyphs did not cover lets the Scene beneath show through.
//!
//! # Why the box is fixed
//!
//! An upload element's size is settled when the pipeline is built and it
//! refuses a frame of any other, so the surface here is a box the text is
//! drawn *into* rather than a rectangle that hugs the glyphs. That is not a
//! concession: a string's own width changes whenever the string does, and a
//! surface that followed it would rebuild the pipeline every time a clock
//! ticked from `9` to `10`. The box stays, and
//! [`TextAlignment`] decides which edge the text keeps against while its
//! width moves underneath it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use media_pp::{buffer::MediaBuffer, ffmpeg, pipeline::Pipeline, pool::UnboundObjectPool};

use crate::domain::{
    ClockFormat, SourceSettings, TextAlignment, TextMode, TextSourceSettings, TimerFormat,
};
use crate::snapshots::SceneItemSnapshot;

use super::super::backend::{BackendError, Layer, RunningSource};
use super::{
    FilledRack, OpenSource, PushedContent, PushedSurface, SourceFilters, filters, input_name,
};

/// The most pixels one rasterized string is allowed to occupy.
///
/// A guard against a font size and a string that multiply into gigabytes.
/// The box is bounded by what the user dragged, but the tight raster this is
/// measured against is not: it is as wide as the text is long.
const MAX_TEXT_PIXELS: usize = 64 * 1024 * 1024;

/// Parsed fonts, by the file they came from.
///
/// A font is parsed once per path for the life of the process rather than per
/// push. That matters because of what pushes: a clock redraws every second,
/// and the CJK font this falls back to on Linux is some twenty megabytes.
/// `FontArc` is a handle, so a cache hit is a refcount.
fn fonts() -> &'static Mutex<HashMap<PathBuf, FontArc>> {
    static FONTS: OnceLock<Mutex<HashMap<PathBuf, FontArc>>> = OnceLock::new();
    FONTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The font a Text Source draws with.
///
/// `None` means this application's own: the same file the interface found to
/// show Korean with, which is already resolved per platform — see
/// [`crate::i18n::font`]. Reusing it is what keeps a Text Source able to say
/// anything the rest of the window can say, without this having a second
/// opinion about where fonts live.
fn font(path: Option<&Path>) -> Result<FontArc, BackendError> {
    let path = match path {
        Some(path) => path.to_path_buf(),
        None => {
            crate::i18n::font::interface_font_path().ok_or("no font was found to draw text with")?
        }
    };
    let mut cache = fonts().lock().expect("font cache poisoned");
    if let Some(font) = cache.get(&path) {
        return Ok(font.clone());
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("could not read the font {}: {error}", path.display()))?;
    // `try_from_vec` indexes a font collection at zero, which is what the
    // `.ttc` files this falls back to on Linux and macOS need.
    let font = FontArc::try_from_vec(bytes)
        .map_err(|_| format!("{} is not a font this can draw with", path.display()))?;
    cache.insert(path, font.clone());
    Ok(font)
}

/// What a Text Source shows *now*, which for two of the three modes is not
/// what is stored.
///
/// Returns the settings with [`text`](TextSourceSettings::text) replaced by
/// the resolved line, rather than the line alone. That is what lets every
/// push go through one comparison: the engine keeps the last settings it
/// drew, and a second ticking over changes them exactly as typing does — so
/// a clock redraws once a second and a static caption still redraws never.
///
/// Called at every point that pushes, and cheap enough to be: it is a
/// `strftime` and a `format!` for two modes, and a clone for the third.
pub(in crate::engine) fn resolved(settings: &TextSourceSettings) -> TextSourceSettings {
    let mut resolved = settings.clone();
    resolved.text = match settings.mode {
        TextMode::Static => return resolved,
        TextMode::Clock => clock_line(settings.clock_format),
        TextMode::Timer => timer_line(
            settings.timer_format,
            settings.timer.elapsed(crate::clock::now_micros()),
        ),
    };
    resolved
}

/// The wall clock, written the way the chosen format writes it.
///
/// The formats are compile-time descriptions, so the only way this can fail
/// is a `Display` implementation refusing to write into a `String`, which
/// does not happen. An empty line is what a failure would produce, and it is
/// what a blank caption would look like — hence the fallback saying so.
fn clock_line(format: ClockFormat) -> String {
    use time::macros::format_description;

    let description = match format {
        ClockFormat::Time => format_description!("[hour]:[minute]:[second]"),
        ClockFormat::TimeToMinute => format_description!("[hour]:[minute]"),
        ClockFormat::DateAndTime => {
            format_description!("[year]-[month]-[day] [hour]:[minute]:[second]")
        }
        ClockFormat::Date => format_description!("[year]-[month]-[day]"),
    };
    crate::clock::now_local()
        .format(description)
        .unwrap_or_else(|_| "--:--:--".to_owned())
}

/// An elapsed duration, zero-padded so the line does not change width as it
/// counts — which matters more here than anywhere else, because a caption
/// that changed width would walk across the Canvas once a second.
fn timer_line(format: TimerFormat, elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs();
    match format {
        TimerFormat::HoursMinutesSeconds => format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            (seconds % 3600) / 60,
            seconds % 60
        ),
        // Minutes keep counting past sixty rather than rolling into an hours
        // field that is not being shown, which would make 1:00:00 read as
        // 00:00 — the one thing a stopwatch must never do.
        TimerFormat::MinutesSeconds => format!("{:02}:{:02}", seconds / 60, seconds % 60),
    }
}

/// A rasterized string: one coverage byte per pixel, tightly bounding the
/// glyphs.
///
/// Coverage rather than colour, as `media-pp`'s own text layer keeps it: the
/// colour is uniform over the whole string, so carrying it per pixel would be
/// three redundant bytes each.
struct Raster {
    width: u32,
    height: u32,
    /// `width * height` bytes, row-major. Zero is untouched by any glyph.
    coverage: Vec<u8>,
    /// Where the baseline sits within `height`, so a line can be placed by
    /// what it sits on rather than by its bounding box — two strings of the
    /// same size line up even when one has no descender.
    ascent: f32,
}

/// Lays one line out and draws it into its own tight coverage plane.
///
/// One line: newlines are dropped with the rest of the control characters,
/// which is what the settings promise. Kerning is applied, because a caption
/// at ninety pixels shows its absence.
fn rasterize(font: &FontArc, size_px: f32, text: &str) -> Option<Raster> {
    let scaled = font.as_scaled(PxScale::from(size_px));
    let mut caret = ab_glyph::point(0.0, scaled.ascent());
    let mut previous = None;
    let mut glyphs = Vec::new();
    for character in text.chars() {
        if character.is_control() {
            continue;
        }
        let mut glyph = scaled.scaled_glyph(character);
        if let Some(previous) = previous {
            caret.x += scaled.kern(previous, glyph.id);
        }
        glyph.position = caret;
        caret.x += scaled.h_advance(glyph.id);
        previous = Some(glyph.id);
        glyphs.push(glyph);
    }

    let outlined: Vec<_> = glyphs
        .into_iter()
        .filter_map(|glyph| font.outline_glyph(glyph))
        .collect();
    if outlined.is_empty() {
        return None;
    }

    // The advance width rather than the ink's own: a trailing space is part
    // of what was typed, and right-aligned text that ignored it would jump
    // when one is added.
    let width = caret.x.ceil().max(1.0);
    let height = scaled.height().ceil().max(1.0);
    if !width.is_finite() || !height.is_finite() {
        return None;
    }
    let (width, height) = (width as u32, height as u32);
    let pixels = (width as usize).checked_mul(height as usize)?;
    if pixels > MAX_TEXT_PIXELS {
        return None;
    }
    let mut coverage = Vec::new();
    coverage.try_reserve_exact(pixels).ok()?;
    coverage.resize(pixels, 0u8);
    for outlined in outlined {
        let bounds = outlined.px_bounds();
        outlined.draw(|x, y, value| {
            let x = bounds.min.x as i32 + x as i32;
            let y = bounds.min.y as i32 + y as i32;
            if x < 0 || y < 0 || x as u32 >= width || y as u32 >= height {
                return;
            }
            let at = y as usize * width as usize + x as usize;
            // Glyphs overlap — kerning tucks them together and accents sit
            // over their letters — so the strongest coverage wins rather
            // than the last one drawn.
            let alpha = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            coverage[at] = coverage[at].max(alpha);
        });
    }
    Some(Raster {
        width,
        height,
        coverage,
        ascent: scaled.ascent(),
    })
}

/// Draws a Text Source into a BGRA frame the compositor can take.
///
/// Transparent where no glyph reached, which is what lets a caption sit over
/// a capture without a rectangle around it. Straight alpha rather than
/// premultiplied, because that is what both compositors read — see
/// [`drawing_bgra`](super::drawing::drawing_bgra), which this follows.
///
/// A string too long for its box is clipped rather than scaled: scaling would
/// change the glyph height a word at a time, and a caption that shrank as it
/// was typed would be worse than one that runs out of room visibly.
pub(in crate::engine) fn text_bgra(
    width: u32,
    height: u32,
    settings: &TextSourceSettings,
) -> Result<MediaBuffer, BackendError> {
    let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::BGRA, width, height);
    let stride = frame.stride(0);
    // A new frame is allocated, not cleared, and everything below writes only
    // where a glyph reached — so without this a caption arrives surrounded by
    // whatever was last in that memory. `drawing_bgra` clears for the same
    // reason and it is the same mistake to make twice.
    frame.data_mut(0).fill(0);

    let font = font(settings.font.as_deref())?;
    if let Some(raster) = rasterize(&font, settings.font_size, &settings.text) {
        // Horizontally by the alignment, vertically on the baseline: the box
        // is centred on where the line sits rather than on its bounding box,
        // so text with no descender is not pushed up by the space one would
        // have taken.
        let left = match settings.alignment {
            TextAlignment::Left => 0.0,
            TextAlignment::Centre => (width as f32 - raster.width as f32) / 2.0,
            TextAlignment::Right => width as f32 - raster.width as f32,
        };
        let top = height as f32 / 2.0 - raster.ascent;
        let (left, top) = (left.round() as i32, top.round() as i32);

        let [red, green, blue, alpha] = settings.rgba;
        let data = frame.data_mut(0);
        for y in 0..raster.height {
            let row = top + y as i32;
            if row < 0 || row as u32 >= height {
                continue;
            }
            for x in 0..raster.width {
                let column = left + x as i32;
                if column < 0 || column as u32 >= width {
                    continue;
                }
                let coverage = raster.coverage[(y * raster.width + x) as usize];
                if coverage == 0 {
                    continue;
                }
                // The glyph's coverage and the chosen colour's own alpha are
                // both in this: a translucent caption is the colour picker's
                // alpha, and the anti-aliased edge is the coverage.
                let at = row as usize * stride + column as usize * 4;
                let combined = (u32::from(coverage) * u32::from(alpha) / 255) as u8;
                data[at..at + 4].copy_from_slice(&[blue, green, red, combined]);
            }
        }
    }

    // As `drawing_bgra` explains: this frame has no pool behind it and never
    // returns to one, which an unbound pool of zero expresses.
    let pool = UnboundObjectPool::new(0, ffmpeg::frame::Video::empty, |_| {});
    let mut slot = pool.get();
    *slot = frame;
    Ok(MediaBuffer::Video(Arc::new(slot)))
}

/// The box this Source draws into, and what it draws.
///
/// Even dimensions, as a Drawing's are: the compositor blends into an NV12
/// canvas whose chroma is shared between pairs of pixels, so an odd-sized
/// layer would be placed at an even position and rounded anyway.
fn surface(item: &SceneItemSnapshot) -> Result<([u32; 2], TextSourceSettings), BackendError> {
    let SourceSettings::Text(settings) = &item.settings else {
        return Err("scene item is not a text source".into());
    };
    Ok((
        [
            (settings.size[0].round() as u32).max(2) & !1,
            (settings.size[1].round() as u32).max(2) & !1,
        ],
        settings.clone(),
    ))
}

/// What both implementations return, so the difference between them stays the
/// pipeline and nothing else.
/// The picture a Text Source opened with, and what it was drawn from.
struct Drawn {
    frame: MediaBuffer,
    size: [u32; 2],
    settings: TextSourceSettings,
}

/// What both implementations return, so the difference between them stays the
/// pipeline and nothing else. The picture is pushed here, once the pipeline
/// is running.
fn opened(
    name: String,
    source: RunningSource,
    layer: Layer,
    pusher: media_pp::elements::AppSourceHandle,
    filters: SourceFilters,
    drawn: Drawn,
) -> Result<OpenSource, BackendError> {
    pusher.push(drawn.frame.clone())?;
    Ok(OpenSource {
        media_file: None,
        // Its box is its own rather than something a device answered with,
        // so there is nothing to correct.
        negotiated_size: None,
        source,
        layer,
        name,
        refreshed_token: None,
        filters: filters.open,
        filter_rack: filters.filter_rack,
        showing: true,
        running: true,
        pushed: Some(PushedSurface {
            pusher,
            size: drawn.size,
            content: PushedContent::Text(drawn.settings),
            frame: drawn.frame,
        }),
    })
}

#[cfg(target_os = "windows")]
pub(in crate::engine) fn open(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: Arc<Mutex<windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext>>,
    handle: &media_pp::elements::D3d11VideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenSource, BackendError> {
    use media_pp::elements::{AppSource, D3d11Upload, D3d11VideoCompositorInput};

    let (size, settings) = surface(item)?;
    let name = input_name(item);
    let frame = text_bgra(size[0], size[1], &settings)?;
    // One frame of capacity, as a Drawing has: only the newest string
    // matters, and a deeper queue would put the picture behind the field
    // being typed into.
    let (source, pusher) = AppSource::new(name.clone(), 1);
    let upload = D3d11Upload::new(format!("{name}-upload"), device, size[0], size[1]);
    let FilledRack { rack, filters } = super::filled_rack(
        &name,
        device,
        context,
        filters::ChainFormat::Bgra,
        size,
        item,
    )?;

    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(upload).pipe(rack).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    opened(
        name,
        RunningSource::Owned(pipeline),
        layer,
        pusher,
        filters,
        Drawn {
            frame,
            size,
            settings,
        },
    )
}

#[cfg(target_os = "linux")]
pub(in crate::engine) fn open(
    device: &Arc<media_pp::elements::CudaDevice>,
    handle: &media_pp::elements::CudaVideoCompositorHandle,
    item: &SceneItemSnapshot,
    layer: media_pp::elements::VideoLayer,
) -> Result<OpenSource, BackendError> {
    use media_pp::elements::{AppSource, CudaFrameFormat, CudaUpload, CudaVideoCompositorInput};

    let (size, settings) = surface(item)?;
    let name = input_name(item);
    let frame = text_bgra(size[0], size[1], &settings)?;
    let (source, pusher) = AppSource::new(name.clone(), 1);
    let upload = CudaUpload::new(
        format!("{name}-upload"),
        device,
        CudaFrameFormat::Bgra,
        size[0],
        size[1],
    )?;
    let FilledRack { rack, filters } =
        super::filled_rack(&name, device, filters::ChainFormat::Bgra, size, item)?;

    // No converter, for the reason a Drawing has none: the alpha *is* the
    // text, and NV12 has nowhere to keep one. Converting first would put an
    // opaque black rectangle behind every caption.
    let CudaVideoCompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;
    let pipeline = Pipeline::new(name.clone(), source, move |source, context| {
        let branch = context.branch().pipe(upload).pipe(rack).to(sink)?;
        context.attach(source, 0, branch)?;
        Ok(())
    })?;
    pipeline.run()?;

    opened(
        name,
        RunningSource(pipeline),
        layer,
        pusher,
        filters,
        Drawn {
            frame,
            size,
            settings,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Every test here needs a font, and a machine with none is not a
    /// failing one — it is a machine this feature cannot work on, which the
    /// Source itself reports at open time.
    fn a_font() -> Option<FontArc> {
        font(None).ok()
    }

    fn settings(text: &str, alignment: TextAlignment) -> TextSourceSettings {
        TextSourceSettings {
            size: [400.0, 100.0],
            text: text.to_owned(),
            font: None,
            mode: TextMode::Static,
            clock_format: ClockFormat::default(),
            timer_format: TimerFormat::default(),
            timer: crate::domain::TextTimer::default(),
            font_size: 48.0,
            rgba: [255, 255, 255, 255],
            alignment,
        }
    }

    /// The alpha plane of a frame, as columns that have any glyph in them.
    fn covered_columns(buffer: &MediaBuffer, width: u32, height: u32) -> Vec<u32> {
        let MediaBuffer::Video(frame) = buffer else {
            panic!("a text source produced something that is not video");
        };
        let stride = frame.stride(0);
        let data = frame.data(0);
        (0..width)
            .filter(|column| {
                (0..height).any(|row| data[row as usize * stride + *column as usize * 4 + 3] > 0)
            })
            .collect()
    }

    #[test]
    fn text_with_no_drawable_glyphs_leaves_the_frame_transparent() {
        if a_font().is_none() {
            return;
        }
        for text in ["", "   ", "\n\t"] {
            let frame = text_bgra(400, 100, &settings(text, TextAlignment::Left)).unwrap();
            assert!(
                covered_columns(&frame, 400, 100).is_empty(),
                "{text:?} drew something"
            );
        }
    }

    #[test]
    fn a_line_lands_against_the_edge_it_is_aligned_to() {
        if a_font().is_none() {
            return;
        }
        let (width, height) = (400, 100);
        let placed = |alignment| {
            let frame = text_bgra(width, height, &settings("III", alignment)).unwrap();
            let columns = covered_columns(&frame, width, height);
            (*columns.first().unwrap(), *columns.last().unwrap())
        };

        let (left_first, left_last) = placed(TextAlignment::Left);
        let (centre_first, centre_last) = placed(TextAlignment::Centre);
        let (right_first, right_last) = placed(TextAlignment::Right);

        assert!(left_first < 8, "left-aligned text is not against the edge");
        assert!(
            width - right_last < 8,
            "right-aligned text is not against the edge"
        );
        // Centred rather than merely between the two: the gaps either side
        // are what "centred" means, and equal ones are what a caption that
        // grows in both directions needs.
        let (before, after) = (centre_first, width - centre_last);
        assert!(
            before.abs_diff(after) < 8,
            "centred text sits {before} from the left and {after} from the right"
        );
        assert!(left_first < centre_first && centre_first < right_first);
        assert!(left_last < centre_last && centre_last < right_last);
    }

    /// Text wider than its box is clipped, not drawn past the edge — which
    /// for a frame handed to an upload element would be a write out of
    /// bounds rather than a cosmetic problem.
    #[test]
    fn a_line_too_long_for_its_box_stays_inside_it() {
        if a_font().is_none() {
            return;
        }
        let mut wide = settings(&"W".repeat(200), TextAlignment::Left);
        wide.font_size = 96.0;
        let frame = text_bgra(64, 64, &wide).unwrap();
        let columns = covered_columns(&frame, 64, 64);
        assert!(!columns.is_empty(), "nothing was drawn at all");
        assert!(*columns.last().unwrap() < 64);
    }

    /// The whole way: a caption drawn here, uploaded as BGRA, blended onto
    /// the compositor's own NV12 canvas, and read back in system memory.
    ///
    /// What this establishes is the claim the pipeline is built on — that a
    /// layer with an alpha channel reaches the canvas *as* one. The unit
    /// tests above prove the frame is drawn correctly; only this proves it
    /// survives the trip, and that the transparent nine-tenths of a caption
    /// does not arrive as a black rectangle over the Scene.
    ///
    /// ```text
    /// AppSource(BGRA) ─ CudaUpload ─┐
    ///                               ├─ CudaVideoCompositor ─ CudaDownload ─ AppSink
    ///           (a red background) ─┘
    /// ```
    ///
    /// Needs a CUDA device. Where the machine has none this says so and
    /// returns rather than failing a build that never had a chance — the
    /// same bargain `preview::platform`'s own hardware test makes.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_caption_reaches_the_canvas_without_a_rectangle_around_it() {
        use std::sync::mpsc;
        use std::time::Duration;

        use media_pp::color::Color;
        use media_pp::elements::{
            AppSink, AppSource, CudaDevice, CudaDownload, CudaFrameFormat, CudaUpload,
            CudaVideoCompositor, CudaVideoCompositorInput, VideoCompositorOptions, VideoLayer,
            VideoRect,
        };

        if a_font().is_none() {
            eprintln!("skipped: no font on this machine to draw with");
            return;
        }
        if media_pp::init().is_err() {
            eprintln!("skipped: ffmpeg would not initialize");
            return;
        }
        let Ok(cuda) = CudaDevice::new() else {
            eprintln!("skipped: no CUDA device on this machine");
            return;
        };

        let (width, height) = (256u32, 128u32);
        // Red, so a caption that arrived with its background opaque would
        // black out what it covers and be impossible to miss. Black would
        // hide exactly the defect this is here for.
        let (compositor, handle) = CudaVideoCompositor::new(
            "text-test",
            &cuda,
            VideoCompositorOptions {
                width,
                height,
                frame_rate: ffmpeg::Rational::new(30, 1),
                background: Color::new(255, 0, 0),
            },
        )
        .expect("compositor");

        let mut caption = settings("H", TextAlignment::Centre);
        caption.size = [width as f32, height as f32];
        caption.font_size = 64.0;
        let frame = text_bgra(width, height, &caption).expect("draw the caption");

        let (source, pusher) = AppSource::new("caption", 1);
        let upload = CudaUpload::new(
            "caption-upload",
            &cuda,
            CudaFrameFormat::Bgra,
            width,
            height,
        )
        .expect("upload");
        let CudaVideoCompositorInput { sink, .. } = handle
            .add_source(
                "caption",
                VideoLayer::new(VideoRect::new(0, 0, width, height)),
            )
            .expect("add the caption layer");
        let feeding = Pipeline::new("caption-in", source, move |source, context| {
            let branch = context.branch().pipe(upload).to(sink)?;
            context.attach(source, 0, branch)?;
            Ok(())
        })
        .expect("source pipeline");
        feeding.run().expect("run the source");
        pusher.push(frame).expect("push the caption");

        let (composed, arrived) = mpsc::channel();
        let download = CudaDownload::new("download", &cuda, CudaFrameFormat::Nv12, width, height);
        let sink = AppSink::new("out", move |buffer: MediaBuffer| {
            if let MediaBuffer::Video(frame) = &buffer {
                // The luma plane alone, which is all the two colours here
                // differ in enough to tell apart. Copied out because the
                // frame goes back to its pool when this returns.
                let stride = frame.stride(0);
                let data = frame.data(0);
                let luma: Vec<u8> = (0..height as usize)
                    .flat_map(|row| data[row * stride..row * stride + width as usize].to_vec())
                    .collect();
                let _ = composed.send(luma);
            }
            Ok(())
        });
        let composing = Pipeline::new("compose", compositor, move |source, context| {
            let branch = context.branch().pipe(download).to(Box::new(sink))?;
            context.attach(source, 0, branch)?;
            Ok(())
        })
        .expect("compositor pipeline");
        composing.run().expect("run the compositor");

        // The first frame can be composed before the pushed one has been
        // uploaded, in which case it is background alone. What is being
        // tested is that the caption arrives at all, so this waits for a
        // frame that has it rather than asserting on whichever came first.
        let mut with_caption = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let Ok(luma) = arrived.recv_timeout(Duration::from_secs(5)) else {
                break;
            };
            if luma.iter().any(|value| *value > 150) {
                with_caption = Some(luma);
                break;
            }
        }
        feeding.stop();
        composing.stop();
        let luma = with_caption.expect("no composited frame carried the caption");

        // BT.709 limited-range red is Y=63, and white glyphs are Y=235. The
        // corners are the assertion that matters: they are where an opaque
        // caption would have painted its own background over the red.
        let at = |row: u32, column: u32| luma[(row * width + column) as usize];
        for (row, column) in [
            (0, 0),
            (0, width - 1),
            (height - 1, 0),
            (height - 1, width - 1),
        ] {
            assert_eq!(
                at(row, column),
                63,
                "the caption painted over the background at {row},{column}"
            );
        }
        let lit = luma.iter().filter(|value| **value > 150).count();
        assert!(
            lit > 50,
            "the glyph did not reach the canvas ({lit} pixels)"
        );
    }

    /// The Windows twin of the test above, and the same claim: a layer with
    /// an alpha channel reaches the canvas *as* one, so the transparent
    /// nine-tenths of a caption does not arrive as a rectangle over the
    /// Scene.
    ///
    /// ```text
    /// AppSource(BGRA) ─ D3d11Upload ─┐
    ///                                ├─ D3d11VideoCompositor ─ D3d11Download ─ AppSink
    ///            (a red background) ─┘
    /// ```
    ///
    /// It reads what its twin cannot. The CUDA compositor hands back NV12,
    /// so the Linux half has to settle for the luma plane and compare
    /// against BT.709 limited-range values; this one composites in BGRA and
    /// keeps it all the way to system memory, so the assertion is the colour
    /// itself. `D3d11Download` refusing anything but a BGRA texture is part
    /// of the check rather than an obstacle to it: a compositor that started
    /// producing NV12 would fail here rather than quietly changing what the
    /// numbers below mean.
    ///
    /// Needs a Direct3D 11 device. Where the machine has none this says so
    /// and returns, the same bargain the CUDA half makes.
    #[cfg(target_os = "windows")]
    #[test]
    fn a_caption_reaches_the_canvas_without_a_rectangle_around_it() {
        use std::sync::mpsc;
        use std::time::Duration;

        use media_pp::color::Color;
        use media_pp::elements::{
            AppSink, AppSource, D3d11Download, D3d11Upload, D3d11VideoCompositor,
            D3d11VideoCompositorInput, VideoCompositorOptions, VideoLayer, VideoRect,
        };

        if a_font().is_none() {
            eprintln!("skipped: no font on this machine to draw with");
            return;
        }
        if media_pp::init().is_err() {
            eprintln!("skipped: ffmpeg would not initialize");
            return;
        }
        let Ok((device, context)) = crate::engine::backend::create_device() else {
            eprintln!("skipped: no Direct3D 11 device on this machine");
            return;
        };

        let (width, height) = (256u32, 128u32);
        // Red, so a caption that arrived with its background opaque would
        // black out what it covers and be impossible to miss. Black would
        // hide exactly the defect this is here for.
        let (compositor, handle) = D3d11VideoCompositor::new(
            "text-test",
            &device,
            context.clone(),
            VideoCompositorOptions {
                width,
                height,
                frame_rate: ffmpeg::Rational::new(30, 1),
                background: Color::new(255, 0, 0),
            },
        )
        .expect("compositor");

        let mut caption = settings("H", TextAlignment::Centre);
        caption.size = [width as f32, height as f32];
        caption.font_size = 64.0;
        let frame = text_bgra(width, height, &caption).expect("draw the caption");

        let (source, pusher) = AppSource::new("caption", 1);
        let upload = D3d11Upload::new("caption-upload", &device, width, height);
        let D3d11VideoCompositorInput { sink, .. } = handle
            .add_source(
                "caption",
                VideoLayer::new(VideoRect::new(0, 0, width, height)),
            )
            .expect("add the caption layer")
            .expect("the compositor is running");
        let feeding = Pipeline::new("caption-in", source, move |source, context| {
            let branch = context.branch().pipe(upload).to(sink)?;
            context.attach(source, 0, branch)?;
            Ok(())
        })
        .expect("source pipeline");
        feeding.run().expect("run the source");
        pusher.push(frame).expect("push the caption");

        let (composed, arrived) = mpsc::channel();
        let download = D3d11Download::new("download", &device, context.clone(), width, height)
            .expect("download");
        let sink = AppSink::new("out", move |buffer: MediaBuffer| {
            if let MediaBuffer::Video(frame) = &buffer {
                // Copied out because the frame goes back to its pool when
                // this returns. Four bytes a pixel, B G R A in that order.
                let stride = frame.stride(0);
                let data = frame.data(0);
                let pixels: Vec<u8> = (0..height as usize)
                    .flat_map(|row| data[row * stride..row * stride + width as usize * 4].to_vec())
                    .collect();
                let _ = composed.send(pixels);
            }
            Ok(())
        });
        let composing = Pipeline::new("compose", compositor, move |source, context| {
            let branch = context.branch().pipe(download).to(Box::new(sink))?;
            context.attach(source, 0, branch)?;
            Ok(())
        })
        .expect("compositor pipeline");
        composing.run().expect("run the compositor");

        // The first frame can be composed before the pushed one has been
        // uploaded, in which case it is background alone. What is being
        // tested is that the caption arrives at all, so this waits for a
        // frame that has it rather than asserting on whichever came first.
        // Green, because the background has none and a white glyph is all
        // of it.
        let mut with_caption = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let Ok(pixels) = arrived.recv_timeout(Duration::from_secs(5)) else {
                break;
            };
            if pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[1] > 150) {
                with_caption = Some(pixels);
                break;
            }
        }
        feeding.stop();
        composing.stop();
        let pixels = with_caption.expect("no composited frame carried the caption");

        // The corners are the assertion that matters: they are where an
        // opaque caption would have painted its own background over the red.
        let at = |row: u32, column: u32| {
            let start = ((row * width + column) * 4) as usize;
            [pixels[start], pixels[start + 1], pixels[start + 2]]
        };
        for (row, column) in [
            (0, 0),
            (0, width - 1),
            (height - 1, 0),
            (height - 1, width - 1),
        ] {
            assert_eq!(
                at(row, column),
                [0, 0, 255],
                "the caption painted over the background at {row},{column}"
            );
        }
        let lit = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[1] > 150)
            .count();
        assert!(
            lit > 50,
            "the glyph did not reach the canvas ({lit} pixels)"
        );
    }

    /// A static caption is the string that was typed and nothing else, so
    /// nothing about it changes as time passes — which is what keeps the
    /// engine's tick from redrawing every Text Source in the Scene.
    #[test]
    fn a_static_caption_resolves_to_itself() {
        let stored = settings("hello", TextAlignment::Left);
        assert_eq!(resolved(&stored), stored);
    }

    #[test]
    fn a_timer_counts_from_where_it_was_started_and_stands_still_when_stopped() {
        use crate::domain::TextTimer;

        let stopped = TextTimer {
            running_since: None,
            accumulated: Duration::from_secs(90),
        };
        assert_eq!(stopped.elapsed(0), Duration::from_secs(90));
        // Whatever the clock says, a stopped timer reads the same.
        assert_eq!(
            stopped.elapsed(i64::MAX),
            Duration::from_secs(90),
            "a stopped timer moved"
        );

        let running = TextTimer {
            running_since: Some(1_000_000),
            accumulated: Duration::from_secs(90),
        };
        assert_eq!(running.elapsed(1_000_000), Duration::from_secs(90));
        assert_eq!(running.elapsed(3_500_000), Duration::from_millis(92_500));

        // A system clock moved back past the start of the run stands still
        // rather than reading as an enormous duration, which is what an
        // unsaturated subtraction would give.
        assert_eq!(running.elapsed(0), Duration::from_secs(90));
    }

    #[test]
    fn a_timer_is_written_zero_padded_and_keeps_its_width() {
        use crate::domain::TimerFormat;

        let written = |format, seconds| timer_line(format, Duration::from_secs(seconds));
        assert_eq!(written(TimerFormat::HoursMinutesSeconds, 0), "00:00:00");
        assert_eq!(written(TimerFormat::HoursMinutesSeconds, 5), "00:00:05");
        assert_eq!(written(TimerFormat::HoursMinutesSeconds, 5025), "01:23:45");
        assert_eq!(written(TimerFormat::MinutesSeconds, 5), "00:05");
        assert_eq!(written(TimerFormat::MinutesSeconds, 1425), "23:45");
        // Past an hour the minutes keep counting rather than rolling into a
        // field that is not being shown — 1:23:45 must not read as 23:45.
        assert_eq!(written(TimerFormat::MinutesSeconds, 5025), "83:45");
    }

    /// The clock is whatever the machine's clock says, so what can be
    /// asserted is its shape — and that it is not the fallback, which is
    /// what a broken format description would leave behind.
    #[test]
    fn every_clock_format_writes_something_of_its_own_shape() {
        use crate::domain::ClockFormat;

        for (format, length, separators) in [
            (ClockFormat::Time, 8, 2),
            (ClockFormat::TimeToMinute, 5, 1),
            (ClockFormat::DateAndTime, 19, 2),
            (ClockFormat::Date, 10, 0),
        ] {
            let line = clock_line(format);
            assert_eq!(line.len(), length, "{format:?} wrote {line:?}");
            assert_eq!(
                line.matches(':').count(),
                separators,
                "{format:?} wrote {line:?}"
            );
            assert!(
                line.chars().any(|character| character.is_ascii_digit()),
                "{format:?} wrote no digits at all: {line:?}"
            );
        }
    }

    /// The colour picker's alpha reaches the frame, so a caption can be
    /// translucent without the strokes' own anti-aliasing being lost.
    #[test]
    fn the_chosen_alpha_scales_the_glyph_coverage() {
        if a_font().is_none() {
            return;
        }
        let peak = |alpha| {
            let mut chosen = settings("H", TextAlignment::Left);
            chosen.rgba = [255, 255, 255, alpha];
            let MediaBuffer::Video(frame) = text_bgra(400, 100, &chosen).unwrap() else {
                panic!("not video");
            };
            let stride = frame.stride(0);
            let data = frame.data(0);
            (0..100usize)
                .flat_map(|row| (0..400usize).map(move |column| (row, column)))
                .map(|(row, column)| data[row * stride + column * 4 + 3])
                .max()
                .unwrap()
        };
        assert_eq!(peak(255), 255);
        let half = peak(128);
        assert!(
            (120..=135).contains(&half),
            "a half-transparent caption peaked at {half}"
        );
    }
}
