//! CEF — the Chromium Embedded Framework — as this process hosts it.
//!
//! See [`super`] for why a browser engine needs a module of its own. This is
//! the Windows implementation of the two things it owes the application: the
//! helper-process entry point, and the runtime thread every browser callback
//! arrives on.

use std::{
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use cef::{
    CefString, LogSeverity, Settings, api_hash, args::Args, do_message_loop_work, execute_process,
    initialize, shutdown, sys,
};

use crate::paths;

/// How long the runtime thread waits between turns of CEF's message pump.
///
/// `do_message_loop_work` is documented as something to call periodically
/// rather than spin on, and this is that period: short enough that a page's
/// input, timers and paints are not visibly late, long enough that an idle
/// browser costs nothing measurable. A page's own frame rate is set per
/// browser and is not this.
const PUMP_INTERVAL: Duration = Duration::from_millis(2);

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
        let thread = thread::Builder::new()
            .name("cef".to_owned())
            .spawn({
                let stop = Arc::clone(&stop);
                move || run(&stop, &ready_tx)
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

/// The runtime thread: initialize, pump until asked to stop, shut down.
///
/// All three have to happen on this one thread — CEF's is the thread that
/// initialized it — which is the whole reason this is a thread and not a call.
fn run(stop: &AtomicBool, ready: &mpsc::Sender<bool>) {
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

    while !stop.load(Ordering::Acquire) {
        do_message_loop_work();
        thread::sleep(PUMP_INTERVAL);
    }

    shutdown();
}
