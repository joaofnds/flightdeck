//! Bounded lifecycle health, independent of diagnostics and canonical history.
use std::ffi::{c_int, c_void};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{private, read_identity, recording_identity};
use crate::programs::{self, CreationError};
use crate::ring::{Identity, ProcessCreation};

const WORDS: usize = 128;
const BYTES: usize = WORDS * size_of::<AtomicU64>();
const MAGIC: u64 = 0x6664_6374_726c_0002;
const RECORD_WORDS: usize = 16;
const RECORD_START: usize = 16;
const STOP_WORD: usize = 5;
const _: () = assert!(RECORD_START + 3 * RECORD_WORDS <= WORDS);

/// The mapping never owns any worker's lifetime lock and never unlinks its backing.
pub struct Control {
    words: NonNull<AtomicU64>,
    writable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Controller,
    Collector,
    Persister,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u64)]
pub enum State {
    #[default]
    Absent = 0,
    Starting = 1,
    Running = 2,
    Retrying = 3,
    Failed = 4,
    Stopping = 5,
    Stopped = 6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Health {
    pub failure: Failure,
    pub pid: i32,
    pub creation: ProcessCreation,
    pub state: State,
    pub heartbeat_ns: u64,
    pub error: i32,
    pub retries: u64,
    pub next_attempt_ns: u64,
    pub run: Identity,
    pub publication: u64,
}

/// Why the controller last delayed or refused a worker launch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Failure {
    #[default]
    None,
    Launch,
    Operating,
    Resource,
    Configuration,
    Invariant,
    Ownership,
}

pub struct Reporter {
    control: Control,
    role: Role,
    health: Health,
}

unsafe extern "C" {
    fn mmap(
        address: *mut c_void,
        length: usize,
        protection: c_int,
        flags: c_int,
        descriptor: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> c_int;
    fn mach_continuous_time() -> u64;
}

impl Control {
    /// Called under the controller lifetime lock before any managed worker is launched.
    pub fn create(root: &Path) -> io::Result<Self> {
        super::check_writer(root)?;
        let registration = private::FileLock::wait_short(&root.join("registration.lock"))?;
        let identity = recording_identity(root)?;
        drop(registration);
        let path = root.join("recorder.control");
        let previous = match Self::open_writable(root) {
            Ok(control) => Some(control),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidData
                ) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        if let Some(control) = previous {
            // Locks survive controller loss and remain authoritative even if a writer died
            // between the odd and even sequence stores in its health record.
            if !control.stopping()
                || private::FileLock::held(&root.join("collector.lock"))?
                || private::FileLock::held(&root.join("persister.lock"))?
            {
                return Ok(control);
            }
        }
        if private::FileLock::held(&root.join("collector.lock"))?
            || private::FileLock::held(&root.join("persister.lock"))?
        {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let authority = super::random_identity()?;
        let mut bytes = [0_u8; BYTES];
        let header = [
            MAGIC,
            identity.0[0],
            identity.0[1],
            authority.0[0],
            authority.0[1],
            0,
            super::BUILD,
        ];
        for (index, word) in header.iter().enumerate() {
            bytes[index * 8..(index + 1) * 8].copy_from_slice(&word.to_ne_bytes());
        }
        private::atomic_write(root, "recorder.control", &bytes)?;
        let control = Self::map(&private::open_private_file_writable(&path)?, true)?;
        control.validate(identity)?;
        Ok(control)
    }

    pub fn open(root: &Path) -> io::Result<Self> {
        let file = private::open_private_file(&root.join("recorder.control"), false)?;
        let control = Self::map(&file, false)?;
        control.validate(read_identity(root)?)?;
        Ok(control)
    }

    pub fn open_writable(root: &Path) -> io::Result<Self> {
        let control = Self::map(
            &private::open_private_file_writable(&root.join("recorder.control"))?,
            true,
        )?;
        control.validate(read_identity(root)?)?;
        Ok(control)
    }

    fn map(file: &std::fs::File, writable: bool) -> io::Result<Self> {
        if file.metadata()?.len() != u64::try_from(BYTES).map_err(io::Error::other)? {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let protection = if writable { 3 } else { 1 };
        let pointer = unsafe {
            mmap(
                std::ptr::null_mut(),
                BYTES,
                protection,
                1,
                file.as_raw_fd(),
                0,
            )
        };
        if pointer == std::ptr::with_exposed_provenance_mut(usize::MAX) {
            return Err(io::Error::last_os_error());
        }
        let words =
            NonNull::new(pointer.cast()).ok_or_else(|| io::Error::from(io::ErrorKind::Other))?;
        Ok(Self { words, writable })
    }

    fn validate(&self, identity: Identity) -> io::Result<()> {
        if self.word(0).load(Ordering::SeqCst) != MAGIC
            || self.word(1).load(Ordering::SeqCst) != identity.0[0]
            || self.word(2).load(Ordering::SeqCst) != identity.0[1]
            || self.word(6).load(Ordering::SeqCst) != super::BUILD
        {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        Ok(())
    }

    fn word(&self, index: usize) -> &AtomicU64 {
        assert!(index < WORDS);
        unsafe { &*self.words.as_ptr().add(index) }
    }

    #[must_use]
    pub fn authority(&self) -> Identity {
        Identity([
            self.word(3).load(Ordering::SeqCst),
            self.word(4).load(Ordering::SeqCst),
        ])
    }

    #[must_use]
    pub fn stopping(&self) -> bool {
        self.word(STOP_WORD).load(Ordering::SeqCst) != 0
    }

    pub fn request_stop(&self) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        self.word(STOP_WORD).store(1, Ordering::SeqCst);
        Ok(())
    }

    pub fn read(&self, role: Role) -> io::Result<Health> {
        let base = role.start();
        for _attempt in 0..8 {
            let sequence = self.word(base).load(Ordering::SeqCst);
            if !sequence.is_multiple_of(2) {
                continue;
            }
            let mut values = [0; 13];
            for (index, value) in values.iter_mut().enumerate() {
                *value = self.word(base + index + 1).load(Ordering::SeqCst);
            }
            if self.word(base).load(Ordering::SeqCst) != sequence {
                continue;
            }
            return Health::decode(&values);
        }
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }

    fn publish(&self, role: Role, health: &Health) {
        assert!(self.writable);
        let base = role.start();
        let before = self.word(base).load(Ordering::SeqCst);
        // A successor may recover the odd sequence left by a dead writer under its role lock.
        let Some(sequence) = before.checked_add(2 - before % 2) else {
            unreachable!("lifecycle sequence exhausted")
        };
        self.word(base).store(sequence - 1, Ordering::SeqCst);
        for (index, value) in health.encode().iter().enumerate() {
            self.word(base + index + 1).store(*value, Ordering::SeqCst);
        }
        self.word(base).store(sequence, Ordering::SeqCst);
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        if unsafe { munmap(self.words.as_ptr().cast(), BYTES) } != 0 {
            crate::diagnostics::error(format_args!(
                "recorder control unmap: {}",
                io::Error::last_os_error()
            ));
        }
    }
}

impl Role {
    fn start(self) -> usize {
        RECORD_START
            + match self {
                Self::Controller => 0,
                Self::Collector => RECORD_WORDS,
                Self::Persister => 2 * RECORD_WORDS,
            }
    }
}

impl Health {
    pub fn own() -> io::Result<Self> {
        let pid = crate::processes::own_pid()?;
        let creation = creation(pid)?.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        Ok(Self {
            failure: Failure::None,
            pid,
            creation,
            state: State::Starting,
            heartbeat_ns: now()?,
            error: 0,
            retries: 0,
            next_attempt_ns: 0,
            run: Identity([0; 2]),
            publication: 0,
        })
    }

    pub fn alive(&self) -> io::Result<bool> {
        if self.pid == 0 {
            return Ok(false);
        }
        Ok(creation(self.pid)?.is_some_and(|creation| creation == self.creation))
    }

    fn encode(&self) -> [u64; 13] {
        [
            u64::from(self.pid.cast_unsigned()),
            self.creation.unique_id,
            u64::from(self.creation.pid_version.cast_unsigned()),
            self.state.number(),
            self.heartbeat_ns,
            u64::from(self.error.cast_unsigned()),
            self.retries,
            self.next_attempt_ns,
            self.run.0[0],
            self.run.0[1],
            self.publication,
            super::BUILD,
            self.failure.number(),
        ]
    }

    fn decode(values: &[u64; 13]) -> io::Result<Self> {
        if values[0] != 0 && values[11] != super::BUILD {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let state = match values[3] {
            0 => State::Absent,
            1 => State::Starting,
            2 => State::Running,
            3 => State::Retrying,
            4 => State::Failed,
            5 => State::Stopping,
            6 => State::Stopped,
            _ => return Err(io::Error::from(io::ErrorKind::InvalidData)),
        };
        let narrow = |value| {
            u32::try_from(value)
                .map(u32::cast_signed)
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
        };
        Ok(Self {
            failure: Failure::decode(values[12])?,
            pid: narrow(values[0])?,
            creation: ProcessCreation {
                unique_id: values[1],
                pid_version: narrow(values[2])?,
            },
            state,
            heartbeat_ns: values[4],
            error: narrow(values[5])?,
            retries: values[6],
            next_attempt_ns: values[7],
            run: Identity([values[8], values[9]]),
            publication: values[10],
        })
    }
}

impl Reporter {
    /// The caller holds this role's lifetime lock before publishing or replacing its record.
    pub fn new(control: Control, role: Role) -> io::Result<Self> {
        let health = Health::own()?;
        control.publish(role, &health);
        Ok(Self {
            control,
            role,
            health,
        })
    }

    #[must_use]
    pub fn control(&self) -> &Control {
        &self.control
    }

    pub fn set_state(&mut self, state: State) -> io::Result<()> {
        self.health.state = state;
        self.refresh()
    }

    pub fn update(&mut self, state: State, run: Identity, publication: u64) -> io::Result<()> {
        self.health.state = state;
        self.health.run = run;
        self.health.publication = publication;
        self.refresh()
    }

    pub fn retry(
        &mut self,
        failure: Failure,
        error: i32,
        retries: u64,
        next_attempt_ns: u64,
    ) -> io::Result<()> {
        self.health.failure = failure;
        self.health.state = State::Retrying;
        self.health.error = error;
        self.health.retries = retries;
        self.health.next_attempt_ns = next_attempt_ns;
        self.refresh()
    }

    pub fn refresh(&mut self) -> io::Result<()> {
        self.health.heartbeat_ns = now()?;
        self.control.publish(self.role, &self.health);
        Ok(())
    }
}

pub fn now() -> io::Result<u64> {
    crate::clock::Timebase::read()?.nanoseconds(unsafe { mach_continuous_time() })
}

fn creation(pid: i32) -> io::Result<Option<ProcessCreation>> {
    let mut result = Err(CreationError::NotRead);
    programs::read_creation(pid, false, &mut result);
    match result {
        Ok(value) => Ok(Some(value)),
        Err(CreationError::Gone(_)) => Ok(None),
        Err(
            CreationError::Refused(code)
            | CreationError::Unsupported(code)
            | CreationError::Syscall(code),
        ) => Err(io::Error::from_raw_os_error(code)),
        Err(CreationError::Size(_) | CreationError::NotRead) => {
            Err(io::Error::from(io::ErrorKind::InvalidData))
        }
    }
}

impl State {
    fn number(self) -> u64 {
        match self {
            Self::Absent => 0,
            Self::Starting => 1,
            Self::Running => 2,
            Self::Retrying => 3,
            Self::Failed => 4,
            Self::Stopping => 5,
            Self::Stopped => 6,
        }
    }
}

impl Failure {
    fn number(self) -> u64 {
        match self {
            Self::None => 0,
            Self::Launch => 1,
            Self::Operating => 2,
            Self::Resource => 3,
            Self::Configuration => 4,
            Self::Invariant => 5,
            Self::Ownership => 6,
        }
    }

    fn decode(value: u64) -> io::Result<Self> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Launch),
            2 => Ok(Self::Operating),
            3 => Ok(Self::Resource),
            4 => Ok(Self::Configuration),
            5 => Ok(Self::Invariant),
            6 => Ok(Self::Ownership),
            _ => Err(io::Error::from(io::ErrorKind::InvalidData)),
        }
    }
}

impl Default for Health {
    fn default() -> Self {
        Self {
            failure: Failure::None,
            pid: 0,
            creation: ProcessCreation {
                unique_id: 0,
                pid_version: 0,
            },
            state: State::Absent,
            heartbeat_ns: 0,
            error: 0,
            retries: 0,
            next_attempt_ns: 0,
            run: Identity([0; 2]),
            publication: 0,
        }
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        if matches!(
            self.health.state,
            State::Starting | State::Running | State::Retrying | State::Stopping
        ) {
            self.health.state = State::Failed;
            self.control.publish(self.role, &self.health);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recording::discovery::tests::allocator_calls_in;
    use std::path::PathBuf;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "fd-control-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            private::create_private_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn publishes_exact_health_without_allocating_after_startup() {
        // Goal: the collector's structured status costs no continuing Rust allocation.
        // Method: publish/read a complete role record through independent mappings.
        let directory = Directory::new();
        let control = Control::create(&directory.0).unwrap();
        let mut reporter = Reporter::new(control, Role::Collector).unwrap();
        let reader = Control::open(&directory.0).unwrap();
        let calls = allocator_calls_in(|| {
            reporter
                .update(State::Running, Identity([11, 12]), 9_007_199_254_740_993)
                .unwrap();
            let health = reader.read(Role::Collector).unwrap();
            assert_eq!(health.run, Identity([11, 12]));
            assert_eq!(health.publication, 9_007_199_254_740_993);
            assert!(health.alive().unwrap());
        });
        assert_eq!(calls, 0);
    }

    #[test]
    fn refuses_torn_health_after_a_bounded_number_of_reads() {
        // Goal: a crashed writer cannot leave a reader spinning indefinitely.
        // Method: leave an odd publication sequence and read the role through production.
        let directory = Directory::new();
        let control = Control::create(&directory.0).unwrap();
        control
            .word(Role::Persister.start())
            .store(1, Ordering::SeqCst);
        assert_eq!(
            control.read(Role::Persister).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn repairs_interrupted_initialization_only_without_worker_owners() {
        // Goal: an initialization crash can recover without replacing live shared backing.
        // Method: leave an invalid private file, first with a role lock and then without it.
        let directory = Directory::new();
        recording_identity(&directory.0).unwrap();
        private::atomic_write(&directory.0, "recorder.control", &[0; BYTES]).unwrap();
        let owner = private::FileLock::try_lock(&directory.0.join("collector.lock")).unwrap();
        assert_eq!(
            Control::create(&directory.0).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(owner);
        let control = Control::create(&directory.0).unwrap();
        assert!(!control.stopping());
        assert_eq!(control.read(Role::Collector).unwrap().state, State::Absent);
    }

    #[test]
    fn an_old_stop_cannot_target_a_replacement_cohort() {
        // Goal: a stale client's mapped stop request cannot stop the next recording cohort.
        // Method: stop an empty cohort, renew it, then issue another stop on the old mapping.
        let directory = Directory::new();
        let old = Control::create(&directory.0).unwrap();
        old.request_stop().unwrap();
        let next = Control::create(&directory.0).unwrap();
        old.request_stop().unwrap();
        assert_ne!(old.authority(), next.authority());
        assert!(!next.stopping());
    }

    #[test]
    fn retains_authority_and_backing_for_a_surviving_worker() {
        // Goal: controller relaunch does not orphan the active worker's control mapping.
        // Method: keep a real process identity in the worker record while opening a controller.
        let directory = Directory::new();
        let control = Control::create(&directory.0).unwrap();
        let authority = control.authority();
        let _lock = private::FileLock::try_lock(&directory.0.join("collector.lock")).unwrap();
        let mut worker = Reporter::new(control, Role::Collector).unwrap();
        worker.update(State::Running, Identity([1, 2]), 3).unwrap();
        let replacement = Control::create(&directory.0).unwrap();
        replacement.request_stop().unwrap();
        assert_eq!(replacement.authority(), authority);
        assert!(worker.control().stopping());
    }

    #[test]
    fn a_read_only_observer_cannot_request_shutdown() {
        // Goal: health inspection alone grants no write mapping.
        // Method: open the reader API and try its explicit stop operation.
        let directory = Directory::new();
        let control = Control::create(&directory.0).unwrap();
        let reader = Control::open(&directory.0).unwrap();
        assert_eq!(
            reader.request_stop().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!control.stopping());
    }

    #[test]
    fn incompatible_protocol_or_build_cannot_replace_a_worker_owned_mapping() {
        // Goal: a live foreign cohort cannot be adopted or replaced through a matching PID.
        // Method: corrupt each compatibility field while the worker retains its role lock.
        for field in [0, 6] {
            let directory = Directory::new();
            let control = Control::create(&directory.0).unwrap();
            let authority = control.authority();
            let _owner = private::FileLock::try_lock(&directory.0.join("collector.lock")).unwrap();
            control.word(field).fetch_xor(1, Ordering::SeqCst);
            assert_eq!(
                Control::open(&directory.0).err().unwrap().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                Control::create(&directory.0).err().unwrap().kind(),
                io::ErrorKind::WouldBlock
            );
            assert_eq!(control.authority(), authority);
            assert!(!control.stopping());
        }
    }

    #[test]
    fn a_role_record_must_match_the_worker_cohort_build() {
        // Goal: coherent health from a different worker build never establishes adoption.
        // Method: keep a valid control header while changing only the role's build word.
        let directory = Directory::new();
        let control = Control::create(&directory.0).unwrap();
        let _owner = private::FileLock::try_lock(&directory.0.join("collector.lock")).unwrap();
        let worker = Reporter::new(control, Role::Collector).unwrap();
        worker
            .control()
            .word(Role::Collector.start() + 12)
            .fetch_xor(1, Ordering::SeqCst);
        assert_eq!(
            worker.control().read(Role::Collector).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
