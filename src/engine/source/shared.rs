//! One capture, several items drawing from it.
//!
//! Two kinds need this, for the same reason and with different consequences:
//! a display cannot be duplicated twice on one device at all, and a camera
//! opened twice keeps ending and reopening on both sides. Either way the
//! answer is one capture with a `Tee` that grows a branch per SceneItem, and
//! this is the part of that arrangement neither kind has anything to say
//! about — attaching a branch, letting it go, following it into and out of
//! the Scene being shown, and reading what only it did.
//!
//! What each kind keeps to itself is how its capture is opened, and whatever
//! it holds beside the pipeline: the display's rate handle, so the
//! compositor's own rate can change without the duplication being reopened.
//! That is `E` here, and nothing in this module reads it.
//!
//! Windows only in effect, so far: the CUDA backend opens a capture per
//! SceneItem still, so nothing on Linux reaches any of this. Compiled there
//! rather than cut out of the build, so it keeps type-checking on the
//! platform that has yet to use it.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use media_pp::{
    elements::TeeHandle,
    graph::BranchId,
    pipeline::{ChainBuilder, DetachedBranch, Pipeline},
};

use crate::engine::backend::BackendError;

/// One open capture, and what is currently drawing from it.
pub(in crate::engine) struct Shared<E> {
    pipeline: Arc<Pipeline>,
    tee: TeeHandle,
    /// What the capture actually opened at. Kept here rather than read per
    /// item because it is shared: every item drawing it is drawing the same
    /// picture, so they all correct their stored hint against one answer.
    size: [u32; 2],
    /// The branches whose SceneItem is in the Scene being shown. The capture
    /// runs while there are any and pauses when there are none — the shared
    /// form of "a Source whose item left the Scene stops running".
    ///
    /// Branches rather than a count, and compared against `running` rather
    /// than acted on at the count's edges, because a count can be moved by a
    /// path that forgets to tell the pipeline. `attach` did exactly that: it
    /// counted a new item as showing without resuming a capture the other
    /// Scene had paused, so a second Scene showing the same display stayed
    /// black. Every change now goes through [`Shared::show`].
    showing: HashSet<BranchId>,
    /// Whether the pipeline was last told to run. `showing` says whether it
    /// should.
    running: bool,
    /// Whatever the kind that opened this keeps beside it.
    pub(in crate::engine) extra: E,
}

impl<E> Shared<E> {
    /// A capture that is open and running, with nothing attached yet.
    pub(in crate::engine) fn new(
        pipeline: Arc<Pipeline>,
        tee: TeeHandle,
        size: [u32; 2],
        extra: E,
    ) -> Self {
        Self {
            pipeline,
            tee,
            size,
            // Filled by whoever attaches the first branch, which finds the
            // pipeline already running and so has nothing to tell it.
            showing: HashSet::new(),
            running: true,
            extra,
        }
    }

    /// The capture's own pipeline, for the questions only its kind asks —
    /// whether a camera has ended, what a rate handle should be told.
    pub(in crate::engine) fn pipeline(&self) -> &Arc<Pipeline> {
        &self.pipeline
    }

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

/// Every capture of one kind that is open, by whatever names its target —
/// a display name, a camera's device link.
pub(in crate::engine) struct Registry<E> {
    open: Mutex<HashMap<String, Shared<E>>>,
}

impl<E> Default for Registry<E> {
    fn default() -> Self {
        Self {
            open: Mutex::new(HashMap::new()),
        }
    }
}

impl<E> Registry<E> {
    /// Points one more compositor input at `key`'s capture, opening it with
    /// `open` if this is the first item to ask.
    ///
    /// `finish` ends this item's branch, and is handed the capture's size:
    /// what it puts between the capture and the compositor — a rack of
    /// filters — is built for the picture that will arrive, which is known
    /// only once the capture is open.
    ///
    /// The returned id names this item's branch and nothing else, so removing
    /// it later cannot disturb another item sharing the same capture.
    pub(in crate::engine) fn attach(
        &self,
        key: &str,
        open: impl FnOnce() -> Result<Shared<E>, BackendError>,
        finish: impl FnOnce(ChainBuilder, [u32; 2]) -> Result<DetachedBranch, BackendError>,
    ) -> Result<(BranchId, [u32; 2]), BackendError> {
        let mut captures = self.lock();
        if !captures.contains_key(key) {
            captures.insert(key.to_owned(), open()?);
        }
        let capture = captures
            .get_mut(key)
            .expect("the capture was just inserted if it was missing");

        // Every branch is attached at runtime, the first one included: a
        // branch handed to `TeeBuilder` is fixed and has no id, and this one
        // has to be removable when its item goes away.
        let builder = capture
            .tee
            .branch()
            .ok_or("the capture behind this Source has stopped")?;
        let branch = finish(builder, capture.size)?;
        let id = capture.tee.attach(branch)?;
        // A new item is added to the Scene being shown, but the capture may
        // have been paused by another Scene's item leaving it.
        capture.show(id, true);
        Ok((id, capture.size))
    }

    /// Removes one item's branch, and the capture itself once the last branch
    /// is gone.
    pub(in crate::engine) fn detach(&self, key: &str, branch: BranchId) {
        let mut captures = self.lock();
        let Some(capture) = captures.get_mut(key) else {
            return;
        };
        if let Err(error) = capture.tee.detach(branch) {
            tracing::warn!("could not detach a capture branch: {error}");
        }
        // An item removed while shown was never hidden first, and would
        // otherwise keep the capture running for Scenes that are not.
        capture.show(branch, false);
        // Only once nothing draws from it: another SceneItem may still be
        // showing this capture.
        if capture.tee.sink_count() == 0
            && let Some(capture) = captures.remove(key)
        {
            capture.pipeline.stop();
        }
    }

    /// Follows one item into or out of the Scene being shown.
    ///
    /// The capture keeps running while any item shows it, so this only
    /// reaches the pipeline at the transitions to and from none.
    pub(in crate::engine) fn set_showing(&self, key: &str, branch: BranchId, showing: bool) {
        if let Some(capture) = self.lock().get_mut(key) {
            capture.show(branch, showing);
        }
    }

    /// What one item's branch of a capture is doing, for the Stats dock.
    ///
    /// Only that branch's elements: the capture itself is every sharing
    /// item's, and reported per item it would be counted once for each. The
    /// branch ends in the item's own compositor input, which is the element
    /// the dock reads a Source from anyway — whether frames are reaching the
    /// Canvas, and when they stopped.
    pub(in crate::engine) fn stats(
        &self,
        key: &str,
        branch: BranchId,
    ) -> Option<media_pp::stats::PipelineStats> {
        // The pipeline is taken out of the lock before it is read: a reading
        // takes the graph's own lock, and nothing about it needs this one.
        let pipeline = Arc::clone(self.lock().get(key)?.pipeline());
        let mut stats = pipeline.stats();
        stats
            .elements
            .retain(|element| element.branch == Some(branch));
        Some(stats)
    }

    /// Asks one open capture something only its own kind knows how to ask.
    /// `None` where there is no capture under that key.
    pub(in crate::engine) fn with<R>(
        &self,
        key: &str,
        ask: impl FnOnce(&Shared<E>) -> R,
    ) -> Option<R> {
        self.lock().get(key).map(ask)
    }

    /// The same of every open capture of this kind.
    pub(in crate::engine) fn each(&self, mut tell: impl FnMut(&Shared<E>)) {
        for capture in self.lock().values() {
            tell(capture);
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Shared<E>>> {
        self.open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One kind's registry, as a running Source's own share of it needs to see
/// it.
///
/// `RunningSource::Shared` holds one of these and the key it attached under,
/// so pausing, stopping or reading an item's share is the same call whatever
/// it is a share *of*. The kinds differ in one answer: a display is not
/// something that ends by itself, and a camera is.
pub(in crate::engine) trait SharedCapture: Send + Sync {
    /// Lets this item's branch go, closing the capture with the last one.
    fn detach(&self, key: &str, branch: BranchId);
    /// Follows this item into or out of the Scene being shown.
    fn set_showing(&self, key: &str, branch: BranchId, showing: bool);
    /// What this item's branch alone did, for the Stats dock.
    fn stats(&self, key: &str, branch: BranchId) -> Option<media_pp::stats::PipelineStats>;
    /// Whether what is behind `key` has ended on its own — unplugged, or
    /// taken by something else.
    fn ended(&self, key: &str) -> bool;
}
