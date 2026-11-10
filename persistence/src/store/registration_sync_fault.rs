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
    unsafe { trace_control_sync(descriptor) };
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

    if !unsafe { matches_owned_target(path.as_ptr().cast(), target) } {
        return unsafe { fcntl(descriptor, command) };
    }

    let after = unsafe { getenv(c"FLIGHTDECK_SYNC_AFTER_EXISTS".as_ptr()) };
    if !after.is_null() && unsafe { access(after, 0) } != 0 {
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
    let selected = unsafe { matches_owned_target(path.as_ptr().cast(), target) };
    unsafe { pause_read(descriptor) };
    if !selected {
        return unsafe { read(descriptor, bytes, length) };
    }
    if unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } == 0
        && !unsafe { matches_owned_target(path.as_ptr().cast(), target) }
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
#[repr(C)]
#[derive(Clone, Copy)]
struct OpenDescriptor {
    descriptor: i32,
    kind: u32,
}
unsafe extern "C" {
    fn getpid() -> i32;
    fn proc_pidinfo(
        process: i32,
        flavor: i32,
        argument: u64,
        buffer: *mut c_void,
        size: i32,
    ) -> i32;
}
const PROC_PIDLISTFDS: i32 = 1;
const OPEN_DESCRIPTORS_MAX: usize = 1024;
// A census runs at every directory read and stat. Asking for each of the 1024 possible
// descriptors made one child's 34,786 passes cost 35 million calls, which passed the child's
// 180 second wait on a Mac running a test per core, while a process holds a few dozen.
unsafe fn open_descriptors(
    table: &mut [OpenDescriptor; OPEN_DESCRIPTORS_MAX],
) -> &[OpenDescriptor] {
    let bytes = unsafe {
        proc_pidinfo(
            getpid(),
            PROC_PIDLISTFDS,
            0,
            table.as_mut_ptr().cast(),
            i32::try_from(size_of_val(table)).unwrap(),
        )
    };
    assert!(bytes > 0);
    &table[..usize::try_from(bytes).unwrap() / size_of::<OpenDescriptor>()]
}
unsafe fn observe_descriptors(root: *const c_char) {
    let length = unsafe { strlen(root) };
    let mut namespaces = 0_u16;
    let mut raw = 0_u16;
    let mut table = [OpenDescriptor {
        descriptor: 0,
        kind: 0,
    }; OPEN_DESCRIPTORS_MAX];
    for open in unsafe { open_descriptors(&mut table) } {
        let fd = open.descriptor;
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
    if namespaces > 8 && !FD_REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        unsafe { report_descriptors(root) };
    }
    if !raises_descriptor_peak(namespaces, raw) {
        return;
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

static FD_RECORDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static FD_NAMESPACE_PEAK: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
static FD_RAW_PEAK: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
// The tests read only the peaks. Appending at every pass left one child's 34,786 passes blocked
// in write and close while other tests ran, past the child's 180 second wait.
fn raises_descriptor_peak(namespaces: u16, raw: u16) -> bool {
    let first = !FD_RECORDED.swap(true, std::sync::atomic::Ordering::Relaxed);
    let namespace_peak =
        FD_NAMESPACE_PEAK.fetch_max(namespaces, std::sync::atomic::Ordering::Relaxed);
    let raw_peak = FD_RAW_PEAK.fetch_max(raw, std::sync::atomic::Ordering::Relaxed);
    first || namespaces > namespace_peak || raw > raw_peak
}

static FD_REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
unsafe fn report_descriptors(root: *const c_char) {
    let length = unsafe { strlen(root) };
    for fd in 0..1024 {
        let mut path = [0_u8; 1024];
        if unsafe { fcntl(fd, 50, path.as_mut_ptr()) } != 0
            || unsafe { strncmp(path.as_ptr().cast(), root, length) } != 0
        {
            continue;
        }
        let count = unsafe { strlen(path.as_ptr().cast()) };
        assert_eq!(
            unsafe { write(1, path.as_ptr().cast(), count) },
            isize::try_from(count).unwrap()
        );
        assert_eq!(unsafe { write(1, c"\n".as_ptr().cast(), 1) }, 1);
    }
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
        || !unsafe { matches_owned_target(path.as_ptr().cast(), target) }
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
    unsafe { matches_owned_target(path.as_ptr().cast(), expected) }
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

#[repr(C)]
struct OpenAtInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_char, i32) -> i32,
    original: unsafe extern "C" fn(i32, *const c_char, i32, ...) -> i32,
}
unsafe extern "C" {
    fn openat(directory: i32, name: *const c_char, flags: i32, ...) -> i32;
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_OPENAT: OpenAtInterpose = OpenAtInterpose {
    replacement: route_openat,
    original: openat,
};
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn route_openat(_directory: i32, _name: *const c_char, _flags: i32) -> i32 {
    core::arch::naked_asm!(
        "tbnz w2, #9, 2f",
        "b {plain}",
        "2:",
        "ldr w3, [sp]",
        "b {create}",
        plain = sym observe_openat, create = sym observe_createat,
    );
}
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn route_openat(_directory: i32, _name: *const c_char, _flags: i32) -> i32 {
    core::arch::naked_asm!(
        "test edx, 0x200", "jnz 2f", "jmp {plain}", "2:", "jmp {create}",
        plain = sym observe_openat, create = sym observe_createat,
    );
}
unsafe extern "C" fn observe_openat(directory: i32, name: *const c_char, flags: i32) -> i32 {
    unsafe { trace_control_name(b'O', directory, name) };
    if unsafe { observe_openat_paced(directory, name) } {
        return -1;
    }
    let open_arm = unsafe { getenv(c"FLIGHTDECK_CONTROL_OPEN_ARMED".as_ptr()) };
    let enabled = open_arm.is_null() || unsafe { access(open_arm, 0) } == 0;
    if enabled
        && unsafe { mutation_fault(directory, name, c"open") }
        && !unsafe { replace_cleanup_name(directory, name) }
        && (unsafe { getenv(c"FLIGHTDECK_OPEN_EVERY_THIRD".as_ptr()) }.is_null()
            || OPEN_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 3 == 2)
    {
        unsafe {
            record_refusal();
            *__error() = 5;
        }
        return -1;
    }
    unsafe { openat(directory, name, flags) }
}
unsafe extern "C" fn observe_createat(
    directory: i32,
    name: *const c_char,
    flags: i32,
    mode: u32,
) -> i32 {
    unsafe { trace_control_name(b'C', directory, name) };
    if unsafe { mutation_fault(directory, name, c"create") } {
        unsafe {
            record_refusal();
            *__error() = 5;
        }
        return -1;
    }
    let result = unsafe { openat(directory, name, flags, mode) };
    if result >= 0
        && unsafe { mutation_fault(directory, name, c"create-effect") }
        && unsafe { metadata_sequence_byte() }.is_none_or(|byte| byte == b'c')
    {
        assert_eq!(unsafe { close(result) }, 0);
        unsafe {
            record_refusal();
            *__error() = 5;
        }
        return -1;
    }
    result
}
unsafe fn control_component(name: *const c_char) -> bool {
    [
        c"foreground",
        c"foreground.pending",
        c"namespace-state",
        c"namespace-state.pending",
    ]
    .iter()
    .any(|expected| unsafe { strcmp(name, expected.as_ptr()) } == 0)
}
unsafe fn trace_control_name(kind: u8, directory: i32, name: *const c_char) {
    if !unsafe { control_component(name) } {
        return;
    }
    let root = unsafe { getenv(c"FLIGHTDECK_FD_ROOT".as_ptr()) };
    if root.is_null() {
        return;
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(directory, 50, path.as_mut_ptr()) } != 0
        || unsafe { strcmp(path.as_ptr().cast(), root) } != 0
    {
        return;
    }
    unsafe { trace_control_byte(kind) };
}
unsafe fn trace_control_sync(descriptor: i32) {
    let root = unsafe { getenv(c"FLIGHTDECK_FD_ROOT".as_ptr()) };
    if root.is_null() {
        return;
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } != 0 {
        return;
    }
    if unsafe { strcmp(path.as_ptr().cast(), root) } == 0 {
        unsafe { trace_control_byte(b'D') };
        return;
    }
    let length = unsafe { strlen(root) };
    if length + 1 >= path.len()
        || path[length] != b'/'
        || unsafe { strncmp(path.as_ptr().cast(), root, length) } != 0
    {
        return;
    }
    if unsafe { control_component(path.as_ptr().add(length + 1).cast()) } {
        unsafe { trace_control_byte(b'F') };
    }
}
unsafe fn trace_control_byte(kind: u8) {
    let log = unsafe { getenv(c"FLIGHTDECK_CONTROL_TRACE".as_ptr()) };
    let armed = unsafe { getenv(c"FLIGHTDECK_CONTROL_ARMED".as_ptr()) };
    if log.is_null() || armed.is_null() || unsafe { access(armed, 0) } != 0 {
        return;
    }
    let error = unsafe { *__error() };
    let fd = unsafe { open(log, 0x1 | 0x8 | 0x0100_0000) };
    assert!(fd >= 0);
    assert_eq!(unsafe { write(fd, (&raw const kind).cast(), 1) }, 1);
    assert_eq!(unsafe { close(fd) }, 0);
    unsafe { *__error() = error };
}

#[repr(C)]
struct PwriteInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_void, usize, i64) -> isize,
    original: unsafe extern "C" fn(i32, *const c_void, usize, i64) -> isize,
}
unsafe extern "C" {
    fn pwrite(fd: i32, bytes: *const c_void, count: usize, offset: i64) -> isize;
    fn shm_open(name: *const c_char, flags: i32, ...) -> i32;
    fn mmap(
        address: *mut c_void,
        length: usize,
        protection: i32,
        flags: i32,
        descriptor: i32,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> i32;
}

#[repr(C)]
struct MappingInterpose {
    replacement: unsafe extern "C" fn(*mut c_void, usize, i32, i32, i32, i64) -> *mut c_void,
    original: unsafe extern "C" fn(*mut c_void, usize, i32, i32, i32, i64) -> *mut c_void,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_MAPPING: MappingInterpose = MappingInterpose {
    replacement: refuse_mapping,
    original: mmap,
};
unsafe extern "C" fn refuse_mapping(
    address: *mut c_void,
    length: usize,
    protection: i32,
    flags: i32,
    descriptor: i32,
    offset: i64,
) -> *mut c_void {
    let target = unsafe { getenv(c"FLIGHTDECK_MAPPING_TARGET".as_ptr()) };
    let armed = unsafe { getenv(c"FLIGHTDECK_REGISTRATION_ARMED".as_ptr()) };
    if !target.is_null() && !armed.is_null() && unsafe { access(armed, 0) } == 0 {
        let mut path = [0_u8; 1024];
        if unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } == 0
            && unsafe { strcmp(path.as_ptr().cast(), target) } == 0
        {
            unsafe { record_refusal() };
            unsafe { *__error() = 12 };
            return std::ptr::without_provenance_mut(usize::MAX);
        }
    }

    unsafe { mmap(address, length, protection, flags, descriptor, offset) }
}

#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_PWRITE: PwriteInterpose = PwriteInterpose {
    replacement: inspect_source_announcement,
    original: pwrite,
};
unsafe extern "C" fn inspect_source_announcement(
    fd: i32,
    bytes: *const c_void,
    count: usize,
    offset: i64,
) -> isize {
    let prefix = unsafe { getenv(c"FLIGHTDECK_SOURCE_INIT_PREFIX".as_ptr()) };
    if !prefix.is_null() && offset == 0 {
        let mut path = [0_u8; 1024];
        let length = unsafe { strlen(prefix) };
        if length + 6 < path.len()
            && unsafe { fcntl(fd, 50, path.as_mut_ptr()) } == 0
            && unsafe { strncmp(path.as_ptr().cast(), prefix, length) } == 0
        {
            let profile = path[length];
            assert!((b'0'..=b'4').contains(&profile));
            let name = unsafe { std::slice::from_raw_parts(bytes.cast::<u8>(), count) };
            unsafe { inspect_source_name(profile, name) };
        }
    }
    unsafe { pwrite(fd, bytes, count, offset) }
}
unsafe fn inspect_source_name(profile: u8, name: &[u8]) {
    assert!(!name.is_empty());
    assert!(name.len() < 32);
    let mut terminated = [0_u8; 32];
    terminated[..name.len()].copy_from_slice(name);
    let object = unsafe { shm_open(terminated.as_ptr().cast(), 0, 0_u32) };
    let sentinel = name.ends_with(b".0");
    if sentinel {
        assert_eq!(object, -1);
        unsafe { source_init_record(c"FLIGHTDECK_SOURCE_INIT_LOG", &[profile, b'S']) };
        return;
    }
    // Record this owned candidate name before assertions so the parent can clean an aborted child.
    unsafe { source_init_record(c"FLIGHTDECK_SOURCE_INIT_OBJECTS", name) };
    unsafe { source_init_record(c"FLIGHTDECK_SOURCE_INIT_OBJECTS", b"\n") };
    assert!(object >= 0);
    use std::os::fd::FromRawFd;
    let file = unsafe { std::fs::File::from_raw_fd(object) };
    let length = usize::try_from(file.metadata().unwrap().len()).unwrap();
    assert!(length >= 352);
    assert!(length < 1024 * 1024);
    let address = unsafe { mmap(std::ptr::null_mut(), length, 1, 1, object, 0) };
    assert_ne!(address as isize, -1);
    let words = unsafe {
        std::slice::from_raw_parts(address.cast::<std::sync::atomic::AtomicU64>(), length / 8)
    };
    inspect_source_header(words, profile);
    assert_eq!(unsafe { munmap(address, length) }, 0);
    drop(file);
    unsafe { source_init_record(c"FLIGHTDECK_SOURCE_INIT_LOG", &[profile, b'R']) };
}
fn inspect_source_header(words: &[std::sync::atomic::AtomicU64], profile: u8) {
    use std::sync::atomic::Ordering;
    assert_eq!(
        words[0].load(Ordering::Acquire),
        u64::from_be_bytes(*b"FLTDRING")
    );
    let value = |index: usize| words[index].load(Ordering::Relaxed);
    assert_eq!(value(1), if profile == b'0' { 5 } else { 6 });
    assert!(value(40) != 0 || value(41) != 0);
    assert_eq!(value(42), 1);
    assert_eq!(value(43), 0);
    assert_eq!(value(31), 1);
    assert_eq!(value(30), 0);
    for index in [2, 3, 6, 23, 25, 27] {
        assert!(value(index) > 0);
    }
    let captured = profile != b'0';
    let slot = value(17)
        + value(9) * value(18)
        + value(10) * value(19)
        + value(12)
        + value(11) * (1 + value(12))
        + value(13)
        + value(14) * value(20)
        + value(15) * value(21)
        + if captured { value(14) * value(36) } else { 0 };
    // A log cut into segments has one segment more than its events fill and a table of two
    // words a segment, and names the events of a segment in word 38.
    let events = if value(38) == 0 {
        value(16) * value(22)
    } else {
        let segment_count = value(16).div_ceil(value(38)) + 1;
        segment_count * value(38) * value(22) + segment_count * 2
    };
    let mut tail = 44
        + value(8) * slot
        + events
        + if captured {
            value(32) * value(37) + value(33) / 8
        } else {
            0
        };
    for (minimum, magic) in [
        (b'2', *b"FDBATT01"),
        (b'3', *b"FDDKTL01"),
        (b'4', *b"FDSYTL01"),
    ] {
        if profile >= minimum {
            let start = usize::try_from(tail).unwrap();
            assert_eq!(value(start), u64::from_be_bytes(magic));
            assert_eq!(value(start + 1), 1);
            assert_eq!(value(start + 2), value(8));
            if minimum != b'4' {
                tail += 4 + value(8) * value(start + 3);
            }
        }
    }
}
unsafe fn source_init_record(variable: &std::ffi::CStr, bytes: &[u8]) {
    let path = unsafe { getenv(variable.as_ptr()) };
    assert!(!path.is_null());
    let fd = unsafe { open(path, 0x1 | 0x8 | 0x0100_0000) };
    assert!(fd >= 0);
    assert_eq!(
        unsafe { write(fd, bytes.as_ptr().cast(), bytes.len()) },
        isize::try_from(bytes.len()).unwrap()
    );
    assert_eq!(unsafe { close(fd) }, 0);
}

unsafe fn replace_cleanup_name(directory: i32, name: *const c_char) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static ATTEMPTS: AtomicU64 = AtomicU64::new(0);
    let replacement = unsafe { getenv(c"FLIGHTDECK_CLEANUP_REPLACEMENT".as_ptr()) };
    if replacement.is_null() {
        return false;
    }
    let second = unsafe { getenv(c"FLIGHTDECK_CLEANUP_SECOND_OPEN".as_ptr()) };
    let selected = if second.is_null() { 1 } else { 2 };
    if ATTEMPTS.fetch_add(1, Ordering::Relaxed) + 1 == selected {
        assert_eq!(unsafe { renameat(-2, replacement, directory, name) }, 0);
        unsafe { record_refusal() };
    }
    true
}

#[repr(C)]
struct WriteInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_void, usize) -> isize,
    original: unsafe extern "C" fn(i32, *const c_void, usize) -> isize,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_WRITE: WriteInterpose = WriteInterpose {
    replacement: metadata_write_fault,
    original: write,
};
unsafe extern "C" fn metadata_write_fault(file: i32, bytes: *const c_void, count: usize) -> isize {
    let target = unsafe { getenv(c"FLIGHTDECK_METADATA_WRITE_TARGET".as_ptr()) };
    let arm = unsafe { getenv(c"FLIGHTDECK_REGISTRATION_ARMED".as_ptr()) };
    let mode = unsafe { getenv(c"FLIGHTDECK_METADATA_WRITE_MODE".as_ptr()) };
    if target.is_null() || arm.is_null() || mode.is_null() || unsafe { access(arm, 0) } != 0 {
        return unsafe { write(file, bytes, count) };
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(file, 50, path.as_mut_ptr()) } != 0
        || !unsafe { matches_owned_target(path.as_ptr().cast(), target) }
    {
        return unsafe { write(file, bytes, count) };
    }
    let sequence = unsafe { metadata_sequence_byte() };
    if (unsafe { strcmp(mode, c"inner".as_ptr()) } == 0
        || unsafe { strcmp(mode, c"complete-inner".as_ptr()) } == 0
        || sequence == Some(b'i'))
        && count == 512
    {
        return unsafe { write(file, bytes, count) };
    }
    let prefix = if unsafe { strcmp(mode, c"complete".as_ptr()) } == 0
        || unsafe { strcmp(mode, c"complete-inner".as_ptr()) } == 0
        || sequence == Some(b'p')
    {
        count
    } else {
        count.min(137)
    };
    let result = unsafe { write(file, bytes, prefix) };
    if result < 0 {
        return result;
    }
    unsafe {
        record_refusal();
        *__error() = 5;
    }
    -1
}

static OPEN_ATTEMPTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
unsafe fn observe_openat_paced(directory: i32, name: *const c_char) -> bool {
    let root = unsafe { getenv(c"FLIGHTDECK_PACED_ROOT".as_ptr()) };
    let log = unsafe { getenv(c"FLIGHTDECK_PACED_LOG".as_ptr()) };
    if root.is_null() || log.is_null() {
        return false;
    }
    let length = unsafe { strlen(name) };
    if length != 12 || unsafe { strcmp(name.add(4), c".parquet".as_ptr()) } != 0 {
        return false;
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(directory, 50, path.as_mut_ptr()) } != 0
        || unsafe { strncmp(path.as_ptr().cast(), root, strlen(root)) } != 0
    {
        return false;
    }
    let output = unsafe { open(log, 0x1 | 0x8 | 0x0100_0000) };
    assert!(output >= 0);
    assert_eq!(unsafe { write(output, b"x".as_ptr().cast(), 1) }, 1);
    assert_eq!(unsafe { close(output) }, 0);
    let armed = unsafe { getenv(c"FLIGHTDECK_PACED_BATCH_ARM".as_ptr()) };
    if !armed.is_null()
        && unsafe { access(armed, 0) } == 0
        && unsafe { strcmp(name, c"0066.parquet".as_ptr()) } == 0
    {
        unsafe {
            record_refusal();
            *__error() = 5;
        }
        return true;
    }
    false
}

unsafe fn metadata_sequence_byte() -> Option<u8> {
    let mode = unsafe { getenv(c"FLIGHTDECK_METADATA_WRITE_MODE".as_ptr()) };
    if mode.is_null() || unsafe { strcmp(mode, c"sequence".as_ptr()) } != 0 {
        return None;
    }
    let arm = unsafe { getenv(c"FLIGHTDECK_REGISTRATION_ARMED".as_ptr()) };
    if arm.is_null() {
        return None;
    }
    let file = unsafe { open(arm, 0) };
    if file < 0 {
        return None;
    }
    let mut byte = 0u8;
    let count = unsafe { read(file, (&raw mut byte).cast(), 1) };
    assert_eq!(unsafe { close(file) }, 0);
    assert_eq!(count, 1);
    Some(byte)
}

// A fresh finite transaction chooses its private object name after the child starts.
// Suffix faults remain restricted to the explicitly owned fixture root.
unsafe fn matches_owned_target(path: *const c_char, target: *const c_char) -> bool {
    if unsafe { *target } != b'@' as c_char {
        return unsafe { strcmp(path, target) == 0 };
    }
    let root = unsafe { getenv(c"FLIGHTDECK_FINITE_ROOT".as_ptr()) };
    if root.is_null() {
        return false;
    }
    let mut path_bytes = [0; 1024];
    let mut root_bytes = [0; 1024];
    let mut suffix = [0; 1024];
    let Some(length) = (unsafe { bounded_target(path, &mut path_bytes) }) else {
        return false;
    };
    let Some(root_length) = (unsafe { bounded_target(root, &mut root_bytes) }) else {
        return false;
    };
    let Some(suffix_length) = (unsafe { bounded_target(target.add(1), &mut suffix) }) else {
        return false;
    };
    root_length < length
        && path_bytes[..root_length] == root_bytes[..root_length]
        && path_bytes[root_length] == b'/'
        && suffix_length > 0
        && suffix[0] == b'/'
        && path_bytes[..length].ends_with(&suffix[..suffix_length])
}
unsafe fn bounded_target(pointer: *const c_char, output: &mut [u8; 1024]) -> Option<usize> {
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = unsafe { *pointer.add(index) as u8 };
        if *byte == 0 {
            return Some(index);
        }
    }
    None
}
