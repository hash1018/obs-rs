//! What a Display Capture and a Window Capture both are on macOS: one
//! ScreenCaptureKit stream, into the compositor — and every stream this
//! backend has open, and who is drawing from each.
//!
//! One API for both kinds, as the portal is on Linux: what tells a display
//! from a window is which [`ScreenCaptureKitTarget`] is opened. What differs
//! between the two Sources — which target a stored item means, and what it
//! means when that target is not there — is in each kind's own file;
//! everything after that is here.
//!
//! # One stream, however many items show it
//!
//! Shared, as Windows shares a display and a window: each target gets one
//! stream whose `Tee` grows a branch per item, and the stream lives as long
//! as any branch does. Nothing refuses a second stream of one display or
//! window here — ScreenCaptureKit hands out as many as are asked for — so
//! this is the Windows window's plain saving rather than the Windows
//! display's necessity: one stream and one set of pixel buffers where there
//! were two, for a display shown large in one Scene and small in another.
//! Linux declines to share for reasons of the portal's own; none of them
//! apply here.
//!
//! The stream is keyed by the target it resolved to, not by what was stored,
//! so two items naming one window by different titles share it.

use std::sync::Arc;

use media_pp::elements::{
    CompositorInput, ScreenCaptureKitOptions, ScreenCaptureKitSource, ScreenCaptureKitTarget,
    VideoLayer,
};
use media_pp::ffmpeg;
use media_pp::pipeline::Pipeline;
use media_pp::rate::FrameRateHandle;

use crate::engine::backend::{BackendError, Compositor, Gpu, RunningSource, pipeline_ended};
use crate::engine::source::shared::{Registry, Share, Shared, SharedCapture};
use crate::engine::source::{
    FilledRack, OpenOutcome, OpenSource, filled_rack, filters, input_name,
};
use crate::snapshots::SceneItemSnapshot;

/// Every display and window this backend is capturing, keyed by [`key`] —
/// the machinery is [`Registry`]'s. Each keeps its rate handle, so the
/// compositor's rate can change without the stream being reopened.
#[derive(Default)]
pub(in crate::engine) struct ScreenRegistry {
    open: Registry<FrameRateHandle>,
}

impl ScreenRegistry {
    /// Tells every open stream to emit at `fps` — a handle call rather than
    /// a reopen, for the reason the Windows display registry gives.
    pub(in crate::engine) fn set_frame_rate(&self, fps: u32) {
        let rate = ffmpeg::Rational::new(fps as i32, 1);
        self.open.each(|capture| {
            if let Err(error) = capture.extra.set(rate) {
                tracing::warn!("a capture kept its rate: {error}");
            }
        });
    }
}

impl SharedCapture for ScreenRegistry {
    fn detach(&self, key: &str, share: Share) {
        self.open.detach(key, share);
    }

    fn set_showing(&self, key: &str, share: Share, showing: bool) {
        self.open.set_showing(key, share, showing);
    }

    fn stats(&self, key: &str, share: Share) -> Option<media_pp::stats::PipelineStats> {
        self.open.stats(key, share)
    }

    /// A window closed, or a display disconnected, ends its stream. Every
    /// item showing it is then put back to be looked for again, and the
    /// first to find it there again opens it for all of them — unlike a
    /// Windows display, which duplication keeps through a layout change.
    fn ended(&self, key: &str, share: Share) -> bool {
        self.open
            .with_share(key, share, |capture| pipeline_ended(capture.pipeline()))
            // Gone from the registry is gone.
            .unwrap_or(true)
    }
}

/// What a stream of `target` is registered under. A display and a window can
/// carry the same number, so the kind is part of it.
fn key(target: ScreenCaptureKitTarget) -> String {
    match target {
        ScreenCaptureKitTarget::Display(id) => format!("display:{id}"),
        ScreenCaptureKitTarget::Window(id) => format!("window:{id}"),
    }
}

/// Points one SceneItem at `target`'s stream, opening it if this is the
/// first item to want it.
///
/// The pixel buffers ScreenCaptureKit draws are handed on as they are — BGRA
/// VideoToolbox frames, which the compositor draws as well as NV12 and the
/// filters want anyway — so nothing is copied or converted between the two.
/// Each item's own compositor input and filters are still its own, in its
/// own branch, as on Windows.
pub(in crate::engine) fn open(
    screens: &Arc<ScreenRegistry>,
    target: ScreenCaptureKitTarget,
    gpu: &Gpu,
    handle: &Compositor,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<OpenOutcome, BackendError> {
    let name = input_name(item);
    let CompositorInput { sink, layer } = handle.add_source(name.clone(), layer)?;

    let key = key(target);
    let mut kept = None;
    let (share, size) = screens.open.attach(
        &key,
        || open_stream(&key, target, gpu, fps),
        |builder, _size| {
            let FilledRack { rack, filters } =
                filled_rack(&name, gpu, filters::ChainFormat::Bgra, item)?;
            kept = Some(filters);
            Ok(builder.pipe(rack).to(sink)?)
        },
    )?;
    let filters = kept.ok_or("the capture answered without finishing the branch")?;

    Ok(OpenOutcome::Open(OpenSource {
        media_file: None,
        page: None,
        source: RunningSource::Shared {
            capture: Arc::clone(screens) as Arc<dyn SharedCapture>,
            key,
            share,
        },
        layer,
        name,
        refreshed_token: None,
        filters: filters.open,
        filter_rack: filters.filter_rack,
        // What the stream opened at: a display's pixels, a window's size as
        // it was — which it keeps, scaling the window into it. On a stream
        // another item opened first, that item's answer, which is the same.
        negotiated_size: Some(size),
        // Set by the engine where it is opened into a Scene's own
        // composition — see `Target`.
        nested_in: None,
        showing: true,
        running: true,
        pushed: None,
    }))
}

/// Starts one stream into a `Tee` nothing is attached to yet.
fn open_stream(
    key: &str,
    target: ScreenCaptureKitTarget,
    gpu: &Gpu,
    fps: u32,
) -> Result<Shared<FrameRateHandle>, BackendError> {
    // The target's own name rather than any item's: the stream outlives each
    // of them, and this is what the log and the Stats dock show it as.
    let name = key.replace(':', "-");
    let options = ScreenCaptureKitOptions {
        frame_rate: ffmpeg::Rational::new(fps as i32, 1),
        // The pointer belongs to whoever is using the screen, and a
        // recording of it is usually about what is on it — as on the other
        // platforms.
        include_cursor: false,
        ..ScreenCaptureKitOptions::new(target)
    };
    let (source, format) =
        ScreenCaptureKitSource::open_videotoolbox(name.clone(), options, gpu.device())?;
    tracing::info!("opened {key} ({}x{})", format.width, format.height);

    // Before the move below: once the `Pipeline` owns the source there is
    // nothing left to ask it with.
    let frame_rate = source.frame_rate();
    let mut handle = None;
    let (pipeline, ()) = Pipeline::new(name.clone(), source, |source, context| {
        let (branch, tee) = context.tee(format!("{name}-tee")).build_dynamic()?;
        context.attach(source, 0, branch)?;
        handle = Some(tee);
        Ok(())
    })?;
    let tee = handle.expect("the wire closure always produces the TeeHandle");
    pipeline.run()?;

    Ok(Shared::new(
        pipeline,
        tee,
        [format.width, format.height],
        frame_rate,
    ))
}

/// The Windows display registry's tests, on a ScreenCaptureKit stream of the
/// main display. They need screen recording, and skip, saying so, where the
/// process running them has not been allowed it.
#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use media_pp::{
        buffer::MediaBuffer,
        element::Sink,
        elements::AppSink,
        pipeline::{ChainBuilder, DetachedBranch},
    };

    use super::*;

    /// Ends a branch at `sink` and nothing else — no filters between.
    fn end(
        sink: Box<dyn Sink>,
    ) -> impl FnOnce(ChainBuilder, [u32; 2]) -> Result<DetachedBranch, BackendError> {
        move |builder, _| Ok(builder.to(sink)?)
    }

    /// A branch end that counts the pictures reaching it, under `name` —
    /// what the Stats dock reads a Source's compositor input by.
    fn counting_as(name: &str) -> (Box<dyn Sink>, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let sink = AppSink::new(name, move |buffer: MediaBuffer| {
            if matches!(buffer, MediaBuffer::Video(_)) {
                seen.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        });
        (Box::new(sink), count)
    }

    /// Whether `count` moves within a few seconds.
    fn moves(count: &AtomicUsize) -> bool {
        let from = count.load(Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if count.load(Ordering::SeqCst) > from {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// Held for a whole test, so two do not share one stream between them.
    static DISPLAY: Mutex<()> = Mutex::new(());

    /// The device, the main display as a target, and the key it is
    /// registered under — or why there are none.
    fn the_main_display() -> Result<(Gpu, ScreenCaptureKitTarget, String), String> {
        let gpu = Gpu::open().map_err(|error| format!("no VideoToolbox: {error}"))?;
        let target = ScreenCaptureKitTarget::Display(objc2_core_graphics::CGMainDisplayID());
        Ok((gpu, target, key(target)))
    }

    /// An item joining a stream another Scene's item paused on leaving gets
    /// pictures — see the Windows twin.
    #[test]
    fn an_item_joining_a_paused_stream_resumes_it() {
        let _display = DISPLAY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (gpu, target, key) = match the_main_display() {
            Ok(found) => found,
            Err(reason) => return eprintln!("skipped: {reason}"),
        };
        let screens = ScreenRegistry::default();
        let (sink, _) = counting_as("scene-item-1");
        let (scene_1, _) =
            match screens
                .open
                .attach(&key, || open_stream(&key, target, &gpu, 30), end(sink))
            {
                Ok(attached) => attached,
                Err(error) => return eprintln!("skipped: could not capture the display: {error}"),
            };
        screens.set_showing(&key, scene_1, false);

        let (sink, count) = counting_as("scene-item-2");
        let (scene_2, _) = screens
            .open
            .attach(&key, || unreachable!("the stream is open"), end(sink))
            .expect("join the open stream");
        assert!(moves(&count), "the new item's branch gets pictures");

        screens.detach(&key, scene_2);
        screens.detach(&key, scene_1);
    }

    /// An item removed while shown leaves the stream paused when the only
    /// one left is in a Scene that is not — see the Windows twin.
    #[test]
    fn removing_the_last_shown_item_pauses_the_stream() {
        let _display = DISPLAY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (gpu, target, key) = match the_main_display() {
            Ok(found) => found,
            Err(reason) => return eprintln!("skipped: {reason}"),
        };
        let screens = ScreenRegistry::default();
        let (sink, count) = counting_as("scene-item-1");
        let (hidden, _) =
            match screens
                .open
                .attach(&key, || open_stream(&key, target, &gpu, 30), end(sink))
            {
                Ok(attached) => attached,
                Err(error) => return eprintln!("skipped: could not capture the display: {error}"),
            };
        screens.set_showing(&key, hidden, false);
        let (sink, _) = counting_as("scene-item-2");
        let (shown, _) = screens
            .open
            .attach(&key, || unreachable!("the stream is open"), end(sink))
            .expect("join the open stream");
        assert!(moves(&count), "running while one item is shown");

        screens.detach(&key, shown);
        // Pausing reaches the stream's own thread asynchronously; a frame
        // already on its way may still land.
        std::thread::sleep(Duration::from_millis(200));
        assert!(!moves(&count), "paused once nothing shown draws from it");

        screens.detach(&key, hidden);
    }

    /// Each item sharing a stream is reported by its own branch, and only by
    /// that — see the Windows twin.
    #[test]
    fn each_item_sharing_a_stream_is_reported_by_its_own_branch() {
        let _display = DISPLAY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (gpu, target, key) = match the_main_display() {
            Ok(found) => found,
            Err(reason) => return eprintln!("skipped: {reason}"),
        };
        let screens = ScreenRegistry::default();
        let (sink, first_count) = counting_as("scene-item-1");
        let (first, _) =
            match screens
                .open
                .attach(&key, || open_stream(&key, target, &gpu, 30), end(sink))
            {
                Ok(attached) => attached,
                Err(error) => return eprintln!("skipped: could not capture the display: {error}"),
            };
        let (sink, _) = counting_as("scene-item-2");
        let (second, _) = screens
            .open
            .attach(&key, || unreachable!("the stream is open"), end(sink))
            .expect("join the open stream");
        assert!(moves(&first_count), "the stream is delivering");

        let names = |branch| -> Vec<String> {
            screens
                .stats(&key, branch)
                .expect("the stream is open")
                .elements
                .iter()
                .map(|element| element.name.to_string())
                .collect()
        };
        assert_eq!(names(first), ["scene-item-1"]);
        assert_eq!(names(second), ["scene-item-2"]);

        screens.detach(&key, second);
        screens.detach(&key, first);
        assert!(
            screens.stats(&key, first).is_none(),
            "a stream that has gone has nothing to report"
        );
    }
}
