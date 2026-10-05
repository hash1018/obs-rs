//! A platform with no compositor backend: nothing composites, so nothing is
//! recorded or captured as a screenshot.
//!
//! Unreachable in practice — `Backend::start` refuses there, so no `Backend`
//! exists to ask. Present because the engine above is written against one
//! backend interface, not one per platform.

use std::sync::Arc;

use media_pp::ffmpeg;

use crate::engine::backend::{Backend, BackendError, VideoTrack};
use crate::settings::RecordingEncoder;

use super::{OutputEncoding, OutputKind};

/// No encoder is ever opened here, so there is nothing to carry — but the
/// engine names this type, so it has to exist.
pub(in crate::engine) enum PreparedOutput {}

impl PreparedOutput {
    pub(in crate::engine) fn parameters(&self) -> ffmpeg::codec::Parameters {
        match *self {}
    }

    pub(in crate::engine) fn time_base(&self) -> ffmpeg::Rational {
        match *self {}
    }
}

const NO_BACKEND: &str = "no compositor backend is written for this platform yet";

impl Backend {
    pub(in crate::engine) fn prepare_output(
        &self,
        _kind: OutputKind,
        _fps: u32,
        _encoding: &OutputEncoding,
    ) -> Result<PreparedOutput, BackendError> {
        Err(NO_BACKEND.into())
    }

    pub(in crate::engine) fn attach_output(
        &self,
        _kind: OutputKind,
        prepared: PreparedOutput,
        _sink: media_pp::element::BoxSink,
    ) -> Result<VideoTrack, BackendError> {
        match prepared {}
    }

    pub(in crate::engine) fn detach_output(&self, _track: VideoTrack) -> Result<(), BackendError> {
        Err(NO_BACKEND.into())
    }

    pub(in crate::engine) fn available_encoders(&self) -> &[RecordingEncoder] {
        &[]
    }

    pub(in crate::engine) fn attach_screenshot(
        &self,
        _sink: media_pp::element::BoxSink,
    ) -> Result<media_pp::graph::BranchId, BackendError> {
        Err(NO_BACKEND.into())
    }

    pub(in crate::engine) fn screenshot_picture(
        &self,
        _frame: Arc<media_pp::pool::UnboundObjectPoolRef<ffmpeg::frame::Video>>,
        _format: crate::engine::source::filters::ChainFormat,
        _sink: media_pp::element::BoxSink,
    ) -> Result<Arc<media_pp::pipeline::Pipeline>, BackendError> {
        Err(NO_BACKEND.into())
    }

    pub(in crate::engine) fn detach_screenshot(
        &self,
        _branch: media_pp::graph::BranchId,
    ) -> Result<(), BackendError> {
        Err(NO_BACKEND.into())
    }
}
