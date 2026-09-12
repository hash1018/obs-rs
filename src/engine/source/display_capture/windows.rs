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

use std::collections::{HashMap, HashSet};
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
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

use media_pp::elements::{D3d11VideoCompositorHandle, D3d11VideoCompositorInput, VideoLayer};

use crate::domain::{DisplayCaptureTarget, SourceSettings};
use crate::engine::backend::{BackendError, RunningSource};
use crate::engine::source::{OpenSource, input_name};
use crate::snapshots::SceneItemSnapshot;

/// One display's capture, and what is currently drawing from it.
pub(in crate::engine) struct SharedCapture {
    pipeline: Arc<Pipeline>,
    tee: TeeHandle,
    /// Taken before the source was moved into its `Pipeline`, which is the
    /// only chance to. It is what lets the compositor's rate change without
    /// this capture being closed and reopened underneath it.
    frame_rate: FrameRateHandle,
    /// What the duplication actually opened at. Kept here rather than read
    /// per item because the capture is shared: every item drawing this
    /// display is drawing the same picture, so they all correct their stored
    /// hint against one answer.
    size: [u32; 2],
    /// The branches whose SceneItem is in the Scene being shown. The capture
    /// runs while there are any and pauses when there are none — the shared
    /// form of "a Source whose item left the Scene stops running".
    ///
    /// Branches rather than a count, and compared against `running` rather
    /// than acted on at the count's edges, because a count can be moved by
    /// a path that forgets to tell the pipeline. `attach` did exactly that:
    /// it counted a new item as showing without resuming a capture the other
    /// Scene had paused, so a second Scene showing the same display stayed
    /// black. Every change now goes through [`SharedCapture::show`].
    showing: HashSet<BranchId>,
    /// Whether the pipeline was last told to run. `showing` says whether it
    /// should.
    running: bool,
}

impl SharedCapture {
    /// Counts `branch` into or out of the Scene being shown, and runs or
    /// pauses the capture to match.
    ///
    /// Idempotent per branch, so hiding an item twice, or detaching one
    /// already hidden, cannot take another item's share with it.
    fn show(&mut self, branch: BranchId, showing: bool) {
        if showing {
            self.showing.insert(branch);
        } else {
            self.showing.remove(&branch);
        }
        let running = !self.showing.is_empty();
        if running != self.running {
            if running {
                self.pipeline.resume();
            } else {
                self.pipeline.pause();
            }
            self.running = running;
        }
    }
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
    ) -> Result<(BranchId, [u32; 2]), BackendError> {
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
        // A new item is added to the Scene being shown, but the capture may
        // have been paused by another Scene's item leaving it.
        capture.show(id, true);
        Ok((id, capture.size))
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
        // An item removed while shown was never hidden first, and would
        // otherwise keep the capture running for Scenes that are not.
        capture.show(branch, false);
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
    pub(in crate::engine) fn set_showing(&self, monitor: &str, branch: BranchId, showing: bool) {
        let mut open = self.lock();
        if let Some(capture) = open.get_mut(monitor) {
            capture.show(branch, showing);
        }
    }

    /// What one item's branch of a capture is doing, for the Stats dock.
    ///
    /// Only that branch's elements: the capture itself is every sharing
    /// item's, and reported per item it would be counted once for each. The
    /// branch ends in the item's own compositor input, which is the element
    /// the dock reads a Source from anyway — whether frames are reaching
    /// the Canvas, and when they stopped.
    pub(in crate::engine) fn stats(
        &self,
        monitor: &str,
        branch: BranchId,
    ) -> Option<media_pp::stats::PipelineStats> {
        // The pipeline is taken out of the lock before it is read: a reading
        // takes the graph's own lock, and nothing about it needs this one.
        let pipeline = Arc::clone(&self.lock().get(monitor)?.pipeline);
        let mut stats = pipeline.stats();
        stats
            .elements
            .retain(|element| element.branch == Some(branch));
        Some(stats)
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
    let output_index = resolve_output_index(monitor)?;
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
        size: [format.width, format.height],
        // Filled by whoever attaches the first branch, which finds the
        // pipeline already running and so has nothing to tell it.
        showing: HashSet::new(),
        running: true,
    })
}

/// Points one SceneItem at a display's capture, opening it if this is the
/// first item to want it.
pub(in crate::engine) fn open(
    device: &ID3D11Device,
    handle: &D3d11VideoCompositorHandle,
    captures: &Arc<CaptureRegistry>,
    item: &SceneItemSnapshot,
    layer: VideoLayer,
    fps: u32,
) -> Result<OpenSource, BackendError> {
    let SourceSettings::DisplayCapture(settings) = &item.settings else {
        return Err("scene item is not a display capture".into());
    };
    let DisplayCaptureTarget::MonitorName(monitor) = &settings.target else {
        // A portal restore token belongs to a Wayland compositor; nothing on
        // Windows can resolve it, so a project moved across platforms gets an
        // error naming the actual problem rather than a capture of the wrong
        // display.
        return Err("a portal selection names no display Windows can resolve".into());
    };

    let name = input_name(item);
    let D3d11VideoCompositorInput { sink, layer } = handle
        .add_source(name.clone(), layer)?
        .ok_or("the compositor is no longer running")?;
    // The capture is shared, so what this item gets is a branch of it. Its
    // own compositor input is still its own: position, size and z-order stay
    // per item even when the pixels behind two of them are the same.
    let (branch, size) = captures.attach(monitor, device, fps, sink)?;

    Ok(OpenSource {
        media_file: None,
        source: RunningSource::Shared {
            captures: Arc::clone(captures),
            monitor: monitor.clone(),
            branch,
        },
        layer,
        name,
        refreshed_token: None,
        filters: Vec::new(),
        filter_rack: None,
        // The display layout can change between runs, so the size a picker
        // reported when the item was added is a hint rather than a fact —
        // this is what duplication actually opened.
        negotiated_size: Some(size),
        showing: true,
        running: true,
        pushed: None,
    })
}

/// Resolves a stable display name such as `\\.\DISPLAY1` to the flat output
/// index [`CaptureArea::Output`] takes — adapter 0's outputs, then adapter
/// 1's, matching that variant's own documented order.
///
/// Resolved at open time against whatever layout is live, not persisted: the
/// name is the stable half, the index is whatever it maps to today.
fn resolve_output_index(monitor: &str) -> Result<u32, BackendError> {
    // SAFETY: enumeration creates and reads only its own COM objects, and
    // `GetDesc` writes one fully-sized descriptor into a live local.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut flat_index = 0u32;
        for adapter_index in 0.. {
            let Ok(adapter) = factory.EnumAdapters1(adapter_index) else {
                break;
            };
            for output_index in 0.. {
                let Ok(output) = adapter.EnumOutputs(output_index) else {
                    break;
                };
                let desc = output.GetDesc()?;
                let name_end = desc
                    .DeviceName
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(desc.DeviceName.len());
                if String::from_utf16_lossy(&desc.DeviceName[..name_end]) == monitor {
                    return Ok(flat_index);
                }
                flat_index += 1;
            }
        }
    }
    Err(format!("display \"{monitor}\" was not found in the current layout").into())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use media_pp::{buffer::MediaBuffer, elements::AppSink};

    use super::*;

    /// A branch end that counts the pictures reaching it.
    fn counting() -> (Box<dyn Sink>, Arc<AtomicUsize>) {
        counting_as("count")
    }

    /// The same, under a name of its own — what the Stats dock reads a
    /// Source's compositor input by.
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

    /// Held for a whole test. One process can duplicate a display only once
    /// at a time — the limit this module exists to work around — so two of
    /// these running side by side would refuse each other.
    static DISPLAY: Mutex<()> = Mutex::new(());

    /// A device and the name of a display on its adapter, or why there are
    /// none — a CI runner has no monitor to duplicate.
    fn a_display() -> Result<(ID3D11Device, String), String> {
        media_pp::init().map_err(|error| format!("ffmpeg would not initialize: {error}"))?;
        let (device, _) = crate::engine::backend::create_device()
            .map_err(|error| format!("no Direct3D 11 device: {error}"))?;
        // SAFETY: as in `resolve_output_index`.
        let name = unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| e.to_string())?;
            let output = factory
                .EnumAdapters1(0)
                .and_then(|adapter| adapter.EnumOutputs(0))
                .map_err(|_| "no display attached to the default adapter".to_owned())?;
            let desc = output.GetDesc().map_err(|e| e.to_string())?;
            let end = desc
                .DeviceName
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(desc.DeviceName.len());
            String::from_utf16_lossy(&desc.DeviceName[..end])
        };
        Ok((device, name))
    }

    /// An item added to a Scene whose display another Scene's item has
    /// already opened, and paused on leaving, gets pictures.
    ///
    /// The case that was black: Scene 1 shows a display, the user switches
    /// to a new Scene 2 and adds the same display there. The capture was
    /// shared, so the new item joined it — paused — and nothing resumed it.
    #[test]
    fn an_item_joining_a_paused_capture_resumes_it() {
        let _display = DISPLAY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (device, monitor) = match a_display() {
            Ok(found) => found,
            Err(reason) => return eprintln!("skipped: {reason}"),
        };
        let captures = CaptureRegistry::default();

        let (sink, _) = counting();
        let (scene_1, _) = match captures.attach(&monitor, &device, 30, sink) {
            Ok(attached) => attached,
            Err(error) => return eprintln!("skipped: could not duplicate {monitor}: {error}"),
        };
        captures.set_showing(&monitor, scene_1, false);

        let (sink, count) = counting();
        let (scene_2, _) = captures
            .attach(&monitor, &device, 30, sink)
            .expect("join the open capture");
        assert!(moves(&count), "the new item's branch gets pictures");

        captures.detach(&monitor, scene_2);
        captures.detach(&monitor, scene_1);
    }

    /// An item removed while shown leaves the capture paused when the only
    /// one left is in a Scene that is not.
    ///
    /// Removal stops an item without hiding it first, so a registry that
    /// only counted hides would have gone on capturing for nobody.
    #[test]
    fn removing_the_last_shown_item_pauses_the_capture() {
        let _display = DISPLAY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (device, monitor) = match a_display() {
            Ok(found) => found,
            Err(reason) => return eprintln!("skipped: {reason}"),
        };
        let captures = CaptureRegistry::default();

        let (sink, count) = counting();
        let (hidden, _) = match captures.attach(&monitor, &device, 30, sink) {
            Ok(attached) => attached,
            Err(error) => return eprintln!("skipped: could not duplicate {monitor}: {error}"),
        };
        captures.set_showing(&monitor, hidden, false);
        let (sink, _) = counting();
        let (shown, _) = captures
            .attach(&monitor, &device, 30, sink)
            .expect("join the open capture");
        assert!(moves(&count), "running while one item is shown");

        captures.detach(&monitor, shown);
        // Pausing reaches the capture's own thread asynchronously; a frame
        // already on its way may still land.
        std::thread::sleep(Duration::from_millis(200));
        assert!(!moves(&count), "paused once nothing shown draws from it");

        captures.detach(&monitor, hidden);
    }

    /// Each item sharing a capture is reported by its own branch, and only
    /// by that.
    ///
    /// The Stats dock reads a Source from its compositor input. A shared
    /// capture's pipeline holds every sharing item's input, so reading all of
    /// it for each item would credit one item with another's frames — and
    /// reading none of it, which is what this replaced, left a Display
    /// Capture with no row at all.
    #[test]
    fn each_item_sharing_a_capture_is_reported_by_its_own_branch() {
        let _display = DISPLAY
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (device, monitor) = match a_display() {
            Ok(found) => found,
            Err(reason) => return eprintln!("skipped: {reason}"),
        };
        let captures = CaptureRegistry::default();

        let (sink, first_count) = counting_as("scene-item-1");
        let (first, _) = match captures.attach(&monitor, &device, 30, sink) {
            Ok(attached) => attached,
            Err(error) => return eprintln!("skipped: could not duplicate {monitor}: {error}"),
        };
        let (sink, _) = counting_as("scene-item-2");
        let (second, _) = captures
            .attach(&monitor, &device, 30, sink)
            .expect("join the open capture");
        assert!(moves(&first_count), "the capture is delivering");

        let names = |branch| -> Vec<String> {
            captures
                .stats(&monitor, branch)
                .expect("the capture is open")
                .elements
                .iter()
                .map(|element| element.name.to_string())
                .collect()
        };
        assert_eq!(names(first), ["scene-item-1"]);
        assert_eq!(names(second), ["scene-item-2"]);
        let delivered = captures
            .stats(&monitor, first)
            .expect("the capture is open")
            .elements[0]
            .buffers_in;
        assert!(delivered > 0, "the branch's own count is what is read");

        captures.detach(&monitor, second);
        captures.detach(&monitor, first);
        assert!(
            captures.stats(&monitor, first).is_none(),
            "a capture that has gone has nothing to report"
        );
    }
}
