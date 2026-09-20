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
//! Windows shares both kinds. Linux shares cameras — which fail there
//! outright, the second reader refused as busy — and opens a display per
//! SceneItem, for the reasons the CUDA backend's `RunningSource` gives.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use media_pp::{
    elements::TeeHandle,
    graph::BranchId,
    pipeline::{ChainBuilder, DetachedBranch, Pipeline},
};

use crate::engine::backend::BackendError;

/// What [`Registry::attach`] answers when the capture ended before the item
/// could join it — as it opened, most likely: a camera whose stream is bad
/// from its first frame. Its own type so that the kind can tell it apart
/// from a failure, and look again later rather than give up.
#[derive(Debug)]
pub(in crate::engine) struct CaptureEnded;

impl std::fmt::Display for CaptureEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the capture behind this Source has stopped")
    }
}

impl std::error::Error for CaptureEnded {}

/// One item's share of a capture: which capture, and which branch of it.
///
/// The capture is named as well as the branch because a branch id is only
/// unique within its own pipeline. A capture that ends by itself stays
/// registered until every item drawing from it has let go, and a new one
/// can be opened under the same key meanwhile — whose first branch may well
/// have the id an item of the old one still holds. Measured on Linux: a
/// camera whose stream went bad as it opened kept every later item of it on
/// "the capture behind this Source has stopped" until obs-rs was restarted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::engine) struct Share {
    capture: u64,
    branch: BranchId,
}

/// One open capture, and what is currently drawing from it.
pub(in crate::engine) struct Shared<E> {
    /// Unique for the life of the process — see [`Share`].
    id: u64,
    pipeline: Arc<Pipeline>,
    tee: TeeHandle,
    /// Every branch attached and not yet detached.
    ///
    /// Counted here rather than asked of the `Tee`, which answers zero once
    /// the capture has ended — while items drawing from it are still
    /// registered, and have yet to be told.
    branches: HashSet<BranchId>,
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
    ///
    /// Read only by the Windows display — a camera keeps nothing — so a
    /// Linux build, which shares cameras alone, never reads it.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
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
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            pipeline,
            tee,
            branches: HashSet::new(),
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

    /// Whether another item can still join this capture: not once it has
    /// ended, when its `Tee` is gone.
    fn joinable(&self) -> bool {
        self.tee.branch().is_some()
    }
}

/// Every capture of one kind that is open, by whatever names its target —
/// a display name, a camera's device link.
///
/// Several per key at times: the one new items join, last, and before it
/// any that ended by themselves and still have items to be told.
pub(in crate::engine) struct Registry<E> {
    /// Held only for as long as it takes to read or change the map — never
    /// while a capture opens or stops, which is where the time goes.
    open: Mutex<HashMap<String, Vec<Shared<E>>>>,
    /// Held for the whole of an [`attach`](Self::attach), opening included.
    ///
    /// What keeps two attaches from opening one target twice, now that the
    /// map is let go while a capture opens — a camera opened twice is the
    /// failure this module exists for. Only attaching takes it, and only the
    /// source opener attaches, so nothing the engine loop asks waits on it.
    attaching: Mutex<()>,
}

impl<E> Default for Registry<E> {
    fn default() -> Self {
        Self {
            open: Mutex::new(HashMap::new()),
            attaching: Mutex::new(()),
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
    ///
    /// `open` runs with the map let go. It is the slow part — a camera took
    /// 2.3 s here — and the engine loop reads this map once a second for
    /// every item sharing a capture, so holding it across the open stopped
    /// the engine for as long as the device took, which is the one thing
    /// opening on a thread of its own is for. `finish` still runs with the
    /// map held: it builds one item's branch, and a capture its last item let
    /// go of meanwhile would be one it had attached to after it stopped.
    pub(in crate::engine) fn attach(
        &self,
        key: &str,
        open: impl FnOnce() -> Result<Shared<E>, BackendError>,
        finish: impl FnOnce(ChainBuilder, [u32; 2]) -> Result<DetachedBranch, BackendError>,
    ) -> Result<(Share, [u32; 2]), BackendError> {
        let _attaching = self
            .attaching
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut open = Some(open);
        loop {
            // A capture that has ended is left for its own items to leave, and
            // a new one opened beside it.
            let fresh = match open.take_if(|_| !self.joinable(key)) {
                Some(open) => Some(open()?),
                None => None,
            };
            let mut captures = self.lock();
            let versions = captures.entry(key.to_owned()).or_default();
            let opened = fresh.is_some();
            match fresh {
                Some(capture) => versions.push(capture),
                // The one this was to join ended, or was let go by its last
                // item, between the look above and now. Round again to open
                // one — `open` is still unused, since it is only taken on the
                // way to this arm's sibling.
                None if !versions.last().is_some_and(Shared::joinable) => {
                    if versions.is_empty() {
                        captures.remove(key);
                    }
                    continue;
                }
                None => {}
            }
            let capture = versions.last_mut().expect("pushed or looked at above");
            let attached = Self::attach_to(capture, finish);
            // One this just opened and could not attach to has nobody to close
            // it later.
            let mut abandoned = None;
            if attached.is_err() && opened && capture.branches.is_empty() {
                abandoned = versions.pop();
                if versions.is_empty() {
                    captures.remove(key);
                }
            }
            drop(captures);
            // With the map let go, since stopping waits on the capture's own
            // threads.
            if let Some(capture) = abandoned {
                capture.pipeline.stop();
            }
            return attached;
        }
    }

    /// Whether `key` has a capture a new item can join.
    fn joinable(&self, key: &str) -> bool {
        self.lock()
            .get(key)
            .and_then(|versions| versions.last())
            .is_some_and(Shared::joinable)
    }

    fn attach_to(
        capture: &mut Shared<E>,
        finish: impl FnOnce(ChainBuilder, [u32; 2]) -> Result<DetachedBranch, BackendError>,
    ) -> Result<(Share, [u32; 2]), BackendError> {
        // Every branch is attached at runtime, the first one included: a
        // branch handed to `TeeBuilder` is fixed and has no id, and this one
        // has to be removable when its item goes away.
        let builder = capture.tee.branch().ok_or(CaptureEnded)?;
        let branch = finish(builder, capture.size)?;
        let branch = capture
            .tee
            .attach(branch)
            // The capture can end between the two calls — as it opens, most
            // likely — and what the `Tee` says then names an element rather
            // than the capture.
            .map_err(|error| match capture.joinable() {
                true => BackendError::from(error),
                false => BackendError::from(CaptureEnded),
            })?;
        capture.branches.insert(branch);
        // A new item is added to the Scene being shown, but the capture may
        // have been paused by another Scene's item leaving it.
        capture.show(branch, true);
        Ok((
            Share {
                capture: capture.id,
                branch,
            },
            capture.size,
        ))
    }

    /// Removes one item's branch, and its capture once that capture's last
    /// branch is gone.
    pub(in crate::engine) fn detach(&self, key: &str, share: Share) {
        let mut captures = self.lock();
        let Some(versions) = captures.get_mut(key) else {
            return;
        };
        let Some(at) = versions
            .iter()
            .position(|capture| capture.id == share.capture)
        else {
            return;
        };
        let capture = &mut versions[at];
        // An ended capture has no `Tee` left to take the branch from.
        if capture.joinable()
            && let Err(error) = capture.tee.detach(share.branch)
        {
            tracing::warn!("could not detach a capture branch: {error}");
        }
        capture.branches.remove(&share.branch);
        // An item removed while shown was never hidden first, and would
        // otherwise keep the capture running for Scenes that are not.
        capture.show(share.branch, false);
        // Only once nothing draws from it: another SceneItem may still be
        // showing this capture.
        if capture.branches.is_empty() {
            let closed = versions.remove(at);
            if versions.is_empty() {
                captures.remove(key);
            }
            // Stopped with the map let go — see `attach`.
            drop(captures);
            closed.pipeline.stop();
        }
    }

    /// Follows one item into or out of the Scene being shown.
    ///
    /// The capture keeps running while any item shows it, so this only
    /// reaches the pipeline at the transitions to and from none.
    pub(in crate::engine) fn set_showing(&self, key: &str, share: Share, showing: bool) {
        self.with_share(key, share, |capture| capture.show(share.branch, showing));
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
        share: Share,
    ) -> Option<media_pp::stats::PipelineStats> {
        // The pipeline is taken out of the lock before it is read: a reading
        // takes the graph's own lock, and nothing about it needs this one.
        let pipeline = self.with_share(key, share, |capture| Arc::clone(capture.pipeline()))?;
        let mut stats = pipeline.stats();
        stats
            .elements
            .retain(|element| element.branch == Some(share.branch));
        Some(stats)
    }

    /// Asks the capture behind one item's share something only its own kind
    /// knows how to ask. `None` where that capture is no longer registered.
    pub(in crate::engine) fn with_share<R>(
        &self,
        key: &str,
        share: Share,
        ask: impl FnOnce(&mut Shared<E>) -> R,
    ) -> Option<R> {
        self.lock()
            .get_mut(key)?
            .iter_mut()
            .find(|capture| capture.id == share.capture)
            .map(ask)
    }

    /// The capture new items of `key` would join, if there is one.
    ///
    /// What a nested Scene's own items are opened against: the composition
    /// already registered for that Scene — see `source::scene`.
    pub(in crate::engine) fn current<R>(
        &self,
        key: &str,
        ask: impl FnOnce(&Shared<E>) -> R,
    ) -> Option<R> {
        self.lock()
            .get(key)?
            .iter()
            .rev()
            .find(|capture| capture.joinable())
            .map(ask)
    }

    /// The same of every open capture of this kind.
    ///
    /// Asked by the Windows display, for its rate handles; see `extra`.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(in crate::engine) fn each(&self, mut tell: impl FnMut(&Shared<E>)) {
        for capture in self.lock().values().flatten() {
            tell(capture);
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Vec<Shared<E>>>> {
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
    fn detach(&self, key: &str, share: Share);
    /// Follows this item into or out of the Scene being shown.
    fn set_showing(&self, key: &str, share: Share, showing: bool);
    /// What this item's branch alone did, for the Stats dock.
    fn stats(&self, key: &str, share: Share) -> Option<media_pp::stats::PipelineStats>;
    /// Whether the capture behind this share has ended on its own —
    /// unplugged, or taken by something else.
    fn ended(&self, key: &str, share: Share) -> bool;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use media_pp::{
        buffer::MediaBuffer,
        elements::{AppSink, AppSource, AppSourceHandle, TeeBuilder},
    };

    use super::*;
    use crate::engine::backend::pipeline_ended;

    /// A capture that needs no device: pushed buffers into a `Tee`. The
    /// handle is how a test ends it, the way an unplugged camera ends.
    fn a_capture() -> (Shared<()>, AppSourceHandle) {
        let (source, pusher) = AppSource::new("capture", 4);
        let mut handle = None;
        let pipeline = Pipeline::new("capture", source, |source, context| {
            let (tee, tee_handle) = TeeBuilder::new("tee", context.clone()).build_dynamic()?;
            let branch = context.branch().queue("capture", 2).to_branch(tee)?;
            context.attach(source, 0, branch)?;
            handle = Some(tee_handle);
            Ok(())
        })
        .expect("wire the capture");
        pipeline.run().expect("run the capture");
        let tee = handle.expect("the wire closure ran");
        (Shared::new(pipeline, tee, [16, 16], ()), pusher)
    }

    fn a_sink() -> impl FnOnce(ChainBuilder, [u32; 2]) -> Result<DetachedBranch, BackendError> {
        |builder, _| Ok(builder.to(Box::new(AppSink::new("sink", |_: MediaBuffer| Ok(()))))?)
    }

    fn until(what: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if what() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn ended(registry: &Registry<()>, share: Share) -> Option<bool> {
        registry.with_share("key", share, |capture| {
            pipeline_ended(capture.pipeline()) && !capture.joinable()
        })
    }

    /// A capture that ends while items still draw from it is not joined:
    /// the next item opens a new one, the old items are still told theirs
    /// ended, and letting them go leaves the new one alone — even though
    /// its branches are numbered from the same start.
    #[test]
    fn an_item_after_its_capture_ended_opens_a_new_one() {
        let registry = Registry::<()>::default();
        let opened = AtomicUsize::new(0);
        let pushers = Mutex::new(Vec::new());
        let open = || {
            opened.fetch_add(1, Ordering::SeqCst);
            let (capture, pusher) = a_capture();
            pushers.lock().unwrap().push(pusher);
            Ok(capture)
        };

        let (first, _) = registry.attach("key", open, a_sink()).expect("open");
        let (second, _) = registry.attach("key", open, a_sink()).expect("join");
        assert_eq!(opened.load(Ordering::SeqCst), 1, "the second item joins");

        pushers.lock().unwrap()[0]
            .push(MediaBuffer::Eos)
            .expect("end the capture");
        assert!(
            until(|| ended(&registry, first) == Some(true)),
            "the capture ends"
        );

        let (third, _) = registry
            .attach("key", open, a_sink())
            .expect("a new item opens a new capture rather than failing");
        assert_eq!(opened.load(Ordering::SeqCst), 2);
        assert_eq!(ended(&registry, second), Some(true), "still told it ended");
        assert_eq!(ended(&registry, third), Some(false));

        registry.detach("key", first);
        registry.detach("key", second);
        assert_eq!(ended(&registry, first), None, "the ended capture is gone");
        assert_eq!(
            registry.with_share("key", third, |capture| capture.branches.len()),
            Some(1),
            "the new capture keeps its item"
        );
        registry.detach("key", third);
        assert!(registry.lock().is_empty());
    }

    /// Opening a capture takes as long as its device does — seconds, for a
    /// camera — and nothing asked of the registry meanwhile may wait on it.
    /// The engine loop reads every shared item's share once a second, so a
    /// read held up here is the whole engine held up, which is what opening
    /// on a thread of its own exists to prevent.
    #[test]
    fn a_capture_being_opened_holds_up_nothing_else() {
        use std::sync::mpsc;

        let registry = Arc::new(Registry::<()>::default());
        // Kept for the whole test: a capture whose handle is dropped ends,
        // and one that ends before its item joins is refused — rightly, and
        // one run in thirty.
        let (capture, _other_pusher) = a_capture();
        let (other, _) = registry
            .attach("other", || Ok(capture), a_sink())
            .expect("open the other capture");

        let (opening_tx, opening_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let slow = std::thread::spawn({
            let registry = Arc::clone(&registry);
            let (capture, pusher) = a_capture();
            move || {
                registry
                    .attach(
                        "slow",
                        move || {
                            let _ = opening_tx.send(());
                            let _ = release_rx.recv();
                            Ok(capture)
                        },
                        a_sink(),
                    )
                    .map(|(share, _)| (share, pusher))
                    .map_err(|error| error.to_string())
            }
        });
        opening_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the slow open started");

        // From a thread of its own and with a deadline: held up, the read
        // would wait for the open, and the open waits for this test.
        let (answer_tx, answer_rx) = mpsc::channel();
        std::thread::spawn({
            let registry = Arc::clone(&registry);
            move || {
                let answer = registry.with_share("other", other, |capture| capture.branches.len());
                let _ = answer_tx.send(answer);
            }
        });
        let answer = answer_rx.recv_timeout(Duration::from_secs(1));
        release_tx.send(()).expect("release the slow open");
        assert_eq!(
            answer.ok(),
            Some(Some(1)),
            "another capture is read while one is still opening"
        );

        let (slow, _slow_pusher) = slow
            .join()
            .expect("the attaching thread")
            .expect("the slow capture opens once released");
        registry.detach("slow", slow);
        registry.detach("other", other);
        assert!(registry.lock().is_empty());
    }

    /// A capture opened for an item that then could not attach is closed
    /// again, rather than left for every later item to find ended.
    #[test]
    fn a_capture_nobody_attached_to_is_not_kept() {
        let registry = Registry::<()>::default();
        let failed = registry.attach(
            "key",
            || Ok(a_capture().0),
            |_, _| Err(BackendError::from("the rack would not build")),
        );
        assert!(failed.is_err());
        assert!(registry.lock().is_empty());

        let refused = registry.attach("key", || Err("not there".into()), a_sink());
        assert!(refused.is_err());
        assert!(registry.lock().is_empty());
    }
}
