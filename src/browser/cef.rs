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
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    let code = execute_process(Some(args.as_main_args()), None, ptr::null_mut());
    // Negative means "not a child process"; anything else is this child's
    // whole life, already lived by the call above.
    (code >= 0).then_some(code)
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
pub type OnPaint = Box<dyn Fn(Painted) + Send + Sync>;

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
}

impl Drop for Page {
    fn drop(&mut self) {
        if let Some(commands) = COMMANDS.get() {
            let _ = commands.send(Command::Close(self.id));
        }
    }
}

/// Opens a page, off-screen, at `size` and at most `fps` frames a second.
///
/// The URL is loaded after this returns — a page that does not exist or will
/// not answer is not an error here, it is a page that never paints. What
/// fails here is the engine not running, or CEF refusing to create a browser
/// at all.
pub fn open_page(url: &str, size: [u32; 2], fps: u32, paint: OnPaint) -> Result<Page, String> {
    let commands = COMMANDS
        .get()
        .ok_or("there is no browser engine running in this process")?;
    let id = PageId(NEXT_PAGE.fetch_add(1, Ordering::Relaxed));
    let (reply, answered) = mpsc::channel();
    commands
        .send(Command::Open {
            id,
            url: url.to_owned(),
            size,
            fps,
            paint,
            reply,
        })
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
        url: String,
        size: [u32; 2],
        fps: u32,
        paint: OnPaint,
        reply: mpsc::Sender<Result<(), String>>,
    },
    /// Whether the page is being shown, which decides whether it is drawn
    /// at all — see [`Page::set_shown`].
    Shown(PageId, bool),
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
        paint: Arc<OnPaint>,
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
            (self.paint)(Painted {
                handle: info.shared_texture_handle as isize,
                size: [coded.width.max(0) as u32, coded.height.max(0) as u32],
            });
        }
    }
}

wrap_client! {
    struct PageClient {
        render: RenderHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }
    }
}

/// Runs one command on the runtime thread, which is CEF's own.
fn apply(command: Command, open: &mut HashMap<PageId, Browser>) {
    match command {
        Command::Open {
            id,
            url,
            size,
            fps,
            paint,
            reply,
        } => {
            let mut client = PageClient::new(PageRenderHandler::new(
                size,
                Arc::new(paint),
                Arc::new(AtomicBool::new(false)),
            ));
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
        Command::Close(id) => {
            if let Some(browser) = open.remove(&id) {
                close(&browser);
            }
        }
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

    if initialize(
        Some(args.as_main_args()),
        Some(&settings),
        None,
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
