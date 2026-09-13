//! One picture of what is being composited, written as a PNG.
//!
//! # A branch that takes one frame and goes
//!
//! The Canvas exists as the compositor's output and nowhere else: the Preview
//! is a scaled copy drawn at its own rate, and reading it back would be a
//! picture of the Preview rather than of what a recording would hold. So a
//! screenshot is a branch on the same `Tee` a recording hangs off:
//!
//! ```text
//! compositor Tee ─ Queue(1, drop newest) ─ download ─ RGB24 ─ this sink
//! ```
//!
//! The sink writes the first frame that reaches it and ignores the rest, and
//! the engine detaches the branch as soon as it hears — see
//! `EngineCommand::ScreenshotTaken`. Detaching from inside the sink would be
//! the branch's own thread tearing the branch down around itself.
//!
//! The queue drops rather than blocks, unlike a recording's: a screenshot
//! that is late costs nothing, and one that made the compositor wait while a
//! PNG was compressed would be a stutter in the recording beside it.
//!
//! # Named for when it was taken
//!
//! Beside the recordings, under their prefix — see
//! [`crate::paths::screenshot_file_in`]. That stamp is one second fine, so a
//! second screenshot inside the same second is written as `-2` rather than
//! over the first; the file is created with `create_new`, so two cannot pick
//! the same name either.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};

use media_pp::{buffer::MediaBuffer, element::Sink, elements::AppSink, ffmpeg};

/// What one screenshot came to: the file it wrote, or why none was written.
pub(in crate::engine) type Taken = Result<PathBuf, String>;

/// How many names past the first are tried before giving up — far more
/// screenshots than one second holds.
const MAX_SUFFIX: u32 = 1_000;

/// The end of a screenshot's branch: writes the first RGB24 frame it is
/// handed to a file beside `path`, and says how that went through `done`.
///
/// Every frame after the first is taken and dropped, for the moment between
/// this answering and the engine detaching it.
pub(in crate::engine) fn sink(
    path: PathBuf,
    done: impl FnOnce(Taken) + Send + 'static,
) -> Box<dyn Sink> {
    let mut pending = Some((path, done));
    Box::new(AppSink::new("screenshot", move |buffer| {
        let MediaBuffer::Video(frame) = &buffer else {
            return Ok(());
        };
        if let Some((path, done)) = pending.take() {
            done(write_png(&path, frame).map_err(|error| error.to_string()));
        }
        Ok(())
    }))
}

/// Writes an RGB24 frame as a PNG, under `path` or the first free name
/// beside it, and answers where it went.
///
/// A file that could not be finished is removed rather than left half
/// written: a PNG that stops partway opens as a broken image, which is worse
/// than no file beside the message saying why.
fn write_png(path: &Path, frame: &ffmpeg::frame::Video) -> io::Result<PathBuf> {
    if frame.format() != ffmpeg::format::Pixel::RGB24 {
        return Err(io::Error::other(format!(
            "expected an RGB24 frame, got {:?}",
            frame.format()
        )));
    }
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let (file, written) = create_unique(path)?;
    if let Err(error) = encode(file, frame) {
        let _ = std::fs::remove_file(&written);
        return Err(error);
    }
    Ok(written)
}

fn encode(file: File, frame: &ffmpeg::frame::Video) -> io::Result<()> {
    let (width, height) = (frame.width(), frame.height());
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(io::Error::other)?;

    // The frame's rows are padded out to its stride, and a PNG's are not.
    let row = width as usize * 3;
    let stride = frame.stride(0);
    let data = frame.data(0);
    let mut pixels = Vec::with_capacity(row * height as usize);
    for line in 0..height as usize {
        pixels.extend_from_slice(&data[line * stride..line * stride + row]);
    }
    writer.write_image_data(&pixels).map_err(io::Error::other)?;
    writer.finish().map_err(io::Error::other)
}

/// Opens `path` for writing if nothing is there, or `stem-2.ext`, `stem-3.ext`
/// and so on — never a file that already exists.
fn create_unique(path: &Path) -> io::Result<(File, PathBuf)> {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned())
        .unwrap_or_default();
    for suffix in 1..=MAX_SUFFIX {
        let candidate = if suffix == 1 {
            path.to_path_buf()
        } else {
            path.with_file_name(format!("{stem}-{suffix}.{extension}"))
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((file, candidate)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "{} and {MAX_SUFFIX} names after it are taken",
            path.display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of this test's own, emptied first.
    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("obs-rs-screenshot-{name}"));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    /// A small RGB24 frame whose every pixel says where it is, with its rows
    /// padded past their width the way a decoder's or a scaler's are.
    fn frame(width: u32, height: u32) -> ffmpeg::frame::Video {
        media_pp::init().expect("ffmpeg initializes");
        let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::RGB24, width, height);
        let stride = frame.stride(0);
        assert!(stride >= width as usize * 3);
        let data = frame.data_mut(0);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let at = y * stride + x * 3;
                data[at..at + 3].copy_from_slice(&[x as u8 * 40, y as u8 * 90, 7]);
            }
        }
        frame
    }

    fn decode(path: &Path) -> (u32, u32, Vec<u8>) {
        let decoder = png::Decoder::new(io::BufReader::new(File::open(path).unwrap()));
        let mut reader = decoder.read_info().expect("a PNG header");
        let mut pixels = vec![0; reader.output_buffer_size().expect("a sized image")];
        let info = reader.next_frame(&mut pixels).expect("the image data");
        pixels.truncate(info.buffer_size());
        (info.width, info.height, pixels)
    }

    /// What is written is the picture, row for row: the stride padding is
    /// left behind, and nothing is shifted or swapped on the way.
    #[test]
    fn a_frame_is_written_pixel_for_pixel_without_its_padding() {
        let directory = scratch("pixels");
        let written = write_png(&directory.join("shot.png"), &frame(3, 2)).expect("written");

        let (width, height, pixels) = decode(&written);
        assert_eq!((width, height), (3, 2));
        assert_eq!(
            pixels,
            [
                0, 0, 7, 40, 0, 7, 80, 0, 7, //
                0, 90, 7, 40, 90, 7, 80, 90, 7,
            ]
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Two screenshots inside one second share a stamp, and the second must
    /// not be written over the first.
    #[test]
    fn a_name_already_taken_gets_a_suffix_instead_of_being_overwritten() {
        let directory = scratch("suffix");
        let path = directory.join("obs-rs-2026-09-13-143005.png");

        let first = write_png(&path, &frame(2, 2)).expect("first");
        let second = write_png(&path, &frame(2, 2)).expect("second");

        assert_eq!(first, path);
        assert_eq!(second, directory.join("obs-rs-2026-09-13-143005-2.png"));
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// Only the first frame is written: the branch goes on delivering until
    /// the engine detaches it, and each of those would otherwise be a file.
    #[test]
    fn the_sink_writes_the_first_frame_and_reports_it_once() {
        let directory = scratch("once");
        let path = directory.join("shot.png");
        let (tell, heard) = std::sync::mpsc::channel();
        let mut sink = sink(path.clone(), move |taken| {
            tell.send(taken).expect("the test is listening");
        });

        let pooled = |frame| {
            let pool =
                media_pp::pool::UnboundObjectPool::new(0, ffmpeg::frame::Video::empty, |_| {});
            let mut slot = pool.get();
            *slot = frame;
            MediaBuffer::Video(std::sync::Arc::new(slot))
        };
        sink.consume(pooled(frame(2, 2))).expect("first");
        sink.consume(pooled(frame(2, 2))).expect("second");

        assert_eq!(heard.try_recv().expect("reported"), Ok(path.clone()));
        assert!(heard.try_recv().is_err(), "reported once");
        assert!(!directory.join("shot-2.png").exists(), "and written once");
        let _ = std::fs::remove_dir_all(&directory);
    }
}
