//! What a build without a browser engine answers — every platform but
//! Windows, and Windows without the `browser` feature.
//!
//! It compiles and does nothing, so nothing above has to be written twice:
//! `main` still asks whether this process is a helper (it never is), and
//! still asks for a runtime (there never is one).

/// Never a helper process: nothing here launches any.
pub fn helper_process() -> Option<i32> {
    None
}

/// The browser engine that is not here.
pub struct Runtime;

impl Runtime {
    /// Always `None`. The caller already treats a missing engine as the
    /// ordinary case — a machine can have the feature compiled in and still
    /// fail to start it — so there is nothing extra to handle here.
    pub fn start() -> Option<Self> {
        tracing::debug!("this build has no browser engine");
        None
    }
}
