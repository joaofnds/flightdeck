use std::ffi::{CString, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub struct Replacement<'paths> {
    pub image: &'paths Path,
    pub root: &'paths Path,
    pub test: &'paths str,
    pub phase: u32,
}

pub fn replace(input: &Replacement<'_>) -> ! {
    let arguments = [
        CString::new(input.image.as_os_str().as_bytes()).unwrap(),
        CString::new("--exact").unwrap(),
        CString::new(input.test).unwrap(),
        CString::new("--nocapture").unwrap(),
    ];
    let environment = [
        CString::new("FLIGHTDECK_ASSOC_ROLE=child").unwrap(),
        CString::new(format!("FLIGHTDECK_ASSOC_ROOT={}", input.root.display())).unwrap(),
        CString::new(format!("FLIGHTDECK_ASSOC_PHASE={}", input.phase)).unwrap(),
        CString::new("FLIGHTDECK_ASSOC_MODE=setexec").unwrap(),
    ];
    let mut argument_pointers = [std::ptr::null_mut(); 5];
    let mut environment_pointers = [std::ptr::null_mut(); 5];
    for (index, argument) in arguments.iter().enumerate() {
        argument_pointers[index] = argument.as_ptr().cast_mut();
    }
    for (index, entry) in environment.iter().enumerate() {
        environment_pointers[index] = entry.as_ptr().cast_mut();
    }
    let mut pointer = std::ptr::null_mut();
    assert_eq!(0, unsafe { posix_spawnattr_init(&raw mut pointer) });
    let mut attributes = Attributes { pointer };
    // SDK sys/spawn.h declares POSIX_SPAWN_SETEXEC=0x0040 and spawn.h uses short flags.
    assert_eq!(0, unsafe {
        posix_spawnattr_setflags(&raw mut attributes.pointer, 0x0040)
    });
    let mut pid = 0;
    let status = unsafe {
        posix_spawn(
            &raw mut pid,
            arguments[0].as_ptr(),
            std::ptr::null(),
            &raw const attributes.pointer,
            argument_pointers.as_ptr(),
            environment_pointers.as_ptr(),
        )
    };
    panic!("POSIX_SPAWN_SETEXEC returned status={status} pid={pid}");
}

struct Attributes {
    pointer: *mut c_void,
}

impl Drop for Attributes {
    fn drop(&mut self) {
        let status = unsafe { posix_spawnattr_destroy(&raw mut self.pointer) };
        if status != 0 {
            eprintln!("spawn attributes release status={status}");
        }
    }
}

unsafe extern "C" {
    fn posix_spawnattr_init(attributes: *mut *mut c_void) -> i32;
    fn posix_spawnattr_setflags(attributes: *mut *mut c_void, flags: i16) -> i32;
    fn posix_spawnattr_destroy(attributes: *mut *mut c_void) -> i32;
    fn posix_spawn(
        pid: *mut i32,
        path: *const c_char,
        actions: *const c_void,
        attributes: *const *mut c_void,
        arguments: *const *mut c_char,
        environment: *const *mut c_char,
    ) -> i32;
}
