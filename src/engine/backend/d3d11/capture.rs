//! Every desktop duplication this backend has open, and who is drawing from
//! each.
//!
//! Desktop Duplication refuses to open the same output twice on one device —
//! the second `DuplicateOutput` fails with `E_INVALIDARG` — and this backend
//! has exactly one device by design, since sharing it is what makes capture,
//! compositing and the Preview zero-copy. So two SceneItems showing the same
//! display cannot each open it, which is what left the second one a silent
//! black rectangle.
//!
//! They share one capture instead. Each display gets a pipeline whose `Tee`
//! grows a branch per item, and the capture lives as long as any branch does.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use media_pp::{
    element::Sink,
    elements::{
        CaptureArea, CaptureMode, DxgiCaptureOptions, DxgiCaptureSource, TeeBuilder, TeeHandle,
    },
    ffmpeg,
    graph::BranchId,
    pipeline::Pipeline,
    rate::FrameRateHandle,
};
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

use super::BackendError;

/// One display's capture, and what is currently drawing from it.
struct SharedCapture {
    pipeline: Arc<Pipeline>,
    tee: TeeHandle,
    /// Taken before the source was moved into its `Pipeline`, which is the
    /// only chance to. It is what lets the compositor's rate change without
    /// this capture being closed and reopened underneath it.
    frame_rate: FrameRateHandle,
    /// How many branches belong to a SceneItem in the Scene being shown. The
    /// capture runs while this is above zero and pauses when it reaches it —
    /// the shared form of "a Source whose item left the Scene stops running".
    showing: usize,
}

/// The open captures, keyed by the display each duplicates.
#[derive(Default)]
pub(in crate::engine) struct CaptureRegistry {
    open: Mutex<HashMap<String, SharedCapture>>,
}

impl CaptureRegistry {
    /// Points one more compositor input at `monitor`, opening its capture if
    /// this is the first item to ask.
    ///
    /// The returned id names this item's branch and nothing else, so removing
    /// it later cannot disturb another item sharing the same capture.
    pub(in crate::engine) fn attach(
        &self,
        monitor: &str,
        device: &ID3D11Device,
        fps: u32,
        sink: Box<dyn Sink>,
    ) -> Result<BranchId, BackendError> {
        let mut open = self.lock();
        if !open.contains_key(monitor) {
            let capture = open_capture(monitor, device, fps)?;
            open.insert(monitor.to_owned(), capture);
        }
        let capture = open
            .get_mut(monitor)
            .expect("the capture was just inserted if it was missing");

        // Every branch is attached at runtime, the first one included: a
        // branch handed to `TeeBuilder` is fixed and has no id, and this one
        // has to be removable when its item goes away.
        let branch = capture
            .tee
            .branch()
            .ok_or("the capture for this display has stopped")?
            .to(sink)?;
        let id = capture.tee.attach(branch)?;
        capture.showing += 1;
        Ok(id)
    }

    /// Removes one item's branch, and the capture itself once the last branch
    /// is gone.
    pub(in crate::engine) fn detach(&self, monitor: &str, branch: BranchId) {
        let mut open = self.lock();
        let Some(capture) = open.get_mut(monitor) else {
            return;
        };
        if let Err(error) = capture.tee.detach(branch) {
            eprintln!("could not detach a capture branch: {error}");
        }
        // Only once nothing draws from it: another SceneItem may still be
        // showing this display.
        if capture.tee.sink_count() == 0
            && let Some(capture) = open.remove(monitor)
        {
            capture.pipeline.stop();
        }
    }

    /// Follows one item into or out of the Scene being shown.
    ///
    /// The capture keeps running while any item shows it, so this only
    /// reaches the pipeline at the transitions to and from none.
    pub(in crate::engine) fn set_showing(&self, monitor: &str, showing: bool) {
        let mut open = self.lock();
        let Some(capture) = open.get_mut(monitor) else {
            return;
        };
        if showing {
            capture.showing += 1;
            if capture.showing == 1 {
                capture.pipeline.resume();
            }
        } else {
            capture.showing = capture.showing.saturating_sub(1);
            if capture.showing == 0 {
                capture.pipeline.pause();
            }
        }
    }

    /// Tells every open capture to emit at `fps`.
    ///
    /// A handle call rather than a reopen: the compositor's rate is a setting,
    /// and closing and reopening a display duplication to follow it would put
    /// a gap in the Preview each time one was applied.
    pub(in crate::engine) fn set_frame_rate(&self, fps: u32) {
        let rate = ffmpeg::Rational::new(fps as i32, 1);
        for capture in self.lock().values() {
            capture.frame_rate.set(rate);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SharedCapture>> {
        self.open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Starts duplicating one display into a `Tee` nothing is attached to yet.
fn open_capture(
    monitor: &str,
    device: &ID3D11Device,
    fps: u32,
) -> Result<SharedCapture, BackendError> {
    let output_index = super::resolve_output_index(monitor)?;
    let name = format!("display-{output_index}");
    // GPU capture: the desktop lands in D3D11 textures on this backend's own
    // device and never reaches system memory. A monitor on another adapter is
    // rejected here rather than bridged through a CPU copy, which is the
    // point — a silent fallback would undo the whole arrangement.
    let (source, format) = DxgiCaptureSource::open_with_device(
        name.clone(),
        DxgiCaptureOptions {
            area: CaptureArea::Output { output_index },
            fps,
            capture_mode: CaptureMode::Gpu,
        },
        device,
    )?;
    eprintln!(
        "opened {monitor} as output {output_index} ({}x{})",
        format.width, format.height
    );

    // Before the move below: once the `Pipeline` owns the source there is
    // nothing left to ask it with.
    let frame_rate = source.frame_rate();

    // Capture gives BGRA D3D11 textures and the compositor takes exactly
    // those, so unlike the CUDA side nothing converts between them.
    let mut handle = None;
    let pipeline = Pipeline::new(name.clone(), source, |source, context| {
        let (branch, tee) =
            TeeBuilder::new(format!("{name}-tee"), context.clone()).build_dynamic()?;
        context.attach(source, 0, branch)?;
        handle = Some(tee);
        Ok(())
    })?;
    let tee = handle.expect("the wire closure always produces the TeeHandle");
    pipeline.run()?;

    Ok(SharedCapture {
        pipeline,
        tee,
        frame_rate,
        // Counted by whoever attaches the first branch.
        showing: 0,
    })
}
