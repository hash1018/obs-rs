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
    pub(in crate::engine) fn pause(&self) {}
    pub(in crate::engine) fn resume(&self) {}
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
    pub(in crate::engine) fn start_recording(
        &self,
        _path: &std::path::Path,
        _fps: u32,
    ) -> Result<(), BackendError> {
        Err("no compositor backend is written for this platform yet".into())
    }

    pub(in crate::engine) fn available_encoders(&self) -> &[crate::settings::RecordingEncoder] {
        &[]
    }

    pub(in crate::engine) fn stop_recording(&self) -> Result<(), BackendError> {
        Err("no compositor backend is written for this platform yet".into())
    }
}
