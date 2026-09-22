//! A placement coming up when it is shown, and going when it is hidden.
//!
//! The project stores only whether an item is visible; how long the change
//! takes is presentation, and lives here. A fade is the layer's opacity
//! moved a tick at a time — the same thing a Scene transition does to a
//! whole Scene, and on the same clock (see `TRANSITION_TICK`) — so it costs
//! the compositor nothing it was not already doing.
//!
//! # What counts as a change
//!
//! Only an item whose visibility is different from the last pass that drew
//! it. One seen for the first time — a Scene being switched to, a project
//! being opened, a nested Scene appearing — is drawn as it is, because it did
//! not change: it arrived, and arriving is the Scene transition's business.
//! That is also why an item toggled from a key while its Scene was not being
//! shown does not start fading the moment somebody switches to it.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::domain::SceneItemId;
use crate::snapshots::SceneItemSnapshot;

/// One fade in progress.
#[derive(Debug, Clone, Copy)]
struct Fade {
    /// Where it started, from nothing to one — which is not always an end:
    /// an item hidden half way through coming up starts going from half.
    from: f32,
    to: f32,
    started: Instant,
    duration: Duration,
}

impl Fade {
    fn at(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started).as_secs_f32();
        let duration = self.duration.as_secs_f32();
        let t = if duration > 0.0 {
            (elapsed / duration).clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.from + (self.to - self.from) * t
    }

    fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= self.duration
    }
}

/// How one item is to be drawn this pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Shown {
    /// Whether its layer is drawn at all. True while an item that has been
    /// hidden is still going.
    pub(super) visible: bool,
    /// What its own opacity is multiplied by, from nothing to one.
    pub(super) factor: f32,
}

/// Every item that is coming up or going.
#[derive(Default)]
pub(super) struct ItemFades {
    running: HashMap<SceneItemId, Fade>,
    /// Each item's visibility as the last pass that drew it saw it.
    last: HashMap<SceneItemId, bool>,
    /// The items the last pass drew, and those this one has so far.
    previous: HashSet<SceneItemId>,
    current: HashSet<SceneItemId>,
}

impl ItemFades {
    /// Starts one pass over everything being drawn.
    pub(super) fn begin_pass(&mut self) {
        self.current.clear();
    }

    /// Ends it. What this pass did not draw is forgotten — see this module's
    /// docs on why an item arriving again is not a change.
    pub(super) fn end_pass(&mut self) {
        std::mem::swap(&mut self.previous, &mut self.current);
        let drawn = &self.previous;
        self.last.retain(|id, _| drawn.contains(id));
        self.running.retain(|id, _| drawn.contains(id));
    }

    /// How `item` is drawn at `now`, starting a fade where its visibility has
    /// just changed and it asks for one.
    pub(super) fn observe(&mut self, item: &SceneItemSnapshot, now: Instant) -> Shown {
        let id = item.id;
        let seen = self.previous.contains(&id) || self.current.contains(&id);
        self.current.insert(id);
        let before = self.last.insert(id, item.visible);
        if seen && before.is_some_and(|was| was != item.visible) {
            let ms = if item.visible {
                item.fades.show_ms
            } else {
                item.fades.hide_ms
            };
            // From wherever it is now: reversed half way, it goes back from
            // half rather than jumping to the other end first.
            let from = self
                .running
                .get(&id)
                .map_or(if item.visible { 0.0 } else { 1.0 }, |fade| fade.at(now));
            if ms == 0 {
                self.running.remove(&id);
            } else {
                self.running.insert(
                    id,
                    Fade {
                        from,
                        to: if item.visible { 1.0 } else { 0.0 },
                        started: now,
                        duration: Duration::from_millis(u64::from(ms)),
                    },
                );
            }
        }
        match self.running.get(&id) {
            Some(fade) if !fade.done(now) => Shown {
                visible: true,
                factor: fade.at(now),
            },
            Some(_) => {
                self.running.remove(&id);
                Shown {
                    visible: item.visible,
                    factor: 1.0,
                }
            }
            None => Shown {
                visible: item.visible,
                factor: 1.0,
            },
        }
    }

    /// Whether `item` is being drawn, fading or not — what a page is told,
    /// and what keeps a clip's sound on until its picture has gone.
    pub(super) fn drawn(&self, item: &SceneItemSnapshot) -> bool {
        item.visible || self.running.contains_key(&item.id)
    }

    /// Whether anything is coming up or going, which is what keeps the
    /// engine drawing a tick at a time rather than waiting.
    pub(super) fn active(&self) -> bool {
        !self.running.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::VisibilityFades;

    fn item(visible: bool, show_ms: u32, hide_ms: u32) -> SceneItemSnapshot {
        use crate::domain::{ColorSourceSettings, Crop, SourceKind, SourceSettings, Transform};

        SceneItemSnapshot {
            filters: Vec::new(),
            audio_filters: Vec::new(),
            id: SceneItemId(1),
            name: "Overlay".to_owned(),
            kind: SourceKind::Color,
            settings: SourceSettings::Color(ColorSourceSettings {
                size: [100.0, 100.0],
                rgba: [0, 0, 0, 255],
            }),
            source_size: [100.0, 100.0],
            visible,
            locked: false,
            transform: Transform::default(),
            crop: Crop::default(),
            opacity: 1.0,
            fades: VisibilityFades { show_ms, hide_ms },
            peak_db: None,
            position: None,
        }
    }

    /// One pass, as reconcile makes it.
    fn pass(fades: &mut ItemFades, item: &SceneItemSnapshot, now: Instant) -> Shown {
        fades.begin_pass();
        let shown = fades.observe(item, now);
        fades.end_pass();
        shown
    }

    #[test]
    fn showing_comes_up_over_its_own_time_and_then_stops() {
        let mut fades = ItemFades::default();
        let start = Instant::now();
        pass(&mut fades, &item(false, 400, 0), start);

        let shown = pass(&mut fades, &item(true, 400, 0), start);
        assert!(shown.visible);
        assert_eq!(shown.factor, 0.0, "it starts from nothing");
        let half = pass(
            &mut fades,
            &item(true, 400, 0),
            start + Duration::from_millis(200),
        );
        assert!((half.factor - 0.5).abs() < 1e-3);
        assert!(fades.active());

        let done = pass(
            &mut fades,
            &item(true, 400, 0),
            start + Duration::from_millis(400),
        );
        assert_eq!(
            done,
            Shown {
                visible: true,
                factor: 1.0
            }
        );
        assert!(
            !fades.active(),
            "and once it is up, nothing is left running"
        );
    }

    /// Hiding keeps the layer drawn until it has gone, and then hides it.
    #[test]
    fn hiding_is_drawn_until_it_has_gone() {
        let mut fades = ItemFades::default();
        let start = Instant::now();
        pass(&mut fades, &item(true, 0, 300), start);

        let going = pass(
            &mut fades,
            &item(false, 0, 300),
            start + Duration::from_millis(150),
        );
        // Started this pass, so it is at its beginning here.
        assert_eq!(
            going,
            Shown {
                visible: true,
                factor: 1.0
            }
        );
        let item_now = item(false, 0, 300);
        assert!(fades.drawn(&item_now), "still drawn while it goes");

        let gone = pass(&mut fades, &item_now, start + Duration::from_millis(500));
        assert!(!gone.visible, "and hidden once it has gone");
        assert!(!fades.drawn(&item_now));
    }

    /// Zero is at once, which is what every item did before there was a
    /// choice.
    #[test]
    fn no_fade_is_at_once() {
        let mut fades = ItemFades::default();
        let start = Instant::now();
        pass(&mut fades, &item(true, 0, 0), start);
        let hidden = pass(&mut fades, &item(false, 0, 0), start);
        assert!(!hidden.visible);
        assert!(!fades.active());
    }

    /// Reversed half way, it goes back from where it was rather than
    /// jumping to the other end first.
    #[test]
    fn a_fade_reversed_half_way_goes_back_from_where_it_was() {
        let mut fades = ItemFades::default();
        let start = Instant::now();
        pass(&mut fades, &item(false, 400, 400), start);
        pass(&mut fades, &item(true, 400, 400), start);
        let reversed = pass(
            &mut fades,
            &item(false, 400, 400),
            start + Duration::from_millis(200),
        );
        assert!(reversed.visible);
        assert!((reversed.factor - 0.5).abs() < 1e-3, "{}", reversed.factor);
    }

    /// An item seen for the first time arrived rather than changed: it is
    /// drawn as it is, with no fade, and so is one that comes back after a
    /// pass that did not draw it.
    #[test]
    fn arriving_is_not_a_change() {
        let mut fades = ItemFades::default();
        let start = Instant::now();
        let first = pass(&mut fades, &item(true, 400, 400), start);
        assert_eq!(
            first,
            Shown {
                visible: true,
                factor: 1.0
            }
        );

        // A pass that drew something else — its Scene was switched away from.
        fades.begin_pass();
        fades.end_pass();
        let back = pass(&mut fades, &item(false, 400, 400), start);
        assert!(
            !back.visible,
            "hidden while away, and drawn hidden on return"
        );
        assert!(!fades.active());
    }
}
