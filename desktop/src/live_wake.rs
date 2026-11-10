use core_foundation::base::{CFRelease, CFRetain};
use core_foundation::runloop::{
    CFRunLoopAddSource, CFRunLoopGetMain, CFRunLoopRef, CFRunLoopSourceContext,
    CFRunLoopSourceCreate, CFRunLoopSourceInvalidate, CFRunLoopSourceRef, CFRunLoopSourceSignal,
    CFRunLoopWakeUp, kCFRunLoopCommonModes,
};
use core_foundation::string::CFStringRef;
use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr;
use std::rc::Rc;

#[cfg(test)]
#[path = "live_wake_tests.rs"]
mod tests;

/// A callback the main run loop runs each time another thread signals it, so the page takes a
/// View the moment the store leaves it rather than at its next timer tick.
pub struct Wake {
    source: CFRunLoopSourceRef,
    run_loop: CFRunLoopRef,
    _callback: Rc<RefCell<Box<dyn FnMut()>>>,
}

impl Wake {
    pub fn start(callback: impl FnMut() + 'static) -> Result<Self, String> {
        // The callback and its owner are confined to the main run loop by Rc.
        if unsafe { libc::pthread_main_np() } != 1 {
            return Err("The page's wake must start on the main thread".into());
        }

        unsafe { Self::on_run_loop(CFRunLoopGetMain(), kCFRunLoopCommonModes, callback) }
    }

    /// A wake on `run_loop` in `mode`.
    ///
    /// # Safety
    ///
    /// `run_loop` is the current thread's, since the callback runs there and is not `Send`.
    unsafe fn on_run_loop(
        run_loop: CFRunLoopRef,
        mode: CFStringRef,
        callback: impl FnMut() + 'static,
    ) -> Result<Self, String> {
        let callback: Rc<RefCell<Box<dyn FnMut()>>> = Rc::new(RefCell::new(Box::new(callback)));
        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: Rc::as_ptr(&callback).cast_mut().cast(),
            retain: None,
            release: None,
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: perform_callback,
        };

        // The owner retains the callback until after the source is invalidated and released.
        let source = unsafe { CFRunLoopSourceCreate(ptr::null(), 0, &raw mut context) };
        if source.is_null() {
            return Err("CoreFoundation could not create the page's wake".into());
        }
        unsafe {
            CFRunLoopAddSource(run_loop, source, mode);
            CFRetain(run_loop.cast());
        }
        Ok(Self {
            source,
            run_loop,
            _callback: callback,
        })
    }

    /// What another thread wakes this run loop through.
    pub fn signal(&self) -> Signal {
        unsafe {
            CFRetain(self.source.cast());
            CFRetain(self.run_loop.cast());
        }

        Signal {
            source: self.source,
            run_loop: self.run_loop,
        }
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // Invalidation removes the source from every run loop before its callback owner is
        // released, so a signal that arrives later runs nothing.
        unsafe {
            CFRunLoopSourceInvalidate(self.source);
            CFRelease(self.source.cast());
            CFRelease(self.run_loop.cast());
        }
    }
}

/// A wake's source and run loop, retained so a signal after the wake is gone is harmless.
pub struct Signal {
    source: CFRunLoopSourceRef,
    run_loop: CFRunLoopRef,
}

// CoreFoundation's run loop functions are thread-safe, and a Signal only signals its source and
// wakes its run loop, which is what another thread does with them.
unsafe impl Send for Signal {}
unsafe impl Sync for Signal {}

impl Signal {
    pub fn signal(&self) {
        unsafe {
            CFRunLoopSourceSignal(self.source);
            CFRunLoopWakeUp(self.run_loop);
        }
    }
}

impl Drop for Signal {
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.source.cast());
            CFRelease(self.run_loop.cast());
        }
    }
}

extern "C" fn perform_callback(information: *const c_void) {
    assert!(!information.is_null());
    let pointer = information.cast::<RefCell<Box<dyn FnMut()>>>();
    // Retain across the call: the callback may remove the view that owns this wake.
    let callback = unsafe {
        Rc::increment_strong_count(pointer);
        Rc::from_raw(pointer)
    };
    match callback.try_borrow_mut() {
        Ok(mut callback) => callback(),
        Err(error) => eprintln!("The page's wake is already running: {error}"),
    }
}
