use serde::{Deserialize, Serialize};

const MIN_WORKSPACE_SCALE: f32 = 0.40;
const MAX_WORKSPACE_SCALE: f32 = 1.00;
const SCALE_STEP: f32 = 0.05;
const DEFAULT_WORKSPACE_SCALE: f32 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreviewScaleMode {
    FitToWorkspace,
    Manual,
}

/// How the Preview was zoomed when the application last closed.
///
/// Both halves, not just the one in effect: switching to Fit and back has
/// always returned to the percentage that was set before it, and a restart
/// should not be what forgets that number.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PreviewZoom {
    pub mode: PreviewScaleMode,
    /// The manual zoom, as a fraction of the Scene Canvas. Kept even while
    /// `mode` is `FitToWorkspace`, which does not use it.
    pub scale: f32,
}

impl Default for PreviewZoom {
    fn default() -> Self {
        Self {
            mode: PreviewScaleMode::Manual,
            scale: DEFAULT_WORKSPACE_SCALE,
        }
    }
}

pub(in crate::ui) struct PreviewViewState {
    mode: PreviewScaleMode,
    workspace_scale: f32,
}

impl PreviewViewState {
    /// How the Preview is zoomed, for the settings file.
    pub(in crate::ui) fn zoom(&self) -> PreviewZoom {
        PreviewZoom {
            mode: self.mode,
            scale: self.workspace_scale,
        }
    }

    /// The zoom a settings file describes, clamped to what the buttons can
    /// reach.
    ///
    /// Clamped rather than trusted: the file is editable, and a scale outside
    /// the range leaves the Preview at a size neither `-` nor `+` can undo —
    /// `can_decrease`/`can_increase` compare against the same bounds, so both
    /// would be dead. A non-finite one is discarded outright.
    pub(in crate::ui) fn restored(saved: &PreviewZoom) -> Self {
        Self {
            mode: saved.mode,
            workspace_scale: if saved.scale.is_finite() {
                saved.scale.clamp(MIN_WORKSPACE_SCALE, MAX_WORKSPACE_SCALE)
            } else {
                DEFAULT_WORKSPACE_SCALE
            },
        }
    }

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

    /// The zoom has to come back as it was left, Fit included — and with the
    /// manual percentage behind it intact, since leaving Fit returns to it.
    #[test]
    fn a_zoom_survives_being_written_and_read() {
        let mut state = PreviewViewState::default();
        state.set_percentage(65.0);
        state.fit_to_workspace();

        let restored = PreviewViewState::restored(&state.zoom());

        assert_eq!(restored.mode(), PreviewScaleMode::FitToWorkspace);
        assert_eq!(restored.percentage(), 100.0);
        // Out of Fit and the number the user had set is still there.
        let mut restored = restored;
        restored.set_percentage(restored.zoom().scale * 100.0);
        assert_eq!(restored.percentage(), 65.0);
    }

    /// A hand-edited scale outside the range would leave both zoom buttons
    /// dead: they compare against the same bounds this clamps to.
    #[test]
    fn a_scale_the_buttons_could_not_undo_is_clamped() {
        for (written, expected) in [(5.0, 40.0), (400.0, 100.0), (f32::NAN, 75.0)] {
            let restored = PreviewViewState::restored(&PreviewZoom {
                mode: PreviewScaleMode::Manual,
                scale: written / 100.0,
            });
            assert_eq!(
                restored.percentage(),
                expected,
                "a saved scale of {written} came back as {}",
                restored.percentage()
            );
            assert!(restored.can_increase() || restored.can_decrease());
        }
    }
}
