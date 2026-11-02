//! Deterministic publication between real classifier reads in an isolated child.
use std::ffi::{CStr, c_char, c_void};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};

#[repr(C)]
struct Interpose {
    replacement: unsafe extern "C" fn(i32, *mut c_void, usize) -> isize,
    original: unsafe extern "C" fn(i32, *mut c_void, usize) -> isize,
}

#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static INTERPOSE: Interpose = Interpose {
    replacement: observe_read,
    original: read,
};
static HIT: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn read(descriptor: i32, bytes: *mut c_void, length: usize) -> isize;
    fn fcntl(descriptor: i32, command: i32, ...) -> i32;
    fn getenv(name: *const c_char) -> *const c_char;
}

unsafe extern "C" fn observe_read(descriptor: i32, bytes: *mut c_void, length: usize) -> isize {
    let received = unsafe { read(descriptor, bytes, length) };
    if received <= 0 {
        return received;
    }
    let target = unsafe { getenv(c"FLIGHTDECK_CLASSIFY_TARGET".as_ptr()) };
    if target.is_null() {
        return received;
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } != 0 {
        return received;
    }
    if unsafe { CStr::from_ptr(path.as_ptr().cast()) != CStr::from_ptr(target) }
        || HIT.swap(true, Ordering::SeqCst)
    {
        return received;
    }
    publish();
    received
}

fn publish() {
    use std::io::Write;
    let base = std::path::PathBuf::from(std::env::var_os("FLIGHTDECK_CLASSIFY_PROBE").unwrap());
    let recording = base.join("recording");
    match std::env::var("FLIGHTDECK_CLASSIFY_ACTION")
        .unwrap()
        .as_str()
    {
        "runtime" | "runtime-error" => {
            if std::env::var("FLIGHTDECK_CLASSIFY_ACTION").unwrap() == "runtime-error" {
                std::fs::rename(base.join("next-quiet"), recording.join("namespace-state"))
                    .unwrap();
                sync_root(&recording);
            }
            for name in ["foreground", "namespace-state"] {
                std::fs::rename(base.join(format!("next-{name}")), recording.join(name)).unwrap();
                sync_root(&recording);
            }
        }
        "identity" => {
            std::fs::rename(base.join("next-identity"), recording.join("identity")).unwrap();
            sync_root(&recording);
            std::fs::remove_file(recording.join("bootstrap")).unwrap();
            sync_root(&recording);
        }
        "bootstrap" => {
            std::fs::rename(
                recording.join("bootstrap.pending"),
                recording.join("bootstrap"),
            )
            .unwrap();
            sync_root(&recording);
            std::fs::rename(base.join("next-ordinary"), recording.join("ordinary")).unwrap();
            sync_root(&recording);
        }
        "pending" => {
            std::fs::rename(
                recording.join("foreground.pending"),
                recording.join("foreground"),
            )
            .unwrap();
            sync_root(&recording);
        }
        _ => panic!("unknown isolated classifier publication"),
    }
    let mut marker = std::fs::OpenOptions::new()
        .append(true)
        .open(base.join("observed"))
        .unwrap();
    marker.write_all(b"x").unwrap();
}

fn sync_root(root: &std::path::Path) {
    let directory = std::fs::File::open(root).unwrap();
    directory.sync_all().unwrap();
    assert_eq!(unsafe { fcntl(directory.as_raw_fd(), 51) }, 0);
}

#[repr(C)]
struct DirectoryInterpose {
    replacement: unsafe extern "C" fn(i32, *mut c_char, usize, *mut i64) -> isize,
    original: unsafe extern "C" fn(i32, *mut c_char, usize, *mut i64) -> isize,
}
#[used]
#[unsafe(link_section = "__DATA,__interpose")]
static DIRECTORY_INTERPOSE: DirectoryInterpose = DirectoryInterpose {
    replacement: observe_entries,
    original: __getdirentries64,
};
unsafe extern "C" {
    fn __getdirentries64(
        descriptor: i32,
        bytes: *mut c_char,
        length: usize,
        base: *mut i64,
    ) -> isize;
}
unsafe extern "C" fn observe_entries(
    descriptor: i32,
    bytes: *mut c_char,
    length: usize,
    base: *mut i64,
) -> isize {
    let received = unsafe { __getdirentries64(descriptor, bytes, length, base) };
    if received <= 0 {
        return received;
    }
    let target = unsafe { getenv(c"FLIGHTDECK_CLASSIFY_TARGET".as_ptr()) };
    if target.is_null() {
        return received;
    }
    let mut path = [0_u8; 1024];
    if unsafe { fcntl(descriptor, 50, path.as_mut_ptr()) } != 0 {
        return received;
    }
    if unsafe { CStr::from_ptr(path.as_ptr().cast()) != CStr::from_ptr(target) }
        || HIT.swap(true, Ordering::SeqCst)
    {
        return received;
    }
    publish();
    received
}
