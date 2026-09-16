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

/// What a page would hand over, so the callback a Source writes has the same
/// shape wherever it is compiled.
pub struct Painted {
    pub handle: isize,
    pub size: [u32; 2],
}

/// What a page would do with each picture it drew.
pub type OnPaint = Box<dyn Fn(Painted) + Send + Sync>;

/// The page that cannot exist here. Uninhabited, so the `Option<Page>` every
/// open Source carries is always `None` and costs nothing.
pub enum Page {}

impl Page {
    /// Unreachable: there is no page here to show or hide.
    pub fn set_shown(&self, _shown: bool) {
        match *self {}
    }
}

/// Always the reason there is no page, which a Browser Source then shows as
/// why it is dark.
pub fn open_page(_options: PageOptions) -> Result<Page, String> {
    Err("this build has no browser engine".to_owned())
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

/// What a page would hand over when it made a sound.
pub struct Heard<'a> {
    pub planes: &'a [&'a [f32]],
}

/// What a page would do with each block of sound it made.
pub type OnAudio = Box<dyn Fn(Heard<'_>) + Send + Sync>;

/// The shape a page's sound would be taken in — see the Windows
/// implementation, which is what settles these.
pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u16 = 2;

/// What a page would be opened as. Nothing reads these here — there is no
/// page to open — but the caller builds one the same way on every platform.
#[allow(dead_code)]
pub struct PageOptions {
    pub url: String,
    pub size: [u32; 2],
    pub fps: u32,
    pub paint: OnPaint,
    pub audio: Option<OnAudio>,
}
