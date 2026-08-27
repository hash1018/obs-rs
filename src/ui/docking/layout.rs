use std::collections::HashMap;

use eframe::egui;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::ui) enum DockPanel {
    Scenes,
    Sources,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum DockRegionId {
    Left,
    Right,
    Bottom,
}

pub(super) const REGIONS: [DockRegionId; 3] = [
    DockRegionId::Left,
    DockRegionId::Right,
    DockRegionId::Bottom,
];

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
}

impl Default for DockLayout {
    fn default() -> Self {
        Self {
            regions: HashMap::from([
                (
                    DockRegionId::Left,
                    DockRegion::new([DockPanel::Scenes, DockPanel::Sources]),
                ),
                (DockRegionId::Right, DockRegion::new([])),
                (DockRegionId::Bottom, DockRegion::new([])),
            ]),
            states: HashMap::from([
                (DockPanel::Scenes, DockState::open()),
                (DockPanel::Sources, DockState::open()),
            ]),
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
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Scenes => "Scenes",
            Self::Sources => "Sources",
        }
    }

    pub(super) fn min_size(self) -> egui::Vec2 {
        match self {
            Self::Scenes | Self::Sources => egui::vec2(180.0, 120.0),
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
        let next_first = (first_weight + delta_fraction).clamp(first_min, pair_total - second_min);

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
            vec![DockPanel::Scenes, DockPanel::Sources]
        );

        layout.move_panel(DockPanel::Scenes, DockRegionId::Left, 1);
        assert_eq!(
            layout.visible_panels(DockRegionId::Left),
            vec![DockPanel::Sources, DockPanel::Scenes]
        );
    }

    #[test]
    fn closed_panels_do_not_take_region_space() {
        let mut layout = DockLayout::default();
        layout.set_open(DockPanel::Scenes, false);

        assert_eq!(
            layout.visible_panels(DockRegionId::Left),
            vec![DockPanel::Sources]
        );
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
}
