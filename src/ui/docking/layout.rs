use std::collections::{HashMap, HashSet};

use eframe::egui;
use serde::{Deserialize, Serialize};

/// One dock.
///
/// Serialised into the settings file by name, so the arrangement survives a
/// restart — and so adding a dock does not invalidate a file written before
/// it existed. See [`WorkspaceDocks`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DockPanel {
    Scenes,
    Sources,
    AudioMixer,
    Controls,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DockRegionId {
    Left,
    Right,
    Bottom,
}

pub(super) const REGIONS: [DockRegionId; 3] = [
    DockRegionId::Left,
    DockRegionId::Right,
    DockRegionId::Bottom,
];

/// How the docks were arranged when the application last closed.
///
/// Written to the settings file and read back at startup. Deliberately
/// forgiving in both directions, because a settings file outlives the version
/// that wrote it: a dock it does not mention keeps the place the default
/// layout gives it, and one it names that no longer exists is skipped. So
/// adding or removing a dock never has to invalidate anybody's saved
/// arrangement.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceDocks {
    pub regions: Vec<RegionPlacement>,
}

/// One region's share of the window, and what is stacked inside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegionPlacement {
    pub region: DockRegionId,
    /// The region's own extent — width for the sides, height for the bottom.
    /// `None` for a region the user never resized, which then opens at its
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    pub panels: Vec<PanelPlacement>,
}

/// One dock's place in the stack it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanelPlacement {
    pub panel: DockPanel,
    /// Its share of the region, as [`DockLayout::normalized_weights`] means
    /// it. Stored normalized, so the numbers in the file read as fractions.
    pub weight: f32,
    pub open: bool,
}

pub(super) struct DockState {
    pub(super) drag_active: bool,
    pub(super) open: bool,
}

pub(super) struct DockRegion {
    panels: Vec<DockPanel>,
    weights: HashMap<DockPanel, f32>,
}

pub(in crate::ui) struct DockLayout {
    regions: HashMap<DockRegionId, DockRegion>,
    states: HashMap<DockPanel, DockState>,
    /// What each region was last dragged to. Kept here rather than left to
    /// egui, whose own panel state lives in a `Context` this application
    /// does not persist — and which would be the wrong place anyway, since
    /// this is the half of the arrangement the user set deliberately.
    region_sizes: HashMap<DockRegionId, f32>,
}

impl Default for DockLayout {
    fn default() -> Self {
        Self {
            regions: HashMap::from([
                (
                    DockRegionId::Left,
                    DockRegion::new([
                        DockPanel::Scenes,
                        DockPanel::Sources,
                        DockPanel::AudioMixer,
                        DockPanel::Controls,
                    ]),
                ),
                (DockRegionId::Right, DockRegion::new([])),
                (DockRegionId::Bottom, DockRegion::new([])),
            ]),
            states: HashMap::from([
                (DockPanel::Scenes, DockState::open()),
                (DockPanel::Sources, DockState::open()),
                (DockPanel::AudioMixer, DockState::open()),
                (DockPanel::Controls, DockState::open()),
            ]),
            region_sizes: HashMap::new(),
        }
    }
}

impl DockState {
    fn open() -> Self {
        Self {
            drag_active: false,
            open: true,
        }
    }
}

impl DockPanel {
    pub(super) fn min_size(self) -> egui::Vec2 {
        match self {
            Self::Scenes | Self::Sources => egui::vec2(180.0, 120.0),
            // A channel plus the name, readout and mute button around it. The
            // channels stand up, so this dock is the one that wants height:
            // what it is given past this becomes meter rather than padding.
            Self::AudioMixer => egui::vec2(200.0, 190.0),
            // Its buttons plus the title bar above them, rather than the
            // list panels' figure: this dock has a height it is complete at,
            // and a splitter should not be able to clip a button in half.
            Self::Controls => egui::vec2(180.0, 110.0),
        }
    }
}

impl DockRegion {
    fn new(panels: impl IntoIterator<Item = DockPanel>) -> Self {
        let panels = panels.into_iter().collect::<Vec<_>>();
        let weights = panels.iter().map(|panel| (*panel, 1.0)).collect();
        Self { panels, weights }
    }

    fn weight(&self, panel: DockPanel) -> f32 {
        self.weights.get(&panel).copied().unwrap_or(1.0)
    }
}

impl DockLayout {
    /// What was arranged, in the shape the settings file keeps it.
    ///
    /// Weights come out normalized, so what is written reads as each dock's
    /// fraction of its region rather than as whatever the drag arithmetic
    /// happened to leave behind. A closed dock keeps its weight, so reopening
    /// it puts it back at the size it had.
    pub(in crate::ui) fn placement(&self) -> WorkspaceDocks {
        let regions = REGIONS
            .into_iter()
            .map(|id| {
                let region = self.region(id);
                let visible = self.visible_panels(id);
                let normalized = self.normalized_weights(id, &visible);
                let panels = region
                    .panels
                    .iter()
                    .map(|panel| PanelPlacement {
                        panel: *panel,
                        weight: visible
                            .iter()
                            .position(|visible| visible == panel)
                            .map_or_else(|| region.weight(*panel), |index| normalized[index]),
                        open: self.is_open(*panel),
                    })
                    .collect();
                RegionPlacement {
                    region: id,
                    size: self.region_sizes.get(&id).copied(),
                    panels,
                }
            })
            .collect();
        WorkspaceDocks { regions }
    }

    /// The arrangement a settings file describes, over the default one.
    ///
    /// Over rather than instead of: whatever the file does not mention keeps
    /// its default place. That is what lets a dock added since the file was
    /// written still appear, in the region it was given, instead of being
    /// silently absent because an older layout had never heard of it.
    pub(in crate::ui) fn restored(saved: &WorkspaceDocks) -> Self {
        let mut layout = Self::default();
        // Everything the file speaks for, worked out before any region is
        // touched. Incrementally would be wrong: a dock the user moved to a
        // later region still has to be taken out of the earlier one the
        // default put it in, and at that point the file has not reached it
        // yet.
        let mentioned: HashSet<DockPanel> = saved
            .regions
            .iter()
            .flat_map(|region| region.panels.iter().map(|placement| placement.panel))
            .collect();
        // First mention wins. A file naming one dock twice, or in two
        // regions, must not put two of it on screen.
        let mut placed = HashSet::new();

        for saved_region in &saved.regions {
            if !layout.regions.contains_key(&saved_region.region) {
                continue;
            }
            let mut panels = Vec::new();
            let mut weights = HashMap::new();
            for placement in &saved_region.panels {
                if !layout.states.contains_key(&placement.panel) || !placed.insert(placement.panel)
                {
                    continue;
                }
                panels.push(placement.panel);
                // The same floor `normalized_weights` applies, so a zero or
                // a negative in a hand-edited file cannot divide by nothing.
                weights.insert(placement.panel, placement.weight.max(0.01));
                layout.state_mut(placement.panel).open = placement.open;
            }
            if let Some(size) = saved_region
                .size
                .filter(|size| size.is_finite() && *size > 0.0)
            {
                layout.region_sizes.insert(saved_region.region, size);
            }
            let region = layout
                .regions
                .get_mut(&saved_region.region)
                .expect("checked above");
            // Kept, not replaced: what the file left out is appended below,
            // in the order the default gave it.
            region.panels.retain(|panel| !mentioned.contains(panel));
            let unmentioned = std::mem::replace(&mut region.panels, panels);
            region.panels.extend(unmentioned);
            region.weights.extend(weights);
        }
        layout
    }

    /// Records what a region was actually drawn at, so closing the
    /// application can write it down.
    pub(super) fn remember_region_size(&mut self, region: DockRegionId, size: f32) {
        if size.is_finite() && size > 0.0 {
            self.region_sizes.insert(region, size);
        }
    }

    /// What this region should open at, or `None` for the built-in default.
    pub(super) fn region_size(&self, region: DockRegionId) -> Option<f32> {
        self.region_sizes.get(&region).copied()
    }

    pub(in crate::ui) fn is_open(&self, panel: DockPanel) -> bool {
        self.state(panel).open
    }

    pub(in crate::ui) fn set_open(&mut self, panel: DockPanel, open: bool) {
        self.state_mut(panel).open = open;
    }

    pub(super) fn state(&self, panel: DockPanel) -> &DockState {
        self.states
            .get(&panel)
            .expect("every dock panel must have state")
    }

    pub(super) fn state_mut(&mut self, panel: DockPanel) -> &mut DockState {
        self.states
            .get_mut(&panel)
            .expect("every dock panel must have state")
    }

    pub(super) fn visible_panels(&self, region: DockRegionId) -> Vec<DockPanel> {
        self.region(region)
            .panels
            .iter()
            .copied()
            .filter(|panel| self.is_open(*panel))
            .collect()
    }

    pub(super) fn normalized_weights(
        &self,
        region: DockRegionId,
        panels: &[DockPanel],
    ) -> Vec<f32> {
        let weights = panels
            .iter()
            .map(|panel| self.region(region).weight(*panel).max(0.01))
            .collect::<Vec<_>>();
        let total = weights.iter().sum::<f32>();
        weights.into_iter().map(|weight| weight / total).collect()
    }

    pub(super) fn resize_pair(
        &mut self,
        region: DockRegionId,
        first: DockPanel,
        second: DockPanel,
        delta_fraction: f32,
        first_min_fraction: f32,
        second_min_fraction: f32,
    ) {
        let panels = self.visible_panels(region);
        let normalized = self.normalized_weights(region, &panels);
        let first_weight = normalized[panels
            .iter()
            .position(|panel| *panel == first)
            .expect("resized panel must be visible")];
        let second_weight = normalized[panels
            .iter()
            .position(|panel| *panel == second)
            .expect("resized panel must be visible")];
        let pair_total = first_weight + second_weight;
        let minimum_total = first_min_fraction + second_min_fraction;
        let minimum_scale = if minimum_total > pair_total {
            pair_total / minimum_total
        } else {
            1.0
        };
        let first_min = first_min_fraction * minimum_scale;
        let second_min = second_min_fraction * minimum_scale;
        // Never below `first_min`, which the subtraction can land just under:
        // when the two minimums fill the pair exactly there is one legal
        // split and both bounds are it, so a single ulp of rounding is enough
        // to put `max` below `min` — and `f32::clamp` panics on that rather
        // than picking either. Reachable by dragging as soon as a region
        // holds four panels, where each starts at a quarter and any two of
        // them come to a half.
        let upper = (pair_total - second_min).max(first_min);
        let next_first = (first_weight + delta_fraction).clamp(first_min, upper);

        let region = self.region_mut(region);
        for (panel, weight) in panels.into_iter().zip(normalized) {
            region.weights.insert(panel, weight);
        }
        region.weights.insert(first, next_first);
        region.weights.insert(second, pair_total - next_first);
    }

    pub(super) fn move_panel(
        &mut self,
        panel: DockPanel,
        target_region: DockRegionId,
        visible_index: usize,
    ) {
        for region in self.regions.values_mut() {
            region.panels.retain(|candidate| *candidate != panel);
        }

        let open_panels = self
            .region(target_region)
            .panels
            .iter()
            .copied()
            .filter(|candidate| self.is_open(*candidate))
            .collect::<Vec<_>>();
        let raw_index = open_panels
            .get(visible_index)
            .and_then(|next| {
                self.region(target_region)
                    .panels
                    .iter()
                    .position(|candidate| candidate == next)
            })
            .unwrap_or_else(|| self.region(target_region).panels.len());

        let region = self.region_mut(target_region);
        region.panels.insert(raw_index, panel);
        region.weights.entry(panel).or_insert(1.0);
    }

    pub(super) fn move_changes_layout(
        &self,
        panel: DockPanel,
        target_region: DockRegionId,
        visible_index: usize,
    ) -> bool {
        let Some((current_region, current_index)) = self.panel_location(panel) else {
            return true;
        };

        current_region != target_region || current_index != visible_index
    }

    pub(super) fn panel_region(&self, panel: DockPanel) -> Option<DockRegionId> {
        self.panel_location(panel).map(|(region, _)| region)
    }

    fn panel_location(&self, panel: DockPanel) -> Option<(DockRegionId, usize)> {
        REGIONS.into_iter().find_map(|region_id| {
            self.visible_panels(region_id)
                .iter()
                .position(|candidate| *candidate == panel)
                .map(|index| (region_id, index))
        })
    }

    fn region(&self, region: DockRegionId) -> &DockRegion {
        self.regions
            .get(&region)
            .expect("every dock region must exist")
    }

    fn region_mut(&mut self, region: DockRegionId) -> &mut DockRegion {
        self.regions
            .get_mut(&region)
            .expect("every dock region must exist")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_can_share_a_region_and_be_reordered() {
        let mut layout = DockLayout::default();
        assert_eq!(
            layout.visible_panels(DockRegionId::Left),
            vec![
                DockPanel::Scenes,
                DockPanel::Sources,
                DockPanel::AudioMixer,
                DockPanel::Controls
            ]
        );

        layout.move_panel(DockPanel::Scenes, DockRegionId::Left, 1);
        assert_eq!(
            layout.visible_panels(DockRegionId::Left),
            vec![
                DockPanel::Sources,
                DockPanel::Scenes,
                DockPanel::AudioMixer,
                DockPanel::Controls
            ]
        );
    }

    #[test]
    fn closed_panels_do_not_take_region_space() {
        let mut layout = DockLayout::default();
        layout.set_open(DockPanel::Scenes, false);

        assert_eq!(
            layout.visible_panels(DockRegionId::Left),
            vec![
                DockPanel::Sources,
                DockPanel::AudioMixer,
                DockPanel::Controls
            ]
        );
    }

    /// Every panel needs state, and `DockLayout::state` panics without it —
    /// so a new `DockPanel` variant that was added to a region but not to
    /// `states` would take down the first frame that drew it.
    #[test]
    fn every_docked_panel_has_state() {
        let layout = DockLayout::default();
        for region in REGIONS {
            for panel in layout.region(region).panels.iter() {
                assert!(
                    layout.states.contains_key(panel),
                    "{panel:?} is docked in {region:?} but has no state"
                );
            }
        }
    }

    #[test]
    fn splitter_respects_each_panels_minimum_fraction() {
        let mut layout = DockLayout::default();
        layout.resize_pair(
            DockRegionId::Left,
            DockPanel::Scenes,
            DockPanel::Sources,
            -1.0,
            0.2,
            0.3,
        );

        let weights =
            layout.normalized_weights(DockRegionId::Left, &[DockPanel::Scenes, DockPanel::Sources]);
        assert!(weights[0] >= 0.2);
        assert!(weights[1] >= 0.3);
    }

    /// Two minimums that exactly fill the pair leave one legal split, so both
    /// clamp bounds are the same number — and a single ulp of rounding is
    /// enough to put the upper one below the lower, which `f32::clamp`
    /// answers with a panic. Four panels in a region is where it starts being
    /// reachable: each holds a quarter, so any two of them come to a half.
    #[test]
    fn a_pair_its_minimums_exactly_fill_can_still_be_dragged() {
        let mut layout = DockLayout::default();

        layout.resize_pair(
            DockRegionId::Left,
            DockPanel::Scenes,
            DockPanel::Sources,
            -1.0,
            0.2,
            0.3,
        );

        let weights =
            layout.normalized_weights(DockRegionId::Left, &[DockPanel::Scenes, DockPanel::Sources]);
        assert!(weights.iter().all(|weight| weight.is_finite()));
    }

    /// A round trip has to come back identical, or closing the application
    /// quietly rearranges it.
    #[test]
    fn an_arrangement_survives_being_written_and_read() {
        let mut layout = DockLayout::default();
        layout.set_open(DockPanel::Controls, false);
        layout.move_panel(DockPanel::AudioMixer, DockRegionId::Bottom, 0);
        layout.remember_region_size(DockRegionId::Left, 240.0);
        layout.remember_region_size(DockRegionId::Bottom, 180.0);

        let restored = DockLayout::restored(&layout.placement());

        assert_eq!(
            restored.visible_panels(DockRegionId::Left),
            layout.visible_panels(DockRegionId::Left)
        );
        assert_eq!(
            restored.visible_panels(DockRegionId::Bottom),
            vec![DockPanel::AudioMixer]
        );
        assert!(!restored.is_open(DockPanel::Controls));
        assert_eq!(restored.region_size(DockRegionId::Left), Some(240.0));
        assert_eq!(restored.region_size(DockRegionId::Bottom), Some(180.0));
    }

    /// A settings file written before a dock existed must not lose it. The
    /// alternative is a dock nobody can reach, from a file nobody knows to
    /// delete.
    #[test]
    fn a_dock_the_saved_layout_never_heard_of_keeps_its_default_place() {
        let full = DockLayout::default().placement();
        let mut older = full.clone();
        for region in &mut older.regions {
            region.panels.retain(|placement| {
                !matches!(placement.panel, DockPanel::AudioMixer | DockPanel::Controls)
            });
        }

        let restored = DockLayout::restored(&older);

        let left = restored.visible_panels(DockRegionId::Left);
        assert!(
            left.contains(&DockPanel::AudioMixer) && left.contains(&DockPanel::Controls),
            "docks the file omits should still be there: {left:?}"
        );
        // Behind the ones it did name, in the order the default gives them.
        assert_eq!(
            left,
            vec![
                DockPanel::Scenes,
                DockPanel::Sources,
                DockPanel::AudioMixer,
                DockPanel::Controls,
            ]
        );
    }

    /// A file naming one dock twice — hand-edited, or written by a version
    /// that let it happen — must not draw two of it.
    #[test]
    fn a_dock_named_twice_appears_once() {
        let mut saved = DockLayout::default().placement();
        let duplicate = saved
            .regions
            .iter()
            .find(|region| region.region == DockRegionId::Left)
            .and_then(|region| region.panels.first().copied())
            .expect("the default layout fills the left region");
        for region in &mut saved.regions {
            if region.region == DockRegionId::Bottom {
                region.panels.push(duplicate);
            }
        }

        let restored = DockLayout::restored(&saved);

        assert_eq!(
            restored.visible_panels(DockRegionId::Left),
            vec![
                DockPanel::Scenes,
                DockPanel::Sources,
                DockPanel::AudioMixer,
                DockPanel::Controls,
            ]
        );
        assert!(restored.visible_panels(DockRegionId::Bottom).is_empty());
    }

    /// Weights are written normalized, and a hand-edited zero must not make
    /// `normalized_weights` divide by nothing.
    #[test]
    fn weights_are_written_as_fractions_and_a_zero_is_floored() {
        let layout = DockLayout::default();
        let saved = layout.placement();
        let left = saved
            .regions
            .iter()
            .find(|region| region.region == DockRegionId::Left)
            .expect("the left region is always written");
        let total: f32 = left.panels.iter().map(|placement| placement.weight).sum();
        assert!((total - 1.0).abs() < 1e-5, "weights summed to {total}");

        let mut zeroed = saved.clone();
        for region in &mut zeroed.regions {
            for placement in &mut region.panels {
                placement.weight = 0.0;
            }
        }
        let restored = DockLayout::restored(&zeroed);
        let panels = restored.visible_panels(DockRegionId::Left);
        for weight in restored.normalized_weights(DockRegionId::Left, &panels) {
            assert!(weight.is_finite() && weight > 0.0, "weight was {weight}");
        }
    }
}
