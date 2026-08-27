//! A platform with no compositor backend written yet.
//!
//! Windows is the one that matters: `media-pp` has `DxgiCaptureSource` and
//! `D3d11VideoCompositor` waiting for it, and those pair with each other —
//! DXGI capture produces D3D11 textures, which a CUDA compositor cannot
//! accept at all. See this module's parent for what a backend has to provide.

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

pub(in crate::engine) struct Backend;

impl Backend {
    pub(in crate::engine) fn start(
        _render_state: &RenderState,
        _size: [u32; 2],
        _fps: u32,
        _on_frame: impl Fn(Option<egui::TextureId>) + Send + Sync + 'static,
    ) -> Result<Self, BackendError> {
        Err("no compositor backend is written for this platform yet".into())
    }

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
}
