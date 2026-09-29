//! A platform with no compositor backend written yet.
//!
//! `start` refuses, so the application runs with no engine and no Preview:
//! everything above the backend still builds and starts. What a real backend
//! provides is documented in this module's parent; recording and screenshots
//! are `engine::output`'s platform half, which has its own unsupported file.

use std::sync::Arc;

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::elements::{LayerFrame, VideoLayer};
use media_pp::pipeline::Pipeline;

use crate::snapshots::SceneItemSnapshot;

use crate::engine::source::{OpenOutcome, unsupported_kind};

use super::{BackendError, Target};

/// Runtime control for one registered input. Nothing registers any.
pub(in crate::engine) struct Layer;

impl Layer {
    pub(in crate::engine) fn set_layer(&self, _layer: VideoLayer) -> Result<(), BackendError> {
        Ok(())
    }

    pub(in crate::engine) fn set_visible(&self, _visible: bool) -> Result<(), BackendError> {
        Ok(())
    }

    pub(in crate::engine) fn set_opacity(&self, _opacity: f32) -> Result<(), BackendError> {
        Ok(())
    }

    pub(in crate::engine) fn latest_frame(&self) -> Option<LayerFrame> {
        None
    }
}

/// One SceneItem's share of whatever is producing its frames. Nothing does.
pub(in crate::engine) struct RunningSource;

impl RunningSource {
    pub(in crate::engine) fn stats(&self) -> Option<media_pp::stats::PipelineStats> {
        None
    }
    pub(in crate::engine) fn pause(&self) {}
    pub(in crate::engine) fn resume(&self) {}
    pub(in crate::engine) fn ended(&self) -> bool {
        false
    }
    pub(in crate::engine) fn stop(&self) {}
}

/// Never made — [`Backend::start`] refuses — but the engine reads these
/// fields of whichever backend it has, so they are here to be read.
pub(in crate::engine) struct Backend {
    pub(in crate::engine) preview: Arc<Pipeline>,
    pub(in crate::engine) size: [u32; 2],
}

impl Backend {
    pub(in crate::engine) fn start(
        _render_state: &RenderState,
        _size: [u32; 2],
        _fps: u32,
        _preview_fps: u32,
        _on_frame: impl Fn(Option<egui::TextureId>) + Send + Sync + 'static,
        _meter_wake: crate::engine::audio::MeterWake,
    ) -> Result<Self, BackendError> {
        Err("no compositor backend is written for this platform yet".into())
    }

    pub(in crate::engine) fn set_preview_visible(&self, _visible: bool) {}
    pub(in crate::engine) fn stop(&self) {}

    pub(in crate::engine) fn open_source(
        &self,
        item: &SceneItemSnapshot,
        _layer: VideoLayer,
        _fps: u32,
        _mixer: Option<&media_pp::elements::MixerHandle>,
        _into: Target,
    ) -> Result<OpenOutcome, BackendError> {
        Err(unsupported_kind(item))
    }

    pub(in crate::engine) fn remove_source(&self, _name: &str) {}

    /// Nothing composites here, so there is no rate to change.
    pub(in crate::engine) fn set_frame_rate(&self, _fps: u32) -> bool {
        false
    }

    /// The rate a recording would be configured for, if one could start.
    pub(in crate::engine) fn frame_rate(&self) -> u32 {
        crate::engine::TARGET_FPS
    }
}
