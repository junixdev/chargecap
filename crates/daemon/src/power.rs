//! Sleep and wake notifications from IOKit.
//!
//! While the Mac sleeps the control loop does not run, so a battery that
//! charges toward the limit can overshoot it. This module registers with
//! `IORegisterForSystemPower` on a dedicated thread, runs a `CFRunLoop` on
//! that thread, and forwards each event to the control loop as a
//! [`PowerNotice`].
//!
//! Sleep is never vetoed. The callback always calls `IOAllowPowerChange`:
//! at once for `kIOMessageCanSystemSleep`, and for
//! `kIOMessageSystemWillSleep` after the control loop acknowledges, or after
//! [`ACK_TIMEOUT`], whichever comes first.
//!
//! Every `unsafe` block in the daemon lives here.

use std::ffi::c_void;
use std::fmt;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::control::PowerEvent;

/// How long the callback waits for the control loop before it allows sleep.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(5);

/// The system asks whether it may sleep. Idle sleep only.
const K_IO_MESSAGE_CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
/// The system is about to sleep. The last chance to touch the SMC.
const K_IO_MESSAGE_SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;
/// The system has woken and the drivers are back.
const K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON: u32 = 0xe000_0300;

/// A power event on its way to the control loop.
///
/// The acknowledgement is sent when the notice drops, so the callback stops
/// waiting as soon as the handler is done, even if the handler panics.
#[derive(Debug)]
pub struct PowerNotice {
    /// What happened.
    pub event: PowerEvent,
    /// Dropped to acknowledge. `None` for events nobody waits for.
    _ack: Option<Sender<()>>,
}

/// Why power registration failed.
#[derive(Debug)]
pub enum PowerError {
    /// `IORegisterForSystemPower` refused to register.
    Register,
    /// The notification thread could not start.
    Thread(std::io::Error),
    /// The notification thread stopped before it reported back.
    ThreadGone,
}

impl fmt::Display for PowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Register => write!(f, "IORegisterForSystemPower failed"),
            Self::Thread(err) => write!(f, "cannot start the power thread: {err}"),
            Self::ThreadGone => write!(f, "the power thread stopped while starting"),
        }
    }
}

impl std::error::Error for PowerError {}

/// A live registration for sleep and wake notifications.
///
/// [`PowerHooks::stop`] ends the run loop and joins the thread, which
/// deregisters. Dropping the hooks without `stop` leaves the thread running
/// until the process exits, which is safe but not tidy.
pub struct PowerHooks {
    run_loop: RunLoopHandle,
    thread: Option<JoinHandle<()>>,
}

impl PowerHooks {
    /// Stops the run loop and waits for the thread to deregister.
    pub fn stop(mut self) {
        // SAFETY: `run_loop` is a run loop this module retained on the
        // notification thread, and `CFRunLoopStop` may be called from any
        // thread. The thread releases it only after `CFRunLoopRun` returns,
        // which this call causes.
        unsafe { CFRunLoopStop(self.run_loop.0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Registers for system power notifications on a new thread.
///
/// Returns the live registration and the channel the callback writes to. The
/// caller must handle every notice and drop it: dropping the notice releases
/// the sleep acknowledgement.
pub fn spawn() -> Result<(PowerHooks, Receiver<PowerNotice>), PowerError> {
    let (event_tx, event_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("chargecap-power".to_string())
        .spawn(move || run(event_tx, ready_tx))
        .map_err(PowerError::Thread)?;

    match ready_rx.recv() {
        Ok(Ok(run_loop)) => Ok((
            PowerHooks {
                run_loop,
                thread: Some(thread),
            },
            event_rx,
        )),
        Ok(Err(err)) => {
            let _ = thread.join();
            Err(err)
        }
        Err(_) => Err(PowerError::ThreadGone),
    }
}

/// Registers, runs the run loop until it is stopped, then deregisters.
fn run(events: Sender<PowerNotice>, ready: Sender<Result<RunLoopHandle, PowerError>>) {
    let context = Box::into_raw(Box::new(Context {
        events,
        root_port: 0,
    }));
    let mut port: IONotificationPortRef = std::ptr::null_mut();
    let mut notifier: IoObject = 0;

    // SAFETY: `context` is a live leaked box that outlives the registration,
    // `port` and `notifier` are out parameters this call writes, and
    // `on_message` has the callback's C signature.
    let root_port = unsafe {
        IORegisterForSystemPower(
            context.cast::<c_void>(),
            &mut port,
            on_message,
            &mut notifier,
        )
    };
    if root_port == 0 || port.is_null() {
        // SAFETY: the registration failed, so the callback can never run and
        // nothing else holds the box.
        drop(unsafe { Box::from_raw(context) });
        let _ = ready.send(Err(PowerError::Register));
        return;
    }

    // SAFETY: the box is live and, because the run loop below starts only
    // after this write, the callback cannot read the field concurrently.
    unsafe { (*context).root_port = root_port };

    // SAFETY: `port` is the port just registered. The source it owns is
    // added to this thread's run loop, which is retained for the caller's
    // `CFRunLoopStop` and released after `CFRunLoopRun` returns.
    let run_loop = unsafe {
        let source = IONotificationPortGetRunLoopSource(port);
        let run_loop = CFRunLoopGetCurrent();
        CFRetain(run_loop);
        CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
        run_loop
    };

    if ready.send(Ok(RunLoopHandle(run_loop))).is_err() {
        // Nobody is listening, so do not register for events forever.
        // SAFETY: see the cleanup block below; the run loop never ran.
        unsafe { teardown(context, port, &mut notifier, root_port, run_loop) };
        return;
    }

    // SAFETY: this thread owns its run loop. The call returns when the
    // caller's `stop` runs `CFRunLoopStop`.
    unsafe { CFRunLoopRun() };

    // SAFETY: the run loop has stopped, so the callback cannot run again and
    // every handle below is still the one this function created.
    unsafe { teardown(context, port, &mut notifier, root_port, run_loop) };
}

/// Releases everything [`run`] created. Call once, after the run loop stops.
///
/// # Safety
///
/// The callback must not be able to run again, `context` must be a leaked
/// box from [`run`], and the IOKit handles must be the ones it registered.
unsafe fn teardown(
    context: *mut Context,
    port: IONotificationPortRef,
    notifier: &mut IoObject,
    root_port: IoConnect,
    run_loop: CFRunLoopRef,
) {
    IODeregisterForSystemPower(notifier);
    IOServiceClose(root_port);
    IONotificationPortDestroy(port);
    CFRelease(run_loop);
    drop(Box::from_raw(context));
}

/// What the callback needs: where to send events, and the port to answer on.
struct Context {
    events: Sender<PowerNotice>,
    root_port: IoConnect,
}

/// The IOKit callback. Runs on the notification thread's run loop.
extern "C" fn on_message(
    refcon: *mut c_void,
    _service: IoObject,
    message_type: u32,
    message_argument: *mut c_void,
) {
    if refcon.is_null() {
        return;
    }
    // SAFETY: `refcon` is the leaked `Context` box from `run`, which lives
    // until after the run loop stops, and the callback runs only on that
    // run loop's thread, so no other reference exists.
    let context = unsafe { &*refcon.cast::<Context>() };
    let notification = message_argument as isize;

    match message_type {
        K_IO_MESSAGE_CAN_SYSTEM_SLEEP => {
            // Idle sleep. Never veto it.
            // SAFETY: `root_port` is the connection from the registration
            // and `notification` is the id IOKit passed in.
            unsafe { IOAllowPowerChange(context.root_port, notification) };
        }
        K_IO_MESSAGE_SYSTEM_WILL_SLEEP => {
            let (ack_tx, ack_rx) = mpsc::channel();
            let notice = PowerNotice {
                event: PowerEvent::WillSleep,
                _ack: Some(ack_tx),
            };
            if context.events.send(notice).is_ok() {
                // The handler drops the notice, which closes the channel.
                let _ = ack_rx.recv_timeout(ACK_TIMEOUT);
            }
            // WARNING: allow the sleep whatever happened above. A missed
            // acknowledgement must delay sleep, never block it.
            // SAFETY: as above.
            unsafe { IOAllowPowerChange(context.root_port, notification) };
        }
        K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON => {
            let _ = context.events.send(PowerNotice {
                event: PowerEvent::DidWake,
                _ack: None,
            });
        }
        _ => {}
    }
}

/// A retained `CFRunLoopRef` that may cross threads.
struct RunLoopHandle(CFRunLoopRef);

impl fmt::Debug for RunLoopHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RunLoopHandle({:p})", self.0)
    }
}

// SAFETY: the only use of the pointer outside its own thread is
// `CFRunLoopStop`, which Apple documents as thread-safe, and the run loop is
// retained for as long as the handle exists.
unsafe impl Send for RunLoopHandle {}

#[allow(non_camel_case_types)]
type IoObject = u32;
#[allow(non_camel_case_types)]
type IoConnect = u32;
type IONotificationPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFStringRef = *const c_void;
type IOServiceInterestCallback = extern "C" fn(*mut c_void, IoObject, u32, *mut c_void);

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IORegisterForSystemPower(
        refcon: *mut c_void,
        the_port: *mut IONotificationPortRef,
        callback: IOServiceInterestCallback,
        notifier: *mut IoObject,
    ) -> IoConnect;
    fn IODeregisterForSystemPower(notifier: *mut IoObject) -> i32;
    fn IONotificationPortGetRunLoopSource(port: IONotificationPortRef) -> CFRunLoopSourceRef;
    fn IONotificationPortDestroy(port: IONotificationPortRef);
    fn IOAllowPowerChange(root_port: IoConnect, notification_id: isize) -> i32;
    fn IOServiceClose(connect: IoConnect) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(run_loop: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFRunLoopStop(run_loop: CFRunLoopRef);
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    static kCFRunLoopCommonModes: CFStringRef;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The constants must match `IOKit/IOMessage.h`. A wrong value would
    /// make the daemon ignore sleep, so pin them here.
    #[test]
    fn message_types_match_iokit() {
        assert_eq!(K_IO_MESSAGE_CAN_SYSTEM_SLEEP, 0xe0000270);
        assert_eq!(K_IO_MESSAGE_SYSTEM_WILL_SLEEP, 0xe0000280);
        assert_eq!(K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON, 0xe0000300);
    }

    /// The callback waits at most 5 s, so sleep is never blocked for long.
    #[test]
    fn the_ack_timeout_is_five_seconds() {
        assert_eq!(ACK_TIMEOUT, Duration::from_secs(5));
    }

    /// Dropping a notice releases the waiting callback.
    #[test]
    fn dropping_a_notice_acknowledges_it() {
        let (ack_tx, ack_rx) = mpsc::channel();
        let notice = PowerNotice {
            event: PowerEvent::WillSleep,
            _ack: Some(ack_tx),
        };
        assert!(ack_rx.recv_timeout(Duration::from_millis(1)).is_err());
        drop(notice);
        assert!(matches!(
            ack_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }
}
