//! A Video Capture: one camera, into the compositor.
//!
//! # Not attached is not failure
//!
//! A camera is unplugged, switched off, or picked up by a video call as a
//! matter of course, so a device that will not open right now is an ordinary
//! state rather than an error. Opening one answers `Ok(None)` for it, the
//! engine holds the Source [`SourceState::Missing`], and `retry_missing`
//! looks again — the same standing a closed window has.
//!
//! One that goes away *while* it is running ends its pipeline, which
//! `notice_dropped_streams` turns back into `Missing` for the same reason it
//! does for a stream: neither capture element reconnects, and a pipeline is
//! one-shot, so coming back means building a new one.
//!
//! [`SourceState::Missing`]: crate::engine::SourceState
//!
//! # Shape
//!
//! ```text
//! MfCaptureSource   ─ Queue ─ D3d11Upload ─ compositor input   (Windows)
//! V4l2CaptureSource ─ Queue ─ CudaUpload  ─ compositor input   (Linux)
//! ```
//!
//! The camera hands over NV12 in system memory — Media Foundation's own
//! converter puts it there on Windows, and `V4l2CaptureSource` decodes and
//! converts on Linux, since V4L2 hands over whatever the device speaks — and
//! both uploads take NV12 directly, so nothing converts between them on the
//! CPU. The `Queue` is the thread boundary: the upload is a copy into a
//! staging texture, and doing it on the reader's own thread would make every
//! slow copy a frame the camera dropped.
//!
//! No `Pacer`, unlike a stream. A camera is not replaying a recorded
//! timeline; it delivers when it has a picture, and the compositor draws
//! whatever the layer last received at its own rate.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub(in crate::engine) use linux::open;
#[cfg(target_os = "windows")]
pub(in crate::engine) use windows::open;
