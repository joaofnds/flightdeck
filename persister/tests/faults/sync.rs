//! Opt-in native failure injection, loaded only into this test's spawned cohort.
use std::ffi::{c_char, c_void};

#[repr(C)]
struct Interpose {
    replacement: unsafe extern "C" fn(i32, i32) -> i32,
    original: unsafe extern "C" fn(i32, i32, ...) -> i32,
}

#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE: Interpose = Interpose {
    replacement: route_fcntl,
    original: fcntl,
};

unsafe extern "C" {
    fn fcntl(descriptor: i32, command: i32, ...) -> i32;
    fn getenv(name: *const c_char) -> *const c_char;
    fn getprogname() -> *const c_char;
    fn strcmp(left: *const c_char, right: *const c_char) -> i32;
    fn access(path: *const c_char, mode: i32) -> i32;
    fn __error() -> *mut i32;
    fn open(path: *const c_char, flags: i32, ...) -> i32;
    fn write(descriptor: i32, bytes: *const c_void, length: usize) -> isize;
    fn close(descriptor: i32) -> i32;
    fn getpid() -> i32;
    fn usleep(microseconds: u32) -> i32;
    fn _exit(status: i32) -> !;
}

#[used]
#[unsafe(link_section = "__DATA,__mod_init_func")]
static HOLD_STARTUP: unsafe extern "C" fn() = hold_startup;

unsafe extern "C" fn hold_startup() {
    let flag = unsafe { getenv(c"FLIGHTDECK_TEST_HOLD_STARTUP".as_ptr()) };
    if flag.is_null() || unsafe { strcmp(getprogname(), c"persister".as_ptr()) } != 0 {
        return;
    }
    if unsafe { access(flag, 0) } != 0 {
        return;
    }
    let observed = unsafe { getenv(c"FLIGHTDECK_TEST_STARTUP_OBSERVED".as_ptr()) };
    let pid = unsafe { getpid() }.to_ne_bytes();
    if !unsafe { write_observed(observed, &pid) } || !unsafe { wait_released(flag) } {
        unsafe { _exit(74) };
    }
}

// fcntl's other commands have variadic arguments. Tail-forward their untouched ABI;
// only F_FULLFSYNC (51), which takes no variadic argument, enters the Rust function.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn route_fcntl(_descriptor: i32, _command: i32) -> i32 {
    core::arch::naked_asm!(
        "cmp w1, #51",
        "b.eq 2f",
        "b {original}",
        "2:",
        "b {failure}",
        original = sym fcntl,
        failure = sym refuse_full_sync,
    );
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn route_fcntl(_descriptor: i32, _command: i32) -> i32 {
    core::arch::naked_asm!(
        "cmp esi, 51",
        "je 2f",
        "jmp {original}",
        "2:",
        "jmp {failure}",
        original = sym fcntl,
        failure = sym refuse_full_sync,
    );
}

unsafe extern "C" fn refuse_full_sync(descriptor: i32, command: i32) -> i32 {
    if !unsafe { hold_retired_sync(descriptor) } {
        unsafe { *__error() = 5 };
        return -1;
    }
    let flag = unsafe { getenv(c"FLIGHTDECK_TEST_REFUSE_SYNC".as_ptr()) };
    let mut path = [0_u8; 1024];
    let targeted = !flag.is_null()
        && unsafe { strcmp(getprogname(), c"persister".as_ptr()) } == 0
        && unsafe { access(flag, 0) } == 0
        && unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } == 0;
    if targeted {
        if let Ok(path) = std::ffi::CStr::from_bytes_until_nul(&path) {
            if path.to_bytes().ends_with(b".parquet") {
                let observed = unsafe { getenv(c"FLIGHTDECK_TEST_SYNC_OBSERVED".as_ptr()) };
                if !observed.is_null() {
                    let log = unsafe { open(observed, 0x1 | 0x200 | 0x400, 0o600) };
                    if log >= 0 {
                        let written =
                            unsafe { write(log, path.as_ptr().cast(), path.to_bytes().len()) };
                        let closed = unsafe { close(log) };
                        if written < 0 || closed != 0 {
                            unsafe { *__error() = 5 };
                            return -1;
                        }
                    }
                }
                unsafe { *__error() = 5 };
                return -1;
            }
        }
    }
    // dyld excludes the interposer image from its own replacement binding.
    unsafe { fcntl(descriptor, command) }
}

unsafe fn hold_retired_sync(descriptor: i32) -> bool {
    let flag = unsafe { getenv(c"FLIGHTDECK_TEST_HOLD_RETIRED_SYNC".as_ptr()) };
    let target = unsafe { getenv(c"FLIGHTDECK_TEST_RETIRED_DIRECTORY".as_ptr()) };
    if flag.is_null()
        || target.is_null()
        || unsafe { strcmp(getprogname(), c"persister".as_ptr()) } != 0
        || unsafe { access(flag, 0) } != 0
    {
        return true;
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } != 0 {
        return false;
    }
    if unsafe { strcmp(path.as_ptr().cast(), target) } != 0 {
        return true;
    }
    let Ok(path) = std::ffi::CStr::from_bytes_until_nul(&path) else {
        return false;
    };
    let observed = unsafe { getenv(c"FLIGHTDECK_TEST_RETIRED_SYNC_OBSERVED".as_ptr()) };
    unsafe { write_observed(observed, path.to_bytes()) && wait_released(flag) }
}

unsafe fn write_observed(path: *const c_char, bytes: &[u8]) -> bool {
    if path.is_null() {
        return false;
    }
    let descriptor = unsafe { open(path, 0x1 | 0x200 | 0x400, 0o600) };
    if descriptor < 0 {
        return false;
    }
    let written = unsafe { write(descriptor, bytes.as_ptr().cast(), bytes.len()) };
    let closed = unsafe { close(descriptor) };
    written >= 0 && usize::try_from(written) == Ok(bytes.len()) && closed == 0
}

unsafe fn wait_released(flag: *const c_char) -> bool {
    for _step in 0..6000 {
        if unsafe { access(flag, 0) } != 0 {
            return unsafe { *__error() } == 2;
        }
        let result = unsafe { usleep(10_000) };
        if result != 0 && unsafe { *__error() } != 4 {
            return false;
        }
    }
    false
}
