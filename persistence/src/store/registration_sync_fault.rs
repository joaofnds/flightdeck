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
    unsafe { trace_operation(b'S', descriptor) };
    let target = unsafe { getenv(c"FLIGHTDECK_PRIVATE_SYNC_TARGET".as_ptr()) };
    let sync_armed = unsafe { getenv(c"FLIGHTDECK_SYNC_ARMED".as_ptr()) };
    let armed = if sync_armed.is_null() {
        unsafe { getenv(c"FLIGHTDECK_REGISTRATION_ARMED".as_ptr()) }
    } else {
        sync_armed
    };
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

    if !unsafe { getenv(c"FLIGHTDECK_SYNC_EFFECT".as_ptr()) }.is_null()
        && unsafe { fcntl(descriptor, command) } != 0
    {
        return -1;
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
        unsafe { pause_read(descriptor) };
        return unsafe { read(descriptor, bytes, length) };
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } != 0 {
        return -1;
    }
    let selected = unsafe { strcmp(path.as_ptr().cast(), target) } == 0;
    unsafe { pause_read(descriptor) };
    if !selected {
        return unsafe { read(descriptor, bytes, length) };
    }
    if unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } == 0
        && unsafe { strcmp(path.as_ptr().cast(), target) } != 0
    {
        let message = b"FD-READ-PATH-CHANGED\n";
        assert_eq!(
            unsafe { write(1, message.as_ptr().cast(), message.len()) },
            isize::try_from(message.len()).unwrap()
        );
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
    unsafe { pause_read(fd) };
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

static PAUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    returned: i16,
}
unsafe extern "C" {
    fn poll(descriptors: *mut PollFd, count: u32, timeout: i32) -> i32;
    fn renameat(source: i32, name: *const c_char, target: i32, destination: *const c_char) -> i32;
    fn linkat(
        source: i32,
        name: *const c_char,
        target: i32,
        destination: *const c_char,
        flags: i32,
    ) -> i32;
    fn unlinkat(directory: i32, name: *const c_char, flags: i32) -> i32;
}
unsafe fn pause_read(fd: i32) {
    let target = unsafe { getenv(c"FLIGHTDECK_PAUSE_TARGET".as_ptr()) };
    if target.is_null() {
        return;
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(fd, 50, path.as_mut_ptr()) } != 0
        || unsafe { strcmp(path.as_ptr().cast(), target) } != 0
    {
        return;
    }
    if PAUSED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let message = b"FD-PAUSED\n";
    assert_eq!(
        unsafe { write(1, message.as_ptr().cast(), message.len()) },
        isize::try_from(message.len()).unwrap()
    );
    let mut input = PollFd {
        fd: 0,
        events: 1,
        returned: 0,
    };
    assert_eq!(unsafe { poll(&raw mut input, 1, 30_000) }, 1);
    let mut byte = [0_u8; 1];
    assert_eq!(unsafe { read(0, byte.as_mut_ptr().cast(), 1) }, 1);
    assert_eq!(byte[0], b'g');
}
unsafe fn trace_operation(kind: u8, directory: i32) {
    let log = unsafe { getenv(c"FLIGHTDECK_PUBLICATION_TRACE".as_ptr()) };
    let root = unsafe { getenv(c"FLIGHTDECK_FD_ROOT".as_ptr()) };
    if log.is_null() || root.is_null() {
        return;
    }
    let mut path = [0_u8; 1024];
    let length = unsafe { strlen(root) };
    if unsafe { fcntl(directory, 50, path.as_mut_ptr()) } != 0
        || unsafe { strncmp(path.as_ptr().cast(), root, length) } != 0
        || (path[length] != 0 && path[length] != b'/')
    {
        return;
    }
    let fd = unsafe { open(log, 0x1 | 0x8 | 0x0100_0000) };
    assert!(fd >= 0);
    assert_eq!(unsafe { write(fd, (&raw const kind).cast(), 1) }, 1);
    assert_eq!(unsafe { close(fd) }, 0);
}
unsafe fn mutation_fault(directory: i32, name: *const c_char, operation: &std::ffi::CStr) -> bool {
    let expected = unsafe { getenv(c"FLIGHTDECK_MUTATION_TARGET".as_ptr()) };
    let kind = unsafe { getenv(c"FLIGHTDECK_MUTATION_KIND".as_ptr()) };
    let armed = unsafe { getenv(c"FLIGHTDECK_REGISTRATION_ARMED".as_ptr()) };
    if expected.is_null()
        || kind.is_null()
        || armed.is_null()
        || unsafe { access(armed, 0) } != 0
        || unsafe { strcmp(kind, operation.as_ptr()) } != 0
    {
        return false;
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(directory, 50, path.as_mut_ptr()) } != 0 {
        return false;
    }
    let prefix = unsafe { strlen(path.as_ptr().cast()) };
    let length = unsafe { strlen(name) };
    if prefix + 1 + length >= path.len() {
        return false;
    }
    path[prefix] = b'/';
    for index in 0..length {
        path[prefix + 1 + index] = unsafe { *name.add(index).cast::<u8>() };
    }
    unsafe { strcmp(path.as_ptr().cast(), expected) == 0 }
}
#[repr(C)]
struct RenameInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_char, i32, *const c_char) -> i32,
    original: unsafe extern "C" fn(i32, *const c_char, i32, *const c_char) -> i32,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_RENAME: RenameInterpose = RenameInterpose {
    replacement: rename_effect,
    original: renameat,
};
unsafe extern "C" fn rename_effect(
    source: i32,
    name: *const c_char,
    target: i32,
    destination: *const c_char,
) -> i32 {
    unsafe { trace_operation(b'R', target) };
    let result = unsafe { renameat(source, name, target, destination) };
    if result == 0 && unsafe { mutation_fault(target, destination, c"rename") } {
        unsafe {
            record_refusal();
            *__error() = 5;
        }
        return -1;
    }
    result
}
#[repr(C)]
struct LinkInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_char, i32, *const c_char, i32) -> i32,
    original: unsafe extern "C" fn(i32, *const c_char, i32, *const c_char, i32) -> i32,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_LINK: LinkInterpose = LinkInterpose {
    replacement: link_effect,
    original: linkat,
};
unsafe extern "C" fn link_effect(
    source: i32,
    name: *const c_char,
    target: i32,
    destination: *const c_char,
    flags: i32,
) -> i32 {
    unsafe { trace_operation(b'L', target) };
    let result = unsafe { linkat(source, name, target, destination, flags) };
    if result == 0 && unsafe { mutation_fault(target, destination, c"link") } {
        unsafe {
            record_refusal();
            *__error() = 5;
        }
        return -1;
    }
    result
}
#[repr(C)]
struct UnlinkInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_char, i32) -> i32,
    original: unsafe extern "C" fn(i32, *const c_char, i32) -> i32,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_UNLINK: UnlinkInterpose = UnlinkInterpose {
    replacement: unlink_effect,
    original: unlinkat,
};
unsafe extern "C" fn unlink_effect(directory: i32, name: *const c_char, flags: i32) -> i32 {
    unsafe { trace_operation(b'U', directory) };
    let result = unsafe { unlinkat(directory, name, flags) };
    if result == 0 && unsafe { mutation_fault(directory, name, c"unlink") } {
        unsafe {
            record_refusal();
            *__error() = 5;
        }
        return -1;
    }
    result
}
