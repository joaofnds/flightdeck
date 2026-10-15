//! Startup registration shared by the collector and its separate persistence worker.

pub mod child;
pub mod discovery;
pub mod private;
mod registry;

pub use registry::{RegisteredRun, Registry};

use std::ffi::{c_char, c_int, c_void};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::clock::{self, Timebase};
use crate::ring::{Identity, ObservationTimes, Place, RunIdentity};
use private::{FileLock, create_private_dir, full_sync, open_private_file, sync_directory};

unsafe extern "C" {
    fn getentropy(buffer: *mut c_void, length: usize) -> c_int;
    fn mach_absolute_time() -> u64;
    fn sysctlbyname(
        name: *const c_char,
        output: *mut c_void,
        size: *mut usize,
        input: *mut c_void,
        input_size: usize,
    ) -> c_int;
}

/// Startup-only state: neither registry serialization nor child spawning enters a tick.
pub struct Registration {
    root: PathBuf,
    identity: RunIdentity,
}

impl Registration {
    pub fn prepare(root: &Path, place: &Place) -> io::Result<Self> {
        create_private_dir(root)?;
        let lock = FileLock::try_lock(&root.join("registration.lock"))?;
        let recording = recording_identity(root)?;
        let mut registry = Registry::read(root)?;
        registry.capture_previous(&place.lock_path, recording)?;
        registry.write(root)?;
        let identity = new_run_identity(recording)?;
        let registration = Self {
            root: root.to_path_buf(),
            identity,
        };
        drop(lock);
        Ok(registration)
    }

    #[must_use]
    pub fn identity(&self) -> &RunIdentity {
        &self.identity
    }

    pub fn register(&self, ring_name: &str) -> io::Result<()> {
        let lock = FileLock::try_lock(&self.root.join("registration.lock"))?;
        let mut registry = Registry::read(&self.root)?;
        registry.add(RegisteredRun {
            identity: self.identity,
            ring_name: ring_name.to_owned(),
            collector_pid: std::process::id(),
        })?;
        registry.write(&self.root)?;
        drop(lock);
        Ok(())
    }
}

pub fn recording_identity(root: &Path) -> io::Result<Identity> {
    let path = root.join("identity");
    match open_private_file(&path, false) {
        Ok(mut file) => {
            let mut text = String::new();
            Read::by_ref(&mut file).take(34).read_to_string(&mut text)?;
            parse_identity(text.trim_end_matches('\n'))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let identity = random_identity()?;
            let mut file = open_private_file(&path, true)?;
            writeln!(file, "{}", identity_text(identity))?;
            full_sync(&file)?;
            sync_directory(root)?;
            Ok(identity)
        }
        Err(error) => Err(error),
    }
}

pub fn random_identity() -> io::Result<Identity> {
    let mut words = [0_u64; 2];
    if unsafe { getentropy(words.as_mut_ptr().cast(), size_of_val(&words)) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if words == [0, 0] {
        return Err(io::Error::other("empty random recording identity"));
    }

    Ok(Identity(words))
}

pub fn new_run_identity(recording: Identity) -> io::Result<RunIdentity> {
    Ok(RunIdentity {
        recording,
        run: random_identity()?,
        boot: boot_identity()?,
    })
}

pub struct IdentityText(Identity);

impl std::fmt::Display for IdentityText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:016x}{:016x}", self.0.0[0], self.0.0[1])
    }
}

#[must_use]
pub fn identity_text(identity: Identity) -> IdentityText {
    IdentityText(identity)
}

pub fn parse_identity(text: &str) -> io::Result<Identity> {
    if text.len() != 32 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    if !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let first = u64::from_str_radix(&text[..16], 16)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let second = u64::from_str_radix(&text[16..], 16)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    Ok(Identity([first, second]))
}

pub fn observation_times(timebase: Timebase) -> io::Result<ObservationTimes> {
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let wall_ns = u64::try_from(wall.as_nanos()).map_err(io::Error::other)?;
    let continuous_ns = timebase.nanoseconds(clock::now())?;
    let awake_ns = timebase.nanoseconds(unsafe { mach_absolute_time() })?;
    Ok(ObservationTimes {
        wall_ns,
        continuous_ns,
        awake_ns,
    })
}

fn boot_identity() -> io::Result<Identity> {
    let mut bytes = [0_u8; 64];
    let mut length = bytes.len();
    let result = unsafe {
        sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &raw mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length > bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "boot identity length",
        ));
    }
    let text = std::str::from_utf8(&bytes[..length]).map_err(io::Error::other)?;
    let compact: String = text
        .trim_end_matches('\0')
        .chars()
        .filter(|c| *c != '-')
        .collect();
    parse_identity(&compact)
}

pub(crate) fn write_atomic(root: &Path, name: &str, text: &str) -> io::Result<()> {
    let temporary = root.join(format!("{name}.pending"));
    if temporary.try_exists()? {
        private::verify_file(&temporary)?;
        std::fs::remove_file(&temporary)?;
    }
    let mut file: File = open_private_file(&temporary, true)?;
    file.write_all(text.as_bytes())?;
    full_sync(&file)?;
    std::fs::rename(&temporary, root.join(name))?;
    sync_directory(root)
}
