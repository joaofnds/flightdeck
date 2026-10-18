use core_foundation::base::CFRelease;
use core_foundation::date::CFAbsoluteTimeGetCurrent;
use core_foundation::runloop::{
    CFRunLoopAddTimer, CFRunLoopGetMain, CFRunLoopTimerContext, CFRunLoopTimerCreate,
    CFRunLoopTimerInvalidate, CFRunLoopTimerRef, kCFRunLoopCommonModes,
};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr;
use std::rc::Rc;
use std::time::Duration;

pub struct Timer {
    reference: CFRunLoopTimerRef,
    interval: Duration,
    running: Cell<bool>,
    _callback: Rc<RefCell<Box<dyn FnMut()>>>,
}

impl Timer {
    pub fn start(callback: impl FnMut() + 'static) -> Result<Self, String> {
        Self::with_interval(Duration::from_millis(250), callback)
    }

    pub fn with_interval(
        interval: Duration,
        callback: impl FnMut() + 'static,
    ) -> Result<Self, String> {
        assert!(!interval.is_zero());
        // The callback and its owner are confined to the main run loop by Rc.
        if unsafe { libc::pthread_main_np() } != 1 {
            return Err("The live timer must start on the main thread".into());
        }
        let callback: Rc<RefCell<Box<dyn FnMut()>>> = Rc::new(RefCell::new(Box::new(callback)));
        let mut context = CFRunLoopTimerContext {
            version: 0,
            info: Rc::as_ptr(&callback).cast_mut().cast(),
            retain: None,
            release: None,
            copyDescription: None,
        };

        // The owner retains the callback until after the timer is invalidated and released.
        let timer = unsafe {
            CFRunLoopTimerCreate(
                ptr::null(),
                CFAbsoluteTimeGetCurrent() + interval.as_secs_f64(),
                interval.as_secs_f64(),
                0,
                0,
                start_callback,
                &raw mut context,
            )
        };

        if timer.is_null() {
            return Err("CoreFoundation could not create the live timer".into());
        }
        unsafe { CFRunLoopAddTimer(CFRunLoopGetMain(), timer, kCFRunLoopCommonModes) };
        Ok(Self {
            reference: timer,
            interval,
            running: Cell::new(true),
            _callback: callback,
        })
    }

    pub fn set_running(&self, running: bool) {
        if self.running.replace(running) == running {
            return;
        }
        let delay = if running {
            self.interval.as_secs_f64()
        } else {
            10_000_000_000.0
        };
        // A suspended clock retains its allocation while no animation work is scheduled.
        unsafe {
            core_foundation::runloop::CFRunLoopTimerSetNextFireDate(
                self.reference,
                CFAbsoluteTimeGetCurrent() + delay,
            );
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // Invalidation removes the timer from every mode before its callback owner is released.
        unsafe {
            CFRunLoopTimerInvalidate(self.reference);
            CFRelease(self.reference.cast());
        }
    }
}

extern "C" fn start_callback(_timer: CFRunLoopTimerRef, information: *mut c_void) {
    assert!(!information.is_null());
    let pointer = information.cast::<RefCell<Box<dyn FnMut()>>>();
    // Retain across the call: the callback may remove the view that owns this timer.
    let callback = unsafe {
        Rc::increment_strong_count(pointer);
        Rc::from_raw(pointer)
    };
    match callback.try_borrow_mut() {
        Ok(mut callback) => callback(),
        Err(error) => eprintln!("The live timer callback is already running: {error}"),
    }
}
