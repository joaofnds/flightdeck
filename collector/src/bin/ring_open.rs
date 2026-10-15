//! Opens a ring read-only by its name and prints whether macOS let it, so a test can ask as
//! another user, who may not read the lock file that names the ring.

use std::ffi::{CString, c_char, c_int};
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::ExitCode;

unsafe extern "C" {
    fn shm_open(name: *const c_char, oflag: c_int, ...) -> c_int;
}

const O_RDONLY: c_int = 0x0000;

fn main() -> ExitCode {
    let name = match collector::arguments("ring_open").and_then(|arguments| name_from(&arguments)) {
        Ok(name) => name,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };

    let object = unsafe { shm_open(name.as_ptr(), O_RDONLY) };
    if object < 0 {
        let error = std::io::Error::last_os_error();
        let Some(errno) = error.raw_os_error() else {
            eprintln!("ring_open: shm_open failed without an errno: {error}");
            return ExitCode::FAILURE;
        };

        println!("refused with errno {errno}");
        return ExitCode::SUCCESS;
    }

    drop(unsafe { OwnedFd::from_raw_fd(object) });
    println!("opened");
    ExitCode::SUCCESS
}

fn name_from(arguments: &[String]) -> Result<CString, String> {
    let usage = || "usage: ring_open --name NAME".to_owned();
    match arguments {
        [flag, name] if flag == "--name" => CString::new(name.as_str()).map_err(|_| usage()),
        _ => Err(usage()),
    }
}
