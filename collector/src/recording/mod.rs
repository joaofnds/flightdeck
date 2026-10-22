//! Startup registration shared by the collector and its separate persistence worker.

pub mod child;
pub mod discovery;
mod holder;
pub mod private;
mod registry;

pub use holder::{Holder, holder};
pub use registry::{RegisteredRun, Registry};

/// The fingerprint of the code this collector is built from.
pub const BUILD: u64 = include!(concat!(env!("OUT_DIR"), "/build"));

/// How long a collector may take to finish once asked to stop, after which it is ended, so a
/// collector still finishing lets go of its recording within this many seconds.
pub const STOP_LIMIT_SECONDS: u32 = 150;

/// How long the persister may take to seal what the collector left, once the collector finishes.
pub const PERSISTER_FINISH_LIMIT: Duration = Duration::from_mins(2);
// A collector asked to stop ends its tick and waits out the persister well before its deadline.
const _: () = assert!(
    PERSISTER_FINISH_LIMIT.as_secs() + 30
        <= Duration::from_secs(1)
            .saturating_mul(STOP_LIMIT_SECONDS)
            .as_secs()
);

use std::ffi::{c_char, c_int, c_void};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::clock::{self, Timebase};
use crate::ring::{Identity, LockedPlace, ObservationTimes, Place, RunIdentity, Shape, Writer};
use private::{FileLock, create_private_dir, open_private_file};

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

pub struct StartedRecording {
    writer: Writer,
    registered: io::Result<()>,
    /// Why a registered run went unannounced. Its readings are still saved, but no other
    /// Flightdeck attaches to it.
    announcement: Option<io::Error>,
}

#[derive(Clone, Copy)]
enum RingLayout {
    Observed,
    Identity(crate::ring::DictionaryCapacity),
    Battery(crate::ring::DictionaryCapacity),
    Disks(crate::ring::DictionaryCapacity),
    System(crate::ring::DictionaryCapacity),
}

impl StartedRecording {
    pub fn into_parts(self) -> (Writer, io::Result<()>) {
        (self.writer, self.registered)
    }

    #[must_use]
    pub fn announcement_error(&self) -> Option<&io::Error> {
        self.announcement.as_ref()
    }
}

impl Registration {
    /// Reserves both optional sources within the existing live reader mapping bound.
    pub fn start_with_disks(
        root: &Path,
        place: &Place,
        shape: Shape,
        capacity: crate::ring::DictionaryCapacity,
        start_continuous_ns: u64,
    ) -> io::Result<StartedRecording> {
        let size_bytes = Writer::disks_size_bytes(shape, capacity)?;
        if size_bytes > 1024 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("disk ring requires {size_bytes} bytes, above the 1073741824-byte limit"),
            ));
        }

        Self::start_inner(
            root,
            place,
            shape,
            RingLayout::Disks(capacity),
            start_continuous_ns,
        )
    }

    /// Reserves the system records after every optional source, within the live reader's
    /// mapping bound.
    pub fn start_with_system(
        root: &Path,
        place: &Place,
        shape: Shape,
        capacity: crate::ring::DictionaryCapacity,
        start_continuous_ns: u64,
    ) -> io::Result<StartedRecording> {
        let size_bytes = Writer::system_size_bytes(shape, capacity)?;
        if size_bytes > 1024 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("system ring requires {size_bytes} bytes, above the 1073741824-byte limit"),
            ));
        }

        Self::start_inner(
            root,
            place,
            shape,
            RingLayout::System(capacity),
            start_continuous_ns,
        )
    }

    /// Registered identity sources must fit the live reader's existing mapping bound.
    pub fn start_with_identity(
        root: &Path,
        place: &Place,
        shape: Shape,
        capacity: crate::ring::DictionaryCapacity,
        start_continuous_ns: u64,
    ) -> io::Result<StartedRecording> {
        let size_bytes = Writer::identity_size_bytes(shape, capacity)?;
        if size_bytes > 1024 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "identity ring requires {size_bytes} bytes, above the 1073741824-byte limit"
                ),
            ));
        }

        Self::start_inner(
            root,
            place,
            shape,
            RingLayout::Identity(capacity),
            start_continuous_ns,
        )
    }

    /// Battery extends the identity ring without changing its original layout or seal.
    pub fn start_with_battery(
        root: &Path,
        place: &Place,
        shape: Shape,
        capacity: crate::ring::DictionaryCapacity,
        start_continuous_ns: u64,
    ) -> io::Result<StartedRecording> {
        let size_bytes = Writer::battery_size_bytes(shape, capacity)?;
        if size_bytes > 1024 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "battery ring requires {size_bytes} bytes, above the 1073741824-byte limit"
                ),
            ));
        }

        Self::start_inner(
            root,
            place,
            shape,
            RingLayout::Battery(capacity),
            start_continuous_ns,
        )
    }

    /// Creates and registers one source in the required startup order.
    pub fn start(
        root: &Path,
        place: &Place,
        shape: Shape,
        start_continuous_ns: u64,
    ) -> io::Result<StartedRecording> {
        Self::start_inner(
            root,
            place,
            shape,
            RingLayout::Observed,
            start_continuous_ns,
        )
    }

    fn start_inner(
        root: &Path,
        place: &Place,
        shape: Shape,
        layout: RingLayout,
        start_continuous_ns: u64,
    ) -> io::Result<StartedRecording> {
        let locked = LockedPlace::acquire(place)?;

        let ownership = match Self::start_ownership(root) {
            Ok(ownership) => ownership,
            Err(error) => {
                let reclaim = error.kind() != io::ErrorKind::WouldBlock;
                return Self::start_unregistered(
                    locked,
                    shape,
                    start_continuous_ns,
                    error,
                    None,
                    reclaim,
                );
            }
        };

        let registration = match Self::prepare(root, place) {
            Ok(registration) => registration,
            Err(error) => {
                return Self::start_unregistered(
                    locked,
                    shape,
                    start_continuous_ns,
                    error,
                    Some(ownership),
                    true,
                );
            }
        };

        let mut writer = Self::start_inner_writer(
            locked,
            layout,
            shape,
            registration.identity(),
            start_continuous_ns,
        )?;

        writer.retain_recording_ownership(ownership);
        Ok(registration.start_registered(writer))
    }

    fn start_inner_writer(
        locked: LockedPlace<'_>,
        layout: RingLayout,
        shape: Shape,
        identity: &RunIdentity,
        start_continuous_ns: u64,
    ) -> io::Result<Writer> {
        match layout {
            RingLayout::Identity(capacity) => {
                locked.create_with_identity(shape, capacity, identity, start_continuous_ns)
            }
            RingLayout::Battery(capacity) => {
                locked.create_with_battery(shape, capacity, identity, start_continuous_ns)
            }
            RingLayout::Disks(capacity) => {
                locked.create_with_disks(shape, capacity, identity, start_continuous_ns)
            }
            RingLayout::System(capacity) => {
                locked.create_with_system(shape, capacity, identity, start_continuous_ns)
            }
            RingLayout::Observed => locked.create_recording(shape, identity, start_continuous_ns),
        }
    }

    fn start_registered(&self, writer: Writer) -> StartedRecording {
        let registered = self.register(writer.name());
        let announcement = match registered {
            Ok(()) => holder::announce(&self.root, self.identity.run).err(),
            Err(_) => None,
        };

        StartedRecording {
            writer,
            registered,
            announcement,
        }
    }

    fn start_ownership(root: &Path) -> io::Result<File> {
        create_private_dir(root)?;
        let ownership = FileLock::try_lock(&root.join("collector.lock"))?;
        holder::withdraw(root)?;
        ownership.file().try_clone()
    }

    fn start_unregistered(
        locked: LockedPlace<'_>,
        shape: Shape,
        start_continuous_ns: u64,
        error: io::Error,
        ownership: Option<File>,
        reclaim: bool,
    ) -> io::Result<StartedRecording> {
        let mut writer = locked.create_unregistered(shape, start_continuous_ns)?;
        if let Some(ownership) = ownership {
            writer.retain_recording_ownership(ownership);
        }
        if reclaim {
            writer.reclaim_unregistered();
        }
        Ok(StartedRecording {
            writer,
            registered: Err(error),
            announcement: None,
        })
    }

    fn prepare(root: &Path, place: &Place) -> io::Result<Self> {
        create_private_dir(root)?;
        let lock = FileLock::wait_short(&root.join("registration.lock"))?;
        let recording = recording_identity(root)?;
        let mut registry = Registry::read(root)?;
        registry.capture_previous(root, place, recording)?;
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

    fn register(&self, ring_name: &str) -> io::Result<()> {
        let lock = FileLock::wait_short(&self.root.join("registration.lock"))?;
        let mut registry = Registry::read(&self.root)?;
        registry.add(
            &self.root,
            RegisteredRun {
                identity: self.identity,
                ring_name: ring_name.to_owned(),
                collector_pid: std::process::id(),
            },
        )?;
        registry.write(&self.root)?;
        drop(lock);
        Ok(())
    }
}

/// Reads the one canonical 32-hex-digit identity and its single line ending.
pub fn read_identity(root: &Path) -> io::Result<Identity> {
    let mut path_buffer = [0; private::PATH_MAX];
    let path = private::join_path(root, "identity", &mut path_buffer)?;
    let mut file = open_private_file(path, false)?;
    let mut bytes = [0; 34];
    let mut length = 0;
    while length < bytes.len() {
        let read = file.read(&mut bytes[length..])?;
        if read == 0 {
            break;
        }
        length += read;
    }
    if length != 33 {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    if bytes[32] != b'\n' {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let text = std::str::from_utf8(&bytes[..32])
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    parse_identity(text)
}

/// Initializes only an absent identity, under the caller's registration lock.
pub fn recording_identity(root: &Path) -> io::Result<Identity> {
    match read_identity(root) {
        Ok(identity) => Ok(identity),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            recording_identity_check_new(root)?;
            let identity = random_identity()?;
            write_atomic(root, "identity", &format!("{}\n", identity_text(identity)))?;
            Ok(identity)
        }
        Err(error) => Err(error),
    }
}

fn recording_identity_check_new(root: &Path) -> io::Result<()> {
    for artifact in ["data", "commits", "runs", "retired"] {
        let mut buffer = [0; private::PATH_MAX];
        let path = private::join_path(root, artifact, &mut buffer)?;
        match std::fs::symlink_metadata(path) {
            Ok(_) => return Err(io::Error::from(io::ErrorKind::InvalidData)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
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
    observation_times_at(timebase, SystemTime::now())
}

fn observation_times_at(timebase: Timebase, wall: SystemTime) -> io::Result<ObservationTimes> {
    let wall = wall
        .duration_since(UNIX_EPOCH)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let wall_ns =
        u64::try_from(wall.as_nanos()).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
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
    private::atomic_write(root, name, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::discovery::tests::allocator_calls_in;

    #[test]
    fn canonical_identity_reads_and_refusals_allocate_nothing() {
        // Goal: every shared disk identity reader uses the same fixed boundary.
        // Method: count reads of the valid 33-byte form and an extra line ending.
        let root =
            std::env::temp_dir().join(format!("flightdeck-identity-fixed-{}", std::process::id()));
        create_private_dir(&root).unwrap();
        private::atomic_write(&root, "identity", b"00000000000000010000000000000002\n").unwrap();

        let valid = allocator_calls_in(|| {
            assert_eq!(Identity([1, 2]), read_identity(&root).unwrap());
        });
        private::atomic_write(&root, "identity", b"00000000000000010000000000000002\n\n").unwrap();
        let refused = allocator_calls_in(|| {
            assert_eq!(
                io::ErrorKind::InvalidData,
                read_identity(&root).unwrap_err().kind()
            );
        });

        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(0, valid);
        assert_eq!(0, refused);
    }

    #[test]
    fn refuses_a_wall_clock_before_the_epoch_without_allocating() {
        // Goal: a wall clock outside the stored range remains explicitly unavailable.
        // Method: inject a pre-epoch clock and count allocations on the refused snapshot.
        let timebase = Timebase::read().unwrap();
        let wall = UNIX_EPOCH - Duration::from_secs(1);

        let calls = allocator_calls_in(|| {
            assert_eq!(
                io::ErrorKind::InvalidData,
                observation_times_at(timebase, wall).unwrap_err().kind()
            );
        });

        assert_eq!(0, calls);
    }
}

#[cfg(test)]
mod activation_tests;
