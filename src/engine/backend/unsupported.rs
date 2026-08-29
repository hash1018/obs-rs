//! A platform with no compositor backend written yet.
//!
//! macOS is what remains: `media-pp` has no ScreenCaptureKit capture or Metal
//! compositor yet, so there is nothing to pair the way Linux pairs PipeWire
//! with CUDA and Windows pairs DXGI with D3D11. See this module's parent for
//! what a backend has to provide.

use eframe::egui;
use eframe::egui_wgpu::RenderState;
use media_pp::elements::VideoLayer;

use crate::snapshots::SceneItemSnapshot;

use super::{BackendError, OpenSource};

/// Runtime control for one registered input. Nothing registers any.
pub(in crate::engine) struct Layer;

impl Layer {
    pub(in crate::engine) fn set_layer(&self, _layer: VideoLayer) -> Result<(), BackendError> {
        Ok(())
    }

    pub(in crate::engine) fn set_visible(&self, _visible: bool) -> Result<(), BackendError> {
        Ok(())
    }
}

/// One SceneItem's share of whatever is producing its frames. Nothing does.
pub(in crate::engine) struct RunningSource;

impl RunningSource {
    pub(in crate::engine) fn pause(&self) {}
    pub(in crate::engine) fn resume(&self) {}
    pub(in crate::engine) fn stop(&self) {}
}

pub(in crate::engine) struct Backend;

impl Backend {
    pub(in crate::engine) fn start(
        _render_state: &RenderState,
        _size: [u32; 2],
        _fps: u32,
        _preview_fps: u32,
        _on_frame: impl Fn(Option<egui::TextureId>) + Send + Sync + 'static,
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
    ) -> Result<OpenSource, BackendError> {
        Err(super::unsupported_kind(item))
    }

    pub(in crate::engine) fn remove_source(&self, _name: &str) {}

    /// Unreachable in practice — `start` refuses, so no `Backend` exists on
    /// this platform to ask. Present because the engine above is written
    /// against one backend interface, not three.
    pub(in crate::engine) fn prepare_recording(
        &self,
        _fps: u32,
        _settings: &crate::settings::RecordingSettings,
    ) -> Result<PreparedRecording, BackendError> {
        Err("no compositor backend is written for this platform yet".into())
    }

    pub(in crate::engine) fn attach_recording(
        &self,
        _prepared: PreparedRecording,
        _sink: Box<dyn media_pp::element::Sink>,
    ) -> Result<super::VideoTrack, BackendError> {
        Err("no compositor backend is written for this platform yet".into())
    }

    pub(in crate::engine) fn detach_recording(
        &self,
        _track: super::VideoTrack,
    ) -> Result<(), BackendError> {
        Err("no compositor backend is written for this platform yet".into())
    }

    /// Nothing composites here, so there is no rate to change.
    pub(in crate::engine) fn set_frame_rate(&self, _fps: u32) -> bool {
        false
    }

    /// The rate a recording would be configured for, if one could start.
    pub(in crate::engine) fn frame_rate(&self) -> u32 {
        crate::engine::TARGET_FPS
    }

    pub(in crate::engine) fn available_encoders(&self) -> &[crate::settings::RecordingEncoder] {
        &[]
    }
}

/// No encoder is ever opened here, so there is nothing to carry — but the
/// engine names this type, so it has to exist.
pub(in crate::engine) enum PreparedRecording {}

impl PreparedRecording {
    pub(in crate::engine) fn parameters(&self) -> media_pp::ffmpeg::codec::Parameters {
        match *self {}
    }

    pub(in crate::engine) fn time_base(&self) -> media_pp::ffmpeg::Rational {
        match *self {}
    }
}
