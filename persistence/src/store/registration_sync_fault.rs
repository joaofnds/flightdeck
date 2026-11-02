//! Registration test interposer, active only after its isolated child creates the arm file.
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
    fn access(path: *const c_char, mode: i32) -> i32;
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
    let armed = unsafe { getenv(c"FLIGHTDECK_REGISTRATION_ARMED".as_ptr()) };
    if target.is_null() || armed.is_null() || unsafe { access(armed, 0) } != 0 {
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

#[repr(C)]
struct ReadInterpose {
    replacement: unsafe extern "C" fn(i32, *mut c_void, usize) -> isize,
    original: unsafe extern "C" fn(i32, *mut c_void, usize) -> isize,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_READ: ReadInterpose = ReadInterpose {
    replacement: refuse_read,
    original: read,
};
unsafe extern "C" {
    fn read(descriptor: i32, bytes: *mut c_void, length: usize) -> isize;
}
unsafe extern "C" fn refuse_read(descriptor: i32, bytes: *mut c_void, length: usize) -> isize {
    let target = unsafe { getenv(c"FLIGHTDECK_INVENTORY_READ_TARGET".as_ptr()) };
    let armed = unsafe { getenv(c"FLIGHTDECK_REGISTRATION_ARMED".as_ptr()) };
    if target.is_null() || armed.is_null() || unsafe { access(armed, 0) } != 0 {
        return unsafe { read(descriptor, bytes, length) };
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } != 0 {
        return -1;
    }
    if unsafe { strcmp(path.as_ptr().cast(), target) } != 0 {
        return unsafe { read(descriptor, bytes, length) };
    }
    unsafe { replace_state(descriptor) };
    unsafe { record_refusal() };
    unsafe { *__error() = 5 };
    -1
}

unsafe extern "C" {
    fn rename(source: *const c_char, target: *const c_char) -> i32;
    fn __getdirentries64(fd: i32, bytes: *mut c_char, size: usize, base: *mut i64) -> isize;
}
unsafe fn replace_state(descriptor: i32) {
    let stage = unsafe { getenv(c"FLIGHTDECK_STATE_STAGE".as_ptr()) };
    let target = unsafe { getenv(c"FLIGHTDECK_STATE_TARGET".as_ptr()) };
    let trigger = unsafe { getenv(c"FLIGHTDECK_STATE_TRIGGER".as_ptr()) };
    if stage.is_null() || target.is_null() || trigger.is_null() || unsafe { access(stage, 0) } != 0
    {
        return;
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } != 0 {
        return;
    }
    if unsafe { strcmp(path.as_ptr().cast(), trigger) } == 0 {
        assert_eq!(unsafe { rename(stage, target) }, 0);
    }
}
#[repr(C)]
struct DirectoryInterpose {
    replacement: unsafe extern "C" fn(i32, *mut c_char, usize, *mut i64) -> isize,
    original: unsafe extern "C" fn(i32, *mut c_char, usize, *mut i64) -> isize,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_DIRECTORY: DirectoryInterpose = DirectoryInterpose {
    replacement: read_directory,
    original: __getdirentries64,
};
unsafe extern "C" fn read_directory(
    fd: i32,
    bytes: *mut c_char,
    size: usize,
    base: *mut i64,
) -> isize {
    let result = unsafe { __getdirentries64(fd, bytes, size, base) };
    let error = unsafe { *__error() };
    let root = unsafe { getenv(c"FLIGHTDECK_FD_ROOT".as_ptr()) };
    if !root.is_null() {
        unsafe { observe_descriptors(root) };
    }
    unsafe { replace_state(fd) };
    unsafe { *__error() = error };
    result
}

unsafe extern "C" {
    #[cfg_attr(target_arch = "x86_64", link_name = "fstat$INODE64")]
    fn fstat(fd: i32, stat: *mut c_void) -> i32;
    fn strlen(value: *const c_char) -> usize;
    fn strncmp(left: *const c_char, right: *const c_char, count: usize) -> i32;
}
#[repr(C)]
struct StatInterpose {
    replacement: unsafe extern "C" fn(i32, *mut c_void) -> i32,
    original: unsafe extern "C" fn(i32, *mut c_void) -> i32,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_STAT: StatInterpose = StatInterpose {
    replacement: observe_stat,
    original: fstat,
};
unsafe extern "C" fn observe_stat(fd: i32, stat: *mut c_void) -> i32 {
    let result = unsafe { fstat(fd, stat) };
    let error = unsafe { *__error() };
    let root = unsafe { getenv(c"FLIGHTDECK_FD_ROOT".as_ptr()) };
    if !root.is_null() {
        unsafe { observe_descriptors(root) };
    }
    unsafe { *__error() = error };
    result
}
unsafe fn observe_descriptors(root: *const c_char) {
    let length = unsafe { strlen(root) };
    let mut namespaces = 0_u16;
    let mut raw = 0_u16;
    for fd in 0..1024 {
        let mut path = [0_u8; 1024];
        if unsafe { fcntl(fd, 50, path.as_mut_ptr()) } != 0 {
            continue;
        }
        if unsafe { strncmp(path.as_ptr().cast(), root, length) } != 0 {
            continue;
        }
        if path[length] != 0 && path[length] != b'/' {
            continue;
        }
        let mut stat = [0_u64; 18];
        if unsafe { fstat(fd, stat.as_mut_ptr().cast()) } != 0 {
            continue;
        }
        let bytes = unsafe { std::slice::from_raw_parts(stat.as_ptr().cast::<u8>(), 144) };
        let mode = u16::from_ne_bytes([bytes[4], bytes[5]]);
        let suffix = &path[length..];
        if mode & 0xf000 == 0x4000
            || suffix
                .iter()
                .take_while(|byte| **byte != 0)
                .filter(|byte| **byte == b'/')
                .count()
                <= 1
        {
            namespaces += 1;
        } else {
            raw += 1;
        }
    }
    let log = unsafe { getenv(c"FLIGHTDECK_FD_LOG".as_ptr()) };
    assert!(!log.is_null());
    let fd = unsafe { open(log, 0x1 | 0x8 | 0x0100_0000) };
    assert!(fd >= 0);
    let n = namespaces.to_le_bytes();
    let r = raw.to_le_bytes();
    let record = [n[0], n[1], r[0], r[1]];
    assert_eq!(unsafe { write(fd, record.as_ptr().cast(), 4) }, 4);
    assert_eq!(unsafe { close(fd) }, 0);
}
