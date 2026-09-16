//! CEF — the Chromium Embedded Framework — as this process hosts it.
//!
//! See [`super`] for why a browser engine needs a module of its own. This is
//! the Windows implementation of the two things it owes the application: the
//! helper-process entry point, and the runtime thread every browser callback
//! arrives on.

use std::{
    collections::HashMap,
    ptr,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

// A glob, which this crate otherwise avoids: `wrap_render_handler!` and
// `wrap_client!` write code naming a dozen of their neighbours — the traits
// they implement, the handler types they wrap, the guards those hold — and
// listing them is listing macro internals that change with the binding
// generator. Everything from it is `Cef`- or CEF-shaped, and the two names
// that could collide (`Settings`, `Rect`) are CEF's own here.
use cef::args::Args;
use cef::*;

use crate::paths;

/// How long the runtime thread waits between turns of CEF's message pump.
///
/// `do_message_loop_work` is documented as something to call periodically
/// rather than spin on, and this is that period: short enough that a page's
/// input, timers and paints are not visibly late, long enough that an idle
/// browser costs nothing measurable. A page's own frame rate is set per
/// browser and is not this.
const PUMP_INTERVAL: Duration = Duration::from_millis(2);

/// How many turns of the pump a closing browser is given before CEF is shut
/// down — a fifth of a second, which is far longer than one takes.
const CLOSE_TURNS: u32 = 100;

/// How long [`Page::open`] waits for the runtime thread to answer.
///
/// Creating a browser is not a network fetch — the page loads afterwards — so
/// this only covers the thread getting to the command and CEF building the
/// object.
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// How long [`Runtime::start`] waits for CEF to report itself initialized.
///
/// Generous on purpose: it unpacks resources and starts child processes the
/// first time, and a machine's virus scanner has an opinion about all of it.
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// Runs this process's job if it is one of CEF's child processes, and hands
/// back the exit code it must then exit with.
///
/// `None` means this is the browser process — the real obs-rs — and startup
/// should carry on.
///
/// Call this first in `main`, before the single-instance lock, the log, or
/// anything else that a second obs-rs must not do. A helper process has this
/// same executable and this same `main`; what tells it apart is the
/// `--type=` arguments Chromium launched it with, which is what
/// `execute_process` reads.
pub fn helper_process() -> Option<i32> {
    // Before any other CEF call, in every process that makes one — it is what
    // binds this build to the version of libcef.dll actually loaded.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = Args::new();
    // The same switches in a child as in the browser process: this is that
    // child's own chance to be told them — see `PageApp`.
    let mut app = PageApp::new();
    let code = execute_process(Some(args.as_main_args()), Some(&mut app), ptr::null_mut());
    // Negative means "not a child process"; anything else is this child's
    // whole life, already lived by the call above.
    (code >= 0).then_some(code)
}

// What this application is, as far as Chromium is concerned. One thing only:
// the switches its processes start with.
//
// A page in a Source has nobody to click it, and Chromium will not let a page
// play sound — or an autoplaying video start — until someone has. That rule
// is for a browser somebody is browsing with; here it would mean a Source
// that is silent until a click that can never happen. OBS turns it off for
// the same reason.
//
// Appended for every process type, including the renderers this is called
// again for as they are launched: the policy is enforced where the page runs.
wrap_app! {
    struct PageApp;

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(command_line) = command_line {
                command_line.append_switch_with_value(
                    Some(&CefString::from("autoplay-policy")),
                    Some(&CefString::from("no-user-gesture-required")),
                );
            }
        }
    }
}

/// The browser engine, running.
///
/// Holds the thread CEF was initialized on and whose message pump it is
/// turned by — every browser callback arrives there, so it is also the thread
/// pages are created and closed on. Dropping this stops that thread and shuts
/// CEF down, which is why `main` keeps it for the whole run: CEF cannot be
/// initialized twice in one process, so this is a once-per-process thing that
/// ends with the process.
pub struct Runtime {
    stop: Arc<AtomicBool>,
    /// `Some` until [`Drop`] takes it to join.
    thread: Option<JoinHandle<()>>,
}

impl Runtime {
    /// Starts the engine, or answers `None` if it could not start.
    ///
    /// `None` is not a reason to refuse to run: a build has a browser engine
    /// the way a machine has a camera, and an application that would not
    /// start without one would be worse than one whose Browser Sources say
    /// they cannot open. Whatever went wrong is logged here.
    ///
    /// Blocks until CEF reports itself initialized, because what comes next
    /// is a window and the user asking for a page.
    pub fn start() -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (commands_tx, commands_rx) = mpsc::channel();
        // A second call would be a second CEF in one process, which CEF does
        // not allow — so the runtime that got here first keeps the channel.
        if COMMANDS.set(commands_tx).is_err() {
            tracing::error!("the browser engine has already been started in this process");
            return None;
        }
        let thread = thread::Builder::new()
            .name("cef".to_owned())
            .spawn({
                let stop = Arc::clone(&stop);
                move || run(&stop, &ready_tx, &commands_rx)
            })
            .inspect_err(|error| tracing::error!("could not start the browser engine: {error}"))
            .ok()?;

        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(true) => {
                tracing::info!("browser engine started");
                Some(Self {
                    stop,
                    thread: Some(thread),
                })
            }
            // It said no, so the thread is already on its way out. Joined
            // here rather than left to a `Runtime` nobody would be given.
            Ok(false) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                None
            }
            // Still initializing. Kept rather than abandoned: the thread may
            // yet succeed, and either way it is CEF's shutdown that has to
            // run on it when this is dropped.
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(
                    "the browser engine has not reported itself started after \
                     {START_TIMEOUT:?}; carrying on without waiting"
                );
                Some(Self {
                    stop,
                    thread: Some(thread),
                })
            }
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("the browser engine's thread panicked");
        }
        tracing::info!("browser engine stopped");
    }
}

/// Runs what one of CEF's callbacks hands out to this application, and
/// turns a panic in it into a line in the log.
///
/// A panic that reaches an `extern "C"` frame is not an unwind, it is an
/// abort: the whole application goes, with nothing to read but a Windows
/// fast-fail code. This is not hypothetical — a copy into an audio plane
/// FFmpeg reported as empty took obs-rs down exactly that way while this
/// was being written, and the next such mistake should cost a page's
/// picture or a block of its sound instead.
///
/// `AssertUnwindSafe` because what these callbacks touch is a channel, an
/// atomic and a device: state that a half-finished call leaves as valid as
/// it found it.
fn guarded(what: &str, body: impl FnOnce()) {
    let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) else {
        return;
    };
    let said = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("no message");
    tracing::error!("a page's {what} was dropped: it panicked ({said})");
}

/// What a page hands over when it has drawn: the shared-texture handle the
/// browser painted into, and the size of it.
///
/// The handle is only valid for the duration of the call — see
/// `media_pp::elements::D3d11SharedTextureSource`, which is what this is
/// meant to be given to.
pub struct Painted {
    pub handle: isize,
    pub size: [u32; 2],
}

/// What a page does with each picture it draws. Called on the runtime
/// thread, so it must not block for long: nothing else is painted, loaded or
/// closed while it runs.
pub type OnPaint = Arc<dyn Fn(Painted) + Send + Sync>;

/// What a page hands over when it has made a sound: one slice of samples per
/// channel, all the same length, at [`AUDIO_RATE`].
///
/// The slices are the browser's own buffer and are borrowed for the call —
/// what takes them has to copy what it keeps.
pub struct Heard<'a> {
    pub planes: &'a [&'a [f32]],
}

/// What a page does with each block of sound it makes. Called on the audio
/// thread CEF runs its own capture on, not the runtime thread.
pub type OnAudio = Arc<dyn Fn(Heard<'_>) + Send + Sync>;

/// The rate a page's sound is taken at, and the channel count with it.
///
/// Asked for rather than discovered: CEF resamples and mixes a page's own
/// output into whatever an application asks its audio handler for, and a
/// Source has to know the shape of its sound when it is built — before the
/// page has played anything at all. 48 kHz stereo is what the mixer works
/// in and what a page will be making anyway.
pub const AUDIO_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u16 = 2;

/// How many frames a page hands over at a time. A tenth of the mixer's own
/// idea of a block, so a page's sound arrives in pieces small enough to be
/// early rather than late.
const AUDIO_FRAMES_PER_BUFFER: i32 = 480;

/// Which keys are held while something is sent to a page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Held {
    /// The bits CEF reads them as.
    fn flags(self) -> u32 {
        const SHIFT: u32 = 2;
        const CONTROL: u32 = 4;
        const ALT: u32 = 8;
        u32::from(self.shift) * SHIFT + u32::from(self.ctrl) * CONTROL + u32::from(self.alt) * ALT
    }
}

/// Which button a page is being pressed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressed {
    Left,
    Middle,
    Right,
}

/// A key a page is told about by name rather than by the character it would
/// type — what an editable field does something with beyond taking text.
///
/// Small on purpose: this is what a widget in a page needs to be usable, not
/// a keyboard driver. Anything that produces a character arrives as
/// [`PageInput::Typed`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Backspace,
    Delete,
    Enter,
    Tab,
    Escape,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

impl NamedKey {
    /// The Windows virtual-key code, which is what CEF's `windows_key_code`
    /// is on every platform — Chromium's own `VKEY_*` values are these.
    fn code(self) -> i32 {
        match self {
            Self::Backspace => 0x08,
            Self::Tab => 0x09,
            Self::Enter => 0x0D,
            Self::Escape => 0x1B,
            Self::PageUp => 0x21,
            Self::PageDown => 0x22,
            Self::End => 0x23,
            Self::Home => 0x24,
            Self::Left => 0x25,
            Self::Up => 0x26,
            Self::Right => 0x27,
            Self::Down => 0x28,
            Self::Delete => 0x2E,
        }
    }
}

/// Something done to a page, in the page's own pixels.
///
/// Positions are where the pointer is *on the page*, so whatever sends these
/// has already undone the layer's placement on the Canvas — a page knows
/// nothing about where it is being shown.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PageInput {
    Moved {
        x: i32,
        y: i32,
        held: Held,
    },
    /// The pointer left the layer, so the page stops hovering whatever it
    /// was: without this a button under where the pointer left stays lit.
    Left,
    Button {
        x: i32,
        y: i32,
        button: Pressed,
        down: bool,
        /// 1 for a click, 2 for the second of a double click — what a page
        /// selects a word with.
        clicks: u32,
        held: Held,
    },
    Wheel {
        x: i32,
        y: i32,
        /// In the same units a wheel notch is: about 120 to a notch.
        delta_x: i32,
        delta_y: i32,
        held: Held,
    },
    Key {
        key: NamedKey,
        down: bool,
        held: Held,
    },
    /// One character, as the keyboard produced it — which is the layout's
    /// answer rather than a key code, so it is the same in every language.
    Typed {
        character: char,
        held: Held,
    },
    /// Whether the page believes it has the keyboard. A page told it has
    /// lost focus stops blinking its caret and closes what it had open.
    Focused(bool),
}

/// An open page, by the id the runtime thread knows it as.
///
/// Dropping it closes the browser. There is nothing else to it from this
/// side — a page is told what to show when it is created, and changing any
/// of that is a new page.
pub struct Page {
    id: PageId,
}

impl Page {
    /// Whether anything is looking at this page.
    ///
    /// A page told it is not shown stops painting — which is the whole
    /// point: a Source whose Scene is not the one being shown has a paused
    /// pipeline, and every picture drawn for it is a texture copied into a
    /// queue nothing is emptying. Its own timers and scripts keep running,
    /// so a clock is right again the moment it comes back rather than
    /// resuming where it stopped.
    ///
    /// Shown again, the page repaints in full, so there is no stale picture
    /// to arrive first.
    pub fn set_shown(&self, shown: bool) {
        if let Some(commands) = COMMANDS.get() {
            let _ = commands.send(Command::Shown(self.id, shown));
        }
    }

    /// Does something to the page — a click, a wheel, a key.
    ///
    /// Queued for the runtime thread rather than done here, because every
    /// CEF call about a browser has to happen on the thread it was created
    /// on. A page that has closed since is a command nobody applies.
    pub fn send(&self, input: PageInput) {
        if let Some(commands) = COMMANDS.get() {
            let _ = commands.send(Command::Input(self.id, input));
        }
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        if let Some(commands) = COMMANDS.get() {
            let _ = commands.send(Command::Close(self.id));
        }
    }
}

/// What a page is opened as.
///
/// Cloneable, and that is why its two callbacks are `Arc`s: a Source that
/// shuts its page down while nothing is looking at it has to be able to open
/// the same page again — the same address, the same size, and the same
/// callbacks, pushing into the pipeline that is still standing.
#[derive(Clone)]
pub struct PageOptions {
    /// Where the page comes from.
    pub url: String,
    /// The size it is told to render at.
    pub size: [u32; 2],
    /// The rate it is redrawn at, at most.
    pub fps: u32,
    pub paint: OnPaint,
    /// What its sound goes to, or `None` to leave the page's audio to
    /// Chromium — which plays it on this machine's own output, where
    /// nothing here can record it.
    pub audio: Option<OnAudio>,
}

/// Opens a page, off-screen, at the size and rate it is given.
///
/// The URL is loaded after this returns — a page that does not exist or will
/// not answer is not an error here, it is a page that never paints. What
/// fails here is the engine not running, or CEF refusing to create a browser
/// at all.
pub fn open_page(options: PageOptions) -> Result<Page, String> {
    let commands = COMMANDS
        .get()
        .ok_or("there is no browser engine running in this process")?;
    let id = PageId(NEXT_PAGE.fetch_add(1, Ordering::Relaxed));
    let (reply, answered) = mpsc::channel();
    commands
        .send(Command::Open { id, options, reply })
        .map_err(|_| "the browser engine has stopped".to_owned())?;
    match answered.recv_timeout(OPEN_TIMEOUT) {
        Ok(Ok(())) => Ok(Page { id }),
        Ok(Err(error)) => Err(error),
        Err(_) => Err("the browser engine did not answer".to_owned()),
    }
}

/// One open page, as both sides name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PageId(u64);

static NEXT_PAGE: AtomicU64 = AtomicU64::new(1);

/// How anything reaches the runtime thread.
///
/// A process-global, because CEF is one: it is initialized once per process
/// and its browsers live on the one thread that did it. Handing a sender
/// down through the application instead would be threading one path to one
/// object that can only ever have one instance.
static COMMANDS: OnceLock<mpsc::Sender<Command>> = OnceLock::new();

enum Command {
    Open {
        id: PageId,
        options: PageOptions,
        reply: mpsc::Sender<Result<(), String>>,
    },
    /// Whether the page is being shown, which decides whether it is drawn
    /// at all — see [`Page::set_shown`].
    Shown(PageId, bool),
    /// Something done to the page — see [`Page::send`].
    Input(PageId, PageInput),
    Close(PageId),
}

// The page's own drawing, as CEF hands it over. `view_rect` is what makes the
// page the size it was asked for; the GPU paint is the picture. The CPU one is
// implemented too, and does nothing but say so once: it is what CEF falls back
// to when the shared-texture path is unavailable, and a Source that is
// silently blank is worth one line in the log.
//
// The macro writes the struct, so it takes neither doc comments nor derives.
wrap_render_handler! {
    struct PageRenderHandler {
        size: [u32; 2],
        paint: OnPaint,
        warned: Arc<AtomicBool>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = self.size[0] as i32;
                rect.height = self.size[1] as i32;
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            _type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            _buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if !self.warned.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "the browser engine is drawing this page on the CPU ({width}x{height}); \
                     obs-rs takes only the GPU path, so it will show nothing"
                );
            }
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            _type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&cef::AcceleratedPaintInfo>,
        ) {
            let Some(info) = info else { return };
            let coded = &info.extra.coded_size;
            guarded("paint", || {
                (self.paint)(Painted {
                    handle: info.shared_texture_handle as isize,
                    size: [coded.width.max(0) as u32, coded.height.max(0) as u32],
                });
            });
        }
    }
}

// The page's own sound. Implementing this at all is what takes a page's audio
// away from Chromium, which would otherwise play it on this machine's output
// where nothing here could record it — so a page whose sound is not wanted is
// given a client with no audio handler rather than a handler that discards.
//
// `audio_parameters` is what makes the format knowable before the page has
// played anything: CEF mixes and resamples the page's own output into what is
// asked for here.
wrap_audio_handler! {
    struct PageAudioHandler {
        heard: OnAudio,
        channels: Arc<AtomicUsize>,
    }

    impl AudioHandler {
        fn audio_parameters(
            &self,
            _browser: Option<&mut Browser>,
            params: Option<&mut AudioParameters>,
        ) -> ::std::os::raw::c_int {
            let Some(params) = params else { return 0 };
            params.channel_layout = ChannelLayout::LAYOUT_STEREO;
            params.sample_rate = AUDIO_RATE as i32;
            params.frames_per_buffer = AUDIO_FRAMES_PER_BUFFER;
            1
        }

        fn on_audio_stream_started(
            &self,
            _browser: Option<&mut Browser>,
            _params: Option<&AudioParameters>,
            channels: ::std::os::raw::c_int,
        ) {
            // What the packets below actually carry, which is the count to
            // read them by — the layout was a request.
            self.channels.store(channels.max(0) as usize, Ordering::Release);
        }

        fn on_audio_stream_packet(
            &self,
            _browser: Option<&mut Browser>,
            data: *mut *const f32,
            frames: ::std::os::raw::c_int,
            _pts: i64,
        ) {
            let channels = self.channels.load(Ordering::Acquire);
            if data.is_null() || frames <= 0 || channels == 0 {
                return;
            }
            // SAFETY: CEF hands over one pointer per channel it reported
            // when the stream started, each to `frames` samples, and both
            // stay live for the length of this call.
            let planes: Vec<&[f32]> = unsafe {
                std::slice::from_raw_parts(data, channels)
                    .iter()
                    .map(|plane| std::slice::from_raw_parts(*plane, frames as usize))
                    .collect()
            };
            guarded("audio", || (self.heard)(Heard { planes: &planes }));
        }

        fn on_audio_stream_error(
            &self,
            _browser: Option<&mut Browser>,
            message: Option<&CefString>,
        ) {
            tracing::warn!(
                "a page's sound could not be taken: {}",
                message.map(CefString::to_string).unwrap_or_default()
            );
        }
    }
}

// What a page is allowed to do about windows, which is nothing.
//
// `window.open`, a link with `target="_blank"`, a script that decides it
// wants a login box: each of those asks for a second browser, and CEF makes
// that one a real window on the screen — over whatever is being recorded,
// with no way to get it back off. A Source is a page drawn into a layer, so
// the answer is no, and the log says which address asked.
//
// The second line of defence rather than the first: Chromium's own popup
// blocker swallows a `window.open` that no user asked for before it ever
// reaches here, which is why this is quiet in practice. Checked by turning
// that blocker off for a run — the refusal then arrives here, once for the
// page that kept asking, and still no window appears.
wrap_life_span_handler! {
    struct PageLifeSpanHandler {
        complained: Arc<AtomicBool>,
    }

    impl LifeSpanHandler {
        #[allow(clippy::too_many_arguments)]
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            // Once per page: one that asks in a loop would otherwise write
            // the log full of the same line.
            if !self.complained.swap(true, Ordering::Relaxed) {
                tracing::info!(
                    "a page asked to open {} in a window of its own; refused",
                    target_url.map(CefString::to_string).unwrap_or_default()
                );
            }
            // Cancelled.
            1
        }
    }
}

// `audio` is `None` for a page whose sound is left to Chromium — see
// `PageOptions::audio`. As above, the macro takes no doc comments.
wrap_client! {
    struct PageClient {
        render: RenderHandler,
        audio: Option<AudioHandler>,
        life_span: LifeSpanHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn audio_handler(&self) -> Option<AudioHandler> {
            self.audio.clone()
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }
    }
}

/// Runs one command on the runtime thread, which is CEF's own.
fn apply(command: Command, open: &mut HashMap<PageId, Browser>) {
    match command {
        Command::Open { id, options, reply } => {
            let PageOptions {
                url,
                size,
                fps,
                paint,
                audio,
            } = options;
            let mut client = PageClient::new(
                PageRenderHandler::new(size, paint, Arc::new(AtomicBool::new(false))),
                audio.map(|heard| PageAudioHandler::new(heard, Arc::new(AtomicUsize::new(0)))),
                PageLifeSpanHandler::new(Arc::new(AtomicBool::new(false))),
            );
            let window = WindowInfo {
                windowless_rendering_enabled: 1,
                // The whole reason a page can be a Source at all: its
                // pictures arrive as textures this machine's GPU already
                // holds, rather than as pixels copied out to system memory
                // and back.
                shared_texture_enabled: 1,
                ..Default::default()
            };
            let settings = BrowserSettings {
                windowless_frame_rate: fps as i32,
                // Transparent where the page has not drawn, so an overlay is
                // an overlay rather than a black rectangle with one on it.
                background_color: 0,
                ..Default::default()
            };
            let browser = browser_host_create_browser_sync(
                Some(&window),
                Some(&mut client),
                Some(&CefString::from(url.as_str())),
                Some(&settings),
                None,
                None,
            );
            match browser {
                Some(browser) => {
                    open.insert(id, browser);
                    let _ = reply.send(Ok(()));
                }
                None => {
                    let _ =
                        reply.send(Err("the browser engine could not create a page".to_owned()));
                }
            }
        }
        Command::Shown(id, shown) => {
            if let Some(browser) = open.get(&id)
                && let Some(host) = browser.host()
            {
                host.was_hidden(i32::from(!shown));
            }
        }
        Command::Input(id, input) => {
            if let Some(browser) = open.get(&id)
                && let Some(host) = browser.host()
            {
                send(&host, input);
            }
        }
        Command::Close(id) => {
            if let Some(browser) = open.remove(&id) {
                close(&browser);
            }
        }
    }
}

/// Hands one piece of input to the page behind `host`.
///
/// Every one of these is where the page believes the pointer is, so a click
/// is sent as the position as well as the button: CEF keeps no cursor of its
/// own for an off-screen browser.
fn send(host: &BrowserHost, input: PageInput) {
    let at = |x: i32, y: i32, held: Held| MouseEvent {
        x,
        y,
        modifiers: held.flags(),
    };
    match input {
        PageInput::Moved { x, y, held } => {
            host.send_mouse_move_event(Some(&at(x, y, held)), 0);
        }
        PageInput::Left => {
            // The position goes with it and is not read: `mouse_leave` is
            // what the page acts on.
            host.send_mouse_move_event(Some(&at(0, 0, Held::default())), 1);
        }
        PageInput::Button {
            x,
            y,
            button,
            down,
            clicks,
            held,
        } => {
            let button = match button {
                Pressed::Left => MouseButtonType::LEFT,
                Pressed::Middle => MouseButtonType::MIDDLE,
                Pressed::Right => MouseButtonType::RIGHT,
            };
            host.send_mouse_click_event(
                Some(&at(x, y, held)),
                button,
                i32::from(!down),
                clicks.clamp(1, 3) as i32,
            );
        }
        PageInput::Wheel {
            x,
            y,
            delta_x,
            delta_y,
            held,
        } => {
            host.send_mouse_wheel_event(Some(&at(x, y, held)), delta_x, delta_y);
        }
        PageInput::Key { key, down, held } => {
            // Two events for a press, as a keyboard sends: the raw one a
            // page's `keydown` listener sees, then the one that acts on an
            // editable field. A release is the one event.
            let event = KeyEvent {
                type_: match down {
                    true => KeyEventType::RAWKEYDOWN,
                    false => KeyEventType::KEYUP,
                },
                modifiers: held.flags(),
                windows_key_code: key.code(),
                native_key_code: 0,
                is_system_key: 0,
                character: 0,
                unmodified_character: 0,
                focus_on_editable_field: 0,
                ..Default::default()
            };
            host.send_key_event(Some(&event));
            if down {
                host.send_key_event(Some(&KeyEvent {
                    type_: KeyEventType::KEYDOWN,
                    ..event
                }));
            }
        }
        PageInput::Typed { character, held } => {
            // What the layout produced, not what was struck: a page taking
            // text wants the character, and this is the one event that
            // carries it.
            let mut units = [0u16; 2];
            let Some(unit) = character.encode_utf16(&mut units).first().copied() else {
                return;
            };
            host.send_key_event(Some(&KeyEvent {
                type_: KeyEventType::CHAR,
                modifiers: held.flags(),
                windows_key_code: unit as i32,
                native_key_code: 0,
                is_system_key: 0,
                character: unit,
                unmodified_character: unit,
                focus_on_editable_field: 0,
                ..Default::default()
            }));
        }
        PageInput::Focused(focused) => host.set_focus(i32::from(focused)),
    }
}

/// Asks one browser to close, forcing it: a page that would ask "are you
/// sure" is a page nobody can answer, since it has no window.
fn close(browser: &Browser) {
    if let Some(host) = browser.host() {
        host.close_browser(1);
    }
}

/// The runtime thread: initialize, pump until asked to stop, shut down.
///
/// All three have to happen on this one thread — CEF's is the thread that
/// initialized it — which is the whole reason this is a thread and not a call.
fn run(stop: &AtomicBool, ready: &mpsc::Sender<bool>, commands: &mpsc::Receiver<Command>) {
    // Called again here rather than relying on `helper_process`: this is the
    // first CEF call on this thread's own path through the library, and it is
    // cheap and idempotent.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = Args::new();
    let cache = paths::browser_cache_dir();
    if let Err(error) = std::fs::create_dir_all(&cache) {
        tracing::warn!("could not create the browser cache directory: {error}");
    }
    let settings = Settings {
        // Off-screen rendering: a page is drawn into a texture this
        // application composites, never into a window of its own.
        windowless_rendering_enabled: 1,
        // The Chromium sandbox needs the browser process to hand each child a
        // token it can only prepare at its own `main`, before anything else
        // has run. obs-rs has its own first things to do there, and what the
        // sandbox protects against — a page's own renderer — is a risk this
        // takes deliberately for a page the user chose to show.
        no_sandbox: 1,
        // Its own directory under this user's data, so a page's cookies and
        // cache live where the rest of this application's state does, and a
        // second profile does not appear beside the executable.
        root_cache_path: CefString::from(cache.to_string_lossy().as_ref()),
        // Beside the two logs this application already writes — see `log`.
        log_file: CefString::from(
            paths::logs_dir()
                .join("browser.log")
                .to_string_lossy()
                .as_ref(),
        ),
        // A page's own console noise is not this application's business;
        // what is, is the engine failing.
        log_severity: LogSeverity::WARNING,
        ..Default::default()
    };

    let mut app = PageApp::new();
    if initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        ptr::null_mut(),
    ) != 1
    {
        tracing::error!("the browser engine refused to initialize");
        let _ = ready.send(false);
        return;
    }
    let _ = ready.send(true);

    let mut open: HashMap<PageId, Browser> = HashMap::new();
    while !stop.load(Ordering::Acquire) {
        // Before the pump rather than after: a page asked for on the last
        // turn is created on this one, and a closed browser gets this turn's
        // pump to finish closing in.
        for command in commands.try_iter() {
            apply(command, &mut open);
        }
        do_message_loop_work();
        thread::sleep(PUMP_INTERVAL);
    }

    // Every page closed before CEF is shut down, and pumped afterwards: a
    // browser is not gone when `close_browser` returns, it is gone a few
    // turns of the message loop later, and shutting down with one still open
    // is how a browser process is left behind.
    for (_, browser) in open.drain() {
        close(&browser);
    }
    for _ in 0..CLOSE_TURNS {
        do_message_loop_work();
        thread::sleep(PUMP_INTERVAL);
    }

    shutdown();
}
