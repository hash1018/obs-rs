const MIN_WORKSPACE_SCALE: f32 = 0.40;
const MAX_WORKSPACE_SCALE: f32 = 1.00;
const SCALE_STEP: f32 = 0.05;
const DEFAULT_WORKSPACE_SCALE: f32 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewScaleMode {
    FitToWorkspace,
    Manual,
}

pub(in crate::ui) struct PreviewViewState {
    mode: PreviewScaleMode,
    workspace_scale: f32,
}

impl PreviewViewState {
    pub(super) fn scale(&self) -> f32 {
        match self.mode {
            PreviewScaleMode::FitToWorkspace => MAX_WORKSPACE_SCALE,
            PreviewScaleMode::Manual => self.workspace_scale,
        }
    }

    pub(super) fn mode(&self) -> PreviewScaleMode {
        self.mode
    }

    pub(super) fn set_percentage(&mut self, percentage: f32) {
        self.workspace_scale = (percentage / 100.0).clamp(MIN_WORKSPACE_SCALE, MAX_WORKSPACE_SCALE);
        self.mode = PreviewScaleMode::Manual;
    }

    pub(super) fn percentage(&self) -> f32 {
        self.scale() * 100.0
    }

    pub(super) fn decrease(&mut self) {
        self.set_percentage((self.scale() - SCALE_STEP) * 100.0);
    }

    pub(super) fn increase(&mut self) {
        self.set_percentage((self.scale() + SCALE_STEP) * 100.0);
    }

    pub(super) fn fit_to_workspace(&mut self) {
        self.mode = PreviewScaleMode::FitToWorkspace;
    }

    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn can_decrease(&self) -> bool {
        self.scale() > MIN_WORKSPACE_SCALE
    }

    pub(super) fn can_increase(&self) -> bool {
        self.scale() < MAX_WORKSPACE_SCALE
    }
}

impl Default for PreviewViewState {
    fn default() -> Self {
        Self {
            mode: PreviewScaleMode::Manual,
            workspace_scale: DEFAULT_WORKSPACE_SCALE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_scale_is_clamped_and_fit_uses_full_workspace() {
        let mut state = PreviewViewState::default();
        assert_eq!(state.percentage(), 75.0);

        state.set_percentage(10.0);
        assert_eq!(state.percentage(), 40.0);

        state.set_percentage(200.0);
        assert_eq!(state.percentage(), 100.0);

        state.set_percentage(65.0);
        state.fit_to_workspace();
        assert_eq!(state.mode(), PreviewScaleMode::FitToWorkspace);
        assert_eq!(state.percentage(), 100.0);

        state.reset();
        assert_eq!(state.mode(), PreviewScaleMode::Manual);
        assert_eq!(state.percentage(), 75.0);
    }
}
