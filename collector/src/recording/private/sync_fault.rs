//! Native full-sync refusal, loaded only into a test's isolated child process.
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
    fn strcmp(left: *const c_char, right: *const c_char) -> i32;
    fn open(path: *const c_char, flags: i32, ...) -> i32;
    fn write(descriptor: i32, bytes: *const c_void, length: usize) -> isize;
    fn close(descriptor: i32) -> i32;
    fn __error() -> *mut i32;
}

// Only the argument-free F_FULLFSYNC enters Rust; other variadic calls retain their ABI.
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
    let target = unsafe { getenv(c"FLIGHTDECK_PRIVATE_SYNC_TARGET".as_ptr()) };
    if target.is_null() {
        return unsafe { fcntl(descriptor, command) };
    }

    let mut path = [0_u8; 1024];
    if unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } != 0 {
        return -1;
    }

    if unsafe { strcmp(path.as_ptr().cast(), target) } != 0 {
        return unsafe { fcntl(descriptor, command) };
    }

    unsafe { record_refusal() };
    unsafe { *__error() = 5 };

    -1
}

unsafe fn record_refusal() {
    let observed = unsafe { getenv(c"FLIGHTDECK_PRIVATE_SYNC_OBSERVED".as_ptr()) };
    assert!(!observed.is_null());
    let descriptor = unsafe { open(observed, 0x1 | 0x8 | 0x0100_0000) };
    assert!(descriptor >= 0);
    let written = unsafe { write(descriptor, b"x".as_ptr().cast(), 1) };
    let closed = unsafe { close(descriptor) };

    assert_eq!(1, written);
    assert_eq!(0, closed);
}
