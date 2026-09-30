//! What CEF asks of a macOS process that the other platforms do not.
//!
//! Three things, each because of how Chromium lives in a Mac application.
//!
//! # The framework is loaded, not linked
//!
//! On Windows and Linux the executable links libcef and cannot start without
//! it. Here CEF is `Chromium Embedded Framework.framework`, which only an
//! application bundle carries — in `Contents/Frameworks`, with the render,
//! GPU and other helper processes as small applications beside it — and it
//! is loaded at run time, from the path the running executable implies
//! ([`load_framework`]). An executable run from outside a bundle, as `cargo
//! run` does unless the bundle runner is set up, finds none, and has no
//! browser engine: the same as a build without the `browser` feature.
//!
//! # The application object has to say it is one CEF can live in
//!
//! Chromium sends events through `NSApp` and asks it, mid-event, whether it
//! is in the middle of sending one (`CrAppControlProtocol`). winit makes
//! `NSApp` an instance of its own class and must: it refuses to run if the
//! application object is anything else. So [`make_application_cef_ready`]
//! gives that object a subclass of winit's class which answers — winit still
//! sees its own class, CEF sees the protocol.
//!
//! # It runs on the main thread
//!
//! AppKit belongs to the main thread and so does CEF on a Mac: it is
//! initialized there, pumped there, and shut down there. The other
//! platforms give it a thread of its own. Here a run-loop timer turns its
//! pump between the window's own events ([`Pump`]), in every run-loop mode,
//! so it keeps turning while a menu is open or the window is being resized.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use cef::library_loader::LibraryLoader;
use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Bool, ClassBuilder, Sel};
use objc2::{MainThreadMarker, msg_send, sel};
use objc2_app_kit::NSApplication;
use objc2_core_foundation::{
    CFAbsoluteTimeGetCurrent, CFRetained, CFRunLoop, CFRunLoopTimer, kCFRunLoopCommonModes,
};

use super::{Browser, Command, PUMP_INTERVAL, PageId};

/// Where the framework is inside a bundle, from an executable's directory.
const FRAMEWORK: &str = "Chromium Embedded Framework.framework/Chromium Embedded Framework";

/// The loaded framework, kept for the life of the process: CEF cannot be
/// loaded twice, and unloading it under running helpers is not a thing to do.
static FRAMEWORK_LOADED: OnceLock<LibraryLoader> = OnceLock::new();

/// Whether this process was started by Chromium as one of its helpers,
/// which it says with `--type=` among the arguments.
pub(super) fn is_helper() -> bool {
    std::env::args().any(|argument| argument.starts_with("--type="))
}

/// Loads the framework from the bundle this executable is in, and says
/// whether there was one to load.
///
/// The main executable is in `Contents/MacOS`, beside `Contents/Frameworks`;
/// a helper is in `Contents/Frameworks/<name>.app/Contents/MacOS`, three
/// levels further in. `LibraryLoader` works the path out the same way but
/// panics when nothing is there, which for the main executable run outside a
/// bundle is an ordinary case, so the file is looked for first.
pub(super) fn load_framework() -> bool {
    if FRAMEWORK_LOADED.get().is_some() {
        return true;
    }
    let helper = is_helper();
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let Some(directory) = executable.parent() else {
        return false;
    };
    let framework: PathBuf = directory
        .join(if helper { "../../.." } else { "../Frameworks" })
        .join(FRAMEWORK);
    if !framework.exists() {
        return false;
    }
    let loader = LibraryLoader::new(&executable, helper);
    if !loader.load() {
        tracing::error!("could not load {}", framework.display());
        return false;
    }
    let _ = FRAMEWORK_LOADED.set(loader);
    true
}

/// Whether [`load_framework`] found one.
pub(super) fn framework_loaded() -> bool {
    FRAMEWORK_LOADED.get().is_some()
}

/// Whether `NSApp` is inside `sendEvent:` right now — what
/// `isHandlingSendEvent` answers. One application object per process, so
/// one flag.
static HANDLING_SEND_EVENT: AtomicBool = AtomicBool::new(false);

/// Makes `NSApp` answer what CEF asks of it — see this module's docs.
///
/// Called once, on the main thread, after winit has made `NSApp` and before
/// CEF is initialized. Answers whether it could; it cannot only if the
/// runtime refuses to make a class, which is worth refusing to start CEF
/// over rather than finding out inside Chromium.
pub(super) fn make_application_cef_ready(main_thread: MainThreadMarker) -> bool {
    let application = NSApplication::sharedApplication(main_thread);
    let object: &AnyObject = &application;
    let original = object.class();
    if original.responds_to(sel!(isHandlingSendEvent)) {
        return true;
    }
    let Some(mut builder) = ClassBuilder::new(c"ObsRsCefApplication", original) else {
        return false;
    };
    // SAFETY: each function has the signature its selector's type encoding
    // states — see `CrAppProtocol` and `CrAppControlProtocol` in CEF, and
    // `-[NSApplication sendEvent:]`.
    unsafe {
        builder.add_method(
            sel!(isHandlingSendEvent),
            is_handling_send_event as extern "C-unwind" fn(_, _) -> _,
        );
        builder.add_method(
            sel!(setHandlingSendEvent:),
            set_handling_send_event as extern "C-unwind" fn(_, _, _),
        );
        builder.add_method(
            sel!(sendEvent:),
            send_event as extern "C-unwind" fn(_, _, _),
        );
    }
    for protocol in [c"CrAppProtocol", c"CrAppControlProtocol", c"CefAppProtocol"] {
        if let Some(protocol) = AnyProtocol::get(protocol) {
            builder.add_protocol(protocol);
        }
    }
    let subclass = builder.register();
    SUPERCLASS.with(|superclass| superclass.set(Some(original)));
    // SAFETY: the subclass adds methods and no instance variables, so an
    // instance of its superclass is a valid instance of it.
    unsafe { AnyObject::set_class(object, subclass) };
    true
}

thread_local! {
    /// winit's class, which `sendEvent:` hands each event on to.
    static SUPERCLASS: std::cell::Cell<Option<&'static AnyClass>> =
        const { std::cell::Cell::new(None) };
}

extern "C-unwind" fn is_handling_send_event(_this: &AnyObject, _selector: Sel) -> Bool {
    Bool::new(HANDLING_SEND_EVENT.load(Ordering::Relaxed))
}

extern "C-unwind" fn set_handling_send_event(_this: &AnyObject, _selector: Sel, handling: Bool) {
    HANDLING_SEND_EVENT.store(handling.as_bool(), Ordering::Relaxed);
}

/// winit's `sendEvent:`, with the flag up for its length and put back after,
/// as CEF's own sample application does.
extern "C-unwind" fn send_event(this: &AnyObject, _selector: Sel, event: *mut AnyObject) {
    let Some(superclass) = SUPERCLASS.with(std::cell::Cell::get) else {
        return;
    };
    let before = HANDLING_SEND_EVENT.swap(true, Ordering::Relaxed);
    // SAFETY: `sendEvent:` of the class this object was, with the event it
    // was handed.
    unsafe {
        let _: () = msg_send![super(this, superclass), sendEvent: event];
    }
    HANDLING_SEND_EVENT.store(before, Ordering::Relaxed);
}

/// CEF's side of the main thread between two of the window's events: the
/// requests for it, the pages open, and the timer that turns it.
pub(super) struct Pump {
    commands: mpsc::Receiver<Command>,
    open: HashMap<PageId, Browser>,
    timer: CFRetained<CFRunLoopTimer>,
}

thread_local! {
    /// The requests sent before CEF was initialized — see
    /// [`super::Runtime::start`] — waiting for [`start`].
    pub(super) static WAITING: RefCell<Option<mpsc::Receiver<Command>>> =
        const { RefCell::new(None) };
    static PUMP: RefCell<Option<Pump>> = const { RefCell::new(None) };
}

/// Initializes CEF on the main thread and starts turning its pump there.
pub(super) fn start() {
    let Some(main_thread) = MainThreadMarker::new() else {
        tracing::error!("the browser engine was started off the main thread");
        return;
    };
    let Some(commands) = WAITING.with(|waiting| waiting.borrow_mut().take()) else {
        return;
    };
    if !make_application_cef_ready(main_thread) {
        tracing::error!("could not make the application object one the browser engine can run in");
        return;
    }
    if !super::initialize_engine() {
        return;
    }
    tracing::info!("browser engine started");
    // SAFETY: a repeating timer with a plain function and no context,
    // which is all `turn` needs — its state is `PUMP`, on this thread.
    let timer = unsafe {
        CFRunLoopTimer::new(
            None,
            CFAbsoluteTimeGetCurrent() + PUMP_INTERVAL.as_secs_f64(),
            PUMP_INTERVAL.as_secs_f64(),
            0,
            0,
            Some(turn),
            std::ptr::null_mut(),
        )
    };
    let (Some(timer), Some(main_loop)) = (timer, CFRunLoop::main()) else {
        tracing::error!("could not schedule the browser engine's work on the main thread");
        return;
    };
    // SAFETY: reading a constant the framework defines.
    main_loop.add_timer(Some(&timer), unsafe { kCFRunLoopCommonModes });
    PUMP.with(|pump| {
        *pump.borrow_mut() = Some(Pump {
            commands,
            open: HashMap::new(),
            timer,
        })
    });
}

/// One turn, from the timer.
///
/// Skipped rather than nested if a turn is already running: CEF can run a
/// run loop of its own inside `do_message_loop_work`, and this timer firing
/// in it would otherwise take the pump a second time.
unsafe extern "C-unwind" fn turn(_timer: *mut CFRunLoopTimer, _info: *mut std::ffi::c_void) {
    PUMP.with(|pump| {
        let Ok(mut pump) = pump.try_borrow_mut() else {
            return;
        };
        if let Some(pump) = pump.as_mut() {
            super::turn(&pump.commands, &mut pump.open);
        }
    });
}

/// Stops the pump and shuts CEF down, on the main thread, if it started.
pub(super) fn stop() {
    let Some(pump) = PUMP.with(|pump| pump.borrow_mut().take()) else {
        return;
    };
    pump.timer.invalidate();
    let Pump { mut open, .. } = pump;
    super::finish(&mut open);
}
