//! Ends its process at one numbered change to a recording, as a kill would: before the change, or
//! with half of a write done. A full sync changes nothing the next process can see, so it has no
//! number and returns at once, since a kill loses nothing a sync would have kept and a run's time
//! is mostly its syncs. Nothing outside the directory the test names has a number either.
use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicU64, Ordering};

const O_CREAT_BIT: u32 = 9;
const F_GETPATH: i32 = 50;
const F_FULLFSYNC: i32 = 51;
const SEEK_CUR: i32 = 1;
/// What a test reads as this library having ended the process, which nothing else exits with.
const KILLED: i32 = 137;

static CHANGES: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" {
    fn _exit(status: i32) -> !;
    fn fcntl(descriptor: i32, command: i32, ...) -> i32;
    fn getenv(name: *const c_char) -> *const c_char;
    fn lseek(file: i32, offset: i64, whence: i32) -> i64;
    fn linkat(
        source: i32,
        name: *const c_char,
        target: i32,
        destination: *const c_char,
        flags: i32,
    ) -> i32;
    fn mkdirat(directory: i32, name: *const c_char, mode: u16) -> i32;
    fn openat(directory: i32, name: *const c_char, flags: i32, ...) -> i32;
    fn pwrite(file: i32, bytes: *const c_void, count: usize, offset: i64) -> isize;
    fn renameat(
        source: i32,
        name: *const c_char,
        target: i32,
        destination: *const c_char,
    ) -> i32;
    fn strlen(text: *const c_char) -> usize;
    fn strncmp(left: *const c_char, right: *const c_char, count: usize) -> i32;
    fn unlinkat(directory: i32, name: *const c_char, flags: i32) -> i32;
    fn write(file: i32, bytes: *const c_void, count: usize) -> isize;
}

unsafe fn kill_number() -> u64 {
    let text = unsafe { getenv(c"FLIGHTDECK_KILL_AT".as_ptr()) };
    if text.is_null() {
        return 0;
    }
    let mut number = 0;
    let mut position = text;
    loop {
        let digit = unsafe { *position } as u8;
        if !digit.is_ascii_digit() {
            return number;
        }
        number = number * 10 + u64::from(digit - b'0');
        position = unsafe { position.add(1) };
    }
}

unsafe fn inside_recording(descriptor: i32) -> bool {
    let root = unsafe { getenv(c"FLIGHTDECK_KILL_ROOT".as_ptr()) };
    if root.is_null() {
        return false;
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(descriptor, F_GETPATH, path.as_mut_ptr()) } != 0 {
        return false;
    }
    let length = unsafe { strlen(root) };
    let same_start = unsafe { strncmp(path.as_ptr().cast(), root, length) } == 0;

    same_start && (path[length] == 0 || path[length] == b'/')
}

/// Numbers one change to `descriptor`, and ends the process if the test named that number.
unsafe fn before_change(descriptor: i32) {
    if !unsafe { inside_recording(descriptor) } {
        return;
    }
    let number = CHANGES.fetch_add(1, Ordering::Relaxed) + 1;
    if number == unsafe { kill_number() } {
        unsafe { _exit(KILLED) };
    }
}

/// Numbers a write, and says whether the test named its half done, which leaves that half for the
/// caller to write. A write at the file's start has a number for before it as well, the only place
/// a kill leaves an empty file. Before a later write it leaves what half of that write leaves, only
/// shorter, and a number there would cost a run of the worker for no state of its own.
unsafe fn before_write(file: i32, at_start: bool) -> bool {
    if !unsafe { inside_recording(file) } {
        return false;
    }
    let named = unsafe { kill_number() };
    if at_start {
        let before = CHANGES.fetch_add(1, Ordering::Relaxed) + 1;
        if before == named {
            unsafe { _exit(KILLED) };
        }
    }

    CHANGES.fetch_add(1, Ordering::Relaxed) + 1 == named
}

#[repr(C)]
struct WriteInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_void, usize) -> isize,
    original: unsafe extern "C" fn(i32, *const c_void, usize) -> isize,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_WRITE: WriteInterpose = WriteInterpose {
    replacement: killed_write,
    original: write,
};
unsafe extern "C" fn killed_write(file: i32, bytes: *const c_void, count: usize) -> isize {
    if unsafe { before_write(file, lseek(file, 0, SEEK_CUR) == 0) } {
        // A write this library cannot complete ends the process just the same.
        unsafe { write(file, bytes, count / 2) };
        unsafe { _exit(KILLED) };
    }
    unsafe { write(file, bytes, count) }
}

#[repr(C)]
struct PwriteInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_void, usize, i64) -> isize,
    original: unsafe extern "C" fn(i32, *const c_void, usize, i64) -> isize,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_PWRITE: PwriteInterpose = PwriteInterpose {
    replacement: killed_pwrite,
    original: pwrite,
};
unsafe extern "C" fn killed_pwrite(
    file: i32,
    bytes: *const c_void,
    count: usize,
    offset: i64,
) -> isize {
    if unsafe { before_write(file, offset == 0) } {
        // A write this library cannot complete ends the process just the same.
        unsafe { pwrite(file, bytes, count / 2, offset) };
        unsafe { _exit(KILLED) };
    }
    unsafe { pwrite(file, bytes, count, offset) }
}

#[repr(C)]
struct RenameInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_char, i32, *const c_char) -> i32,
    original: unsafe extern "C" fn(i32, *const c_char, i32, *const c_char) -> i32,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_RENAME: RenameInterpose = RenameInterpose {
    replacement: killed_rename,
    original: renameat,
};
unsafe extern "C" fn killed_rename(
    source: i32,
    name: *const c_char,
    target: i32,
    destination: *const c_char,
) -> i32 {
    unsafe { before_change(target) };
    unsafe { renameat(source, name, target, destination) }
}

#[repr(C)]
struct LinkInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_char, i32, *const c_char, i32) -> i32,
    original: unsafe extern "C" fn(i32, *const c_char, i32, *const c_char, i32) -> i32,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_LINK: LinkInterpose = LinkInterpose {
    replacement: killed_link,
    original: linkat,
};
unsafe extern "C" fn killed_link(
    source: i32,
    name: *const c_char,
    target: i32,
    destination: *const c_char,
    flags: i32,
) -> i32 {
    unsafe { before_change(target) };
    unsafe { linkat(source, name, target, destination, flags) }
}

#[repr(C)]
struct UnlinkInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_char, i32) -> i32,
    original: unsafe extern "C" fn(i32, *const c_char, i32) -> i32,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_UNLINK: UnlinkInterpose = UnlinkInterpose {
    replacement: killed_unlink,
    original: unlinkat,
};
unsafe extern "C" fn killed_unlink(directory: i32, name: *const c_char, flags: i32) -> i32 {
    unsafe { before_change(directory) };
    unsafe { unlinkat(directory, name, flags) }
}

#[repr(C)]
struct DirectoryInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_char, u16) -> i32,
    original: unsafe extern "C" fn(i32, *const c_char, u16) -> i32,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_DIRECTORY: DirectoryInterpose = DirectoryInterpose {
    replacement: killed_directory,
    original: mkdirat,
};
unsafe extern "C" fn killed_directory(directory: i32, name: *const c_char, mode: u16) -> i32 {
    unsafe { before_change(directory) };
    unsafe { mkdirat(directory, name, mode) }
}

#[repr(C)]
struct SyncInterpose {
    replacement: unsafe extern "C" fn(i32, i32) -> i32,
    original: unsafe extern "C" fn(i32, i32, ...) -> i32,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_SYNC: SyncInterpose = SyncInterpose {
    replacement: route_control,
    original: fcntl,
};

// Only a full sync returns here. Every other control keeps its arguments, which a variadic call
// passes differently on each architecture.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn route_control(_descriptor: i32, _command: i32) -> i32 {
    core::arch::naked_asm!(
        "cmp w1, #{full_sync}",
        "b.eq 2f",
        "b {original}",
        "2:",
        "mov w0, #0",
        "ret",
        full_sync = const F_FULLFSYNC,
        original = sym fcntl,
    );
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn route_control(_descriptor: i32, _command: i32) -> i32 {
    core::arch::naked_asm!(
        "cmp esi, {full_sync}",
        "je 2f",
        "jmp {original}",
        "2:",
        "xor eax, eax",
        "ret",
        full_sync = const F_FULLFSYNC,
        original = sym fcntl,
    );
}

#[repr(C)]
struct OpenInterpose {
    replacement: unsafe extern "C" fn(i32, *const c_char, i32) -> i32,
    original: unsafe extern "C" fn(i32, *const c_char, i32, ...) -> i32,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE_OPEN: OpenInterpose = OpenInterpose {
    replacement: route_open,
    original: openat,
};

// Only a create is numbered. Every other open keeps its arguments, which a variadic call passes
// differently on each architecture.
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn route_open(_directory: i32, _name: *const c_char, _flags: i32) -> i32 {
    core::arch::naked_asm!(
        "tbnz w2, #{create_bit}, 2f",
        "b {original}",
        "2:",
        "ldr w3, [sp]",
        "b {create}",
        create_bit = const O_CREAT_BIT,
        original = sym openat,
        create = sym killed_create,
    );
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn route_open(_directory: i32, _name: *const c_char, _flags: i32) -> i32 {
    core::arch::naked_asm!(
        "test edx, 0x200",
        "jnz 2f",
        "jmp {original}",
        "2:",
        "jmp {create}",
        original = sym openat,
        create = sym killed_create,
    );
}

unsafe extern "C" fn killed_create(
    directory: i32,
    name: *const c_char,
    flags: i32,
    mode: u32,
) -> i32 {
    unsafe { before_change(directory) };
    unsafe { openat(directory, name, flags, mode) }
}
