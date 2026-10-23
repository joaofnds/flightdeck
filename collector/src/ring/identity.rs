//! Immutable run dictionary and independently available process identity.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use super::layout::{self, Layout};
use crate::u64_from_usize;

/// A creation stays the same across exec; PID version changes across exec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessCreation {
    pub unique_id: u64,
    pub pid_version: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IdentityAvailability {
    Measured = 0,
    Refused = 1,
    Gone = 2,
    Unsupported = 3,
    Changed = 4,
    Invalid = 5,
    Exhausted = 6,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UsageAvailability {
    Measured = 0,
    Refused = 1,
    Gone = 2,
    NotApplicable = 3,
}

/// A key belongs to its exact creating writer or decoding reader handle.
/// Copy bytes through that same reader before dropping it. Reopening the same ring
/// assigns a new owner; references never transfer between handles or processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramReference {
    generation: u64,
    identity: [u64; 6],
    owner: u64,
    number: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityObservation {
    pub pid: i32,
    pub usage: UsageAvailability,
    pub identity: ProcessIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessIdentity {
    Measured {
        creation: ProcessCreation,
        program: ProgramReference,
    },
    Unavailable {
        creation: Option<ProcessCreation>,
        reason: IdentityUnavailable,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IdentityUnavailable {
    Refused = 1,
    Gone = 2,
    Unsupported = 3,
    Changed = 4,
    Invalid = 5,
    Exhausted = 6,
}

impl IdentityObservation {
    pub const EMPTY: Self = Self {
        pid: 0,
        usage: UsageAvailability::NotApplicable,
        identity: ProcessIdentity::Unavailable {
            creation: None,
            reason: IdentityUnavailable::Unsupported,
        },
    };

    #[must_use]
    pub fn creation(&self) -> Option<&ProcessCreation> {
        match &self.identity {
            ProcessIdentity::Measured { creation, .. } => Some(creation),
            ProcessIdentity::Unavailable { creation, .. } => creation.as_ref(),
        }
    }

    #[must_use]
    pub fn program(&self) -> Option<&ProgramReference> {
        match &self.identity {
            ProcessIdentity::Measured { program, .. } => Some(program),
            ProcessIdentity::Unavailable { .. } => None,
        }
    }

    #[must_use]
    pub fn availability(&self) -> IdentityAvailability {
        match self.identity {
            ProcessIdentity::Measured { .. } => IdentityAvailability::Measured,
            ProcessIdentity::Unavailable { reason, .. } => match reason {
                IdentityUnavailable::Refused => IdentityAvailability::Refused,
                IdentityUnavailable::Gone => IdentityAvailability::Gone,
                IdentityUnavailable::Unsupported => IdentityAvailability::Unsupported,
                IdentityUnavailable::Changed => IdentityAvailability::Changed,
                IdentityUnavailable::Invalid => IdentityAvailability::Invalid,
                IdentityUnavailable::Exhausted => IdentityAvailability::Exhausted,
            },
        }
    }
}

impl ProgramReference {
    /// Empty output storage; no dictionary entry can have this reference.
    const EMPTY: Self = Self {
        generation: 0,
        identity: [0; 6],
        owner: 0,
        number: 0,
    };

    /// The entry's number in its run's dictionary, which counts from 1 in the order the
    /// collector added the entries.
    #[must_use]
    pub fn number(&self) -> u64 {
        self.number
    }
}

pub const PROGRAM_PATH_BYTES_MAX: usize = 4095;

/// Names are original bytes. Bundle prefixes stop at the outermost .app component.
pub struct ProgramBytes<'bytes> {
    pub executable_name: &'bytes [u8],
    pub app_path: Option<&'bytes [u8]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramBytesError {
    ExecutableBytes,
    ExecutableBasename,
    AppBytes,
    AppPath,
    AppOutermost,
}

/// Caller-owned storage keeps the two byte destinations unambiguous.
pub struct ProgramBuffers<'buffers> {
    pub executable_name: &'buffers mut [u8; PROGRAM_PATH_BYTES_MAX],
    pub app_path: &'buffers mut [u8; PROGRAM_PATH_BYTES_MAX],
}

impl ProgramBytes<'_> {
    /// Validates the retained byte contract shared by live and saved identity readers.
    pub fn validate(&self) -> Result<(), ProgramBytesError> {
        if self.executable_name.is_empty() {
            return Err(ProgramBytesError::ExecutableBytes);
        }

        if self.executable_name.len() > PROGRAM_PATH_BYTES_MAX {
            return Err(ProgramBytesError::ExecutableBytes);
        }

        if self.executable_name.contains(&0) {
            return Err(ProgramBytesError::ExecutableBytes);
        }

        if self.executable_name.contains(&b'/') {
            return Err(ProgramBytesError::ExecutableBasename);
        }

        if let Some(path) = self.app_path {
            if path.len() > PROGRAM_PATH_BYTES_MAX {
                return Err(ProgramBytesError::AppBytes);
            }

            if !path.starts_with(b"/") {
                return Err(ProgramBytesError::AppPath);
            }

            if !path.ends_with(b".app") {
                return Err(ProgramBytesError::AppPath);
            }

            if path.contains(&0) {
                return Err(ProgramBytesError::AppBytes);
            }

            for component in path.rsplit(|byte| *byte == b'/').skip(1) {
                if component.ends_with(b".app") {
                    return Err(ProgramBytesError::AppOutermost);
                }
            }
        }

        Ok(())
    }
}

/// Identity records are independent of numeric rows, including refused usage.
/// Process rows use strictly increasing PID order. Measured rows match numeric process
/// order exactly; unavailable usage rows have no numeric Process counterpart.
pub struct IdentityPublication<'records> {
    pub processes: &'records [IdentityObservation],
    pub events: &'records [IdentityObservation],
}

/// Run dictionary occupancy; arena bytes include zero alignment padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DictionaryUsage {
    pub entries: u64,
    pub arena_bytes: u64,
}

pub(super) struct DictionaryIndex {
    pub(super) entries: Vec<u64>,
    pub(super) owner: u64,
}

impl DictionaryIndex {
    pub(super) fn initialize(&mut self, capacity: usize, owner: u64) {
        assert!(capacity > 0);
        assert!(capacity <= 65_536);
        assert!(owner > 0);
        assert!(self.entries.is_empty());
        assert_eq!(self.owner, 0);

        self.entries = vec![0; capacity * 2];
        self.owner = owner;
    }

    pub(super) fn intern(
        &mut self,
        words: &[AtomicU64],
        layout: &Layout,
        bytes: &ProgramBytes,
        program: &mut Option<ProgramReference>,
    ) -> Result<(), IdentityAvailability> {
        if bytes.validate().is_err() {
            return Err(IdentityAvailability::Invalid);
        }

        self.intern_hashed(words, layout, bytes, intern_hash(bytes), program)
    }

    fn intern_hashed(
        &mut self,
        words: &[AtomicU64],
        layout: &Layout,
        bytes: &ProgramBytes,
        hash: u64,
        program: &mut Option<ProgramReference>,
    ) -> Result<(), IdentityAvailability> {
        let first = usize::try_from(hash % u64_from_usize(self.entries.len()))
            .map_err(|_| IdentityAvailability::Invalid)?;

        for probe in 0..self.entries.len() {
            let index = (first + probe) % self.entries.len();
            let mut number = self.entries[index];
            if number == 0 {
                number = intern_hashed_insert(words, layout, bytes, hash)?;
                self.entries[index] = number;
            } else {
                if !intern_hashed_matches(words, layout, &DictionaryMatch { number, hash }, bytes)?
                {
                    continue;
                }
            }

            let retained = program.insert(ProgramReference::EMPTY);
            reference(
                words,
                &ReferencePosition {
                    number,
                    owner: self.owner,
                },
                retained,
            );

            return Ok(());
        }

        Err(IdentityAvailability::Exhausted)
    }
}

fn intern_hash(bytes: &ProgramBytes) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;

    for byte in bytes
        .executable_name
        .iter()
        .chain(bytes.app_path.unwrap_or(&[]).iter())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }

    hash ^ u64_from_usize(bytes.executable_name.len())
}

fn intern_hashed_insert(
    words: &[AtomicU64],
    layout: &Layout,
    bytes: &ProgramBytes,
    hash: u64,
) -> Result<u64, IdentityAvailability> {
    let count = load(words, layout::HEADER_DICTIONARY_COUNT);
    let used = load(words, layout::HEADER_ARENA_USED);
    let app = bytes.app_path.unwrap_or(&[]);
    let size = (bytes.executable_name.len() + app.len()).div_ceil(8) * 8;
    if count >= u64_from_usize(layout.dictionary.capacity) {
        return Err(IdentityAvailability::Exhausted);
    }

    if used + u64_from_usize(size) > u64_from_usize(layout.arena_bytes) {
        return Err(IdentityAvailability::Exhausted);
    }

    let offset = usize::try_from(used).map_err(|_| IdentityAvailability::Invalid)?;

    intern_hashed_insert_store_bytes(words, layout.arena_start + offset / 8, bytes);

    let index = usize::try_from(count).map_err(|_| IdentityAvailability::Invalid)?;
    let descriptor = &words[layout.dictionary.record(index)];

    for (word, value) in descriptor.iter().zip([
        used,
        u64_from_usize(bytes.executable_name.len()),
        u64_from_usize(app.len()),
        hash,
    ]) {
        word.store(value, Ordering::Relaxed);
    }

    words[layout::HEADER_ARENA_USED].store(used + u64_from_usize(size), Ordering::Relaxed);
    // Events publish before slots; every referenced entry is committed before either release.
    words[layout::HEADER_DICTIONARY_COUNT].store(count + 1, Ordering::Release);

    Ok(count + 1)
}

fn intern_hashed_insert_store_bytes(words: &[AtomicU64], start: usize, bytes: &ProgramBytes) {
    let app = bytes.app_path.unwrap_or(&[]);
    let count = bytes.executable_name.len() + app.len();

    for index in 0..count.div_ceil(8) {
        let mut packed = [0; 8];
        for (part, byte) in packed.iter_mut().enumerate() {
            let offset = index * 8 + part;
            if offset < bytes.executable_name.len() {
                *byte = bytes.executable_name[offset];
            } else {
                if offset < count {
                    *byte = app[offset - bytes.executable_name.len()];
                }
            }
        }
        words[start + index].store(u64::from_le_bytes(packed), Ordering::Relaxed);
    }
}

fn intern_hashed_matches(
    words: &[AtomicU64],
    layout: &Layout,
    candidate: &DictionaryMatch,
    bytes: &ProgramBytes,
) -> Result<bool, IdentityAvailability> {
    let mut entry = Descriptor::EMPTY;
    descriptor(words, layout, candidate.number, &mut entry)
        .map_err(|_| IdentityAvailability::Invalid)?;

    if entry.hash != candidate.hash {
        return Ok(false);
    }

    if entry.executable_name_bytes != bytes.executable_name.len() {
        return Ok(false);
    }

    if entry.app_path_bytes != bytes.app_path.unwrap_or(&[]).len() {
        return Ok(false);
    }

    Ok(bytes
        .executable_name
        .iter()
        .chain(bytes.app_path.unwrap_or(&[]).iter())
        .enumerate()
        .all(|(index, byte)| arena_byte(words, layout, entry.offset + index) == *byte))
}

struct Descriptor {
    offset: usize,
    executable_name_bytes: usize,
    app_path_bytes: usize,
    hash: u64,
}

impl Descriptor {
    const EMPTY: Self = Self {
        offset: 0,
        executable_name_bytes: 0,
        app_path_bytes: 0,
        hash: 0,
    };
}

struct DictionaryMatch {
    number: u64,
    hash: u64,
}

struct ReferencePosition {
    number: u64,
    owner: u64,
}

fn descriptor(
    words: &[AtomicU64],
    layout: &Layout,
    number: u64,
    output: &mut Descriptor,
) -> io::Result<()> {
    let Some(usage) = dictionary_usage(words, layout)? else {
        return invalid();
    };

    if number == 0 {
        return invalid();
    }

    if number > usage.entries {
        return invalid();
    }

    let index = usize::try_from(number - 1).map_err(|_| invalid_error())?;
    let values = &words[layout.dictionary.record(index)];
    let offset = load(values, 0);
    let executable_name_bytes = load(values, 1);
    let app_path_bytes = load(values, 2);

    if executable_name_bytes == 0 {
        return invalid();
    }

    if executable_name_bytes > u64_from_usize(PROGRAM_PATH_BYTES_MAX) {
        return invalid();
    }

    if app_path_bytes > u64_from_usize(PROGRAM_PATH_BYTES_MAX) {
        return invalid();
    }

    if !offset.is_multiple_of(8) {
        return invalid();
    }

    let Some(end) = offset
        .checked_add(executable_name_bytes)
        .and_then(|end| end.checked_add(app_path_bytes))
    else {
        return invalid();
    };

    if end > usage.arena_bytes {
        return invalid();
    }

    output.offset = usize::try_from(offset).map_err(|_| invalid_error())?;
    output.executable_name_bytes =
        usize::try_from(executable_name_bytes).map_err(|_| invalid_error())?;
    output.app_path_bytes = usize::try_from(app_path_bytes).map_err(|_| invalid_error())?;
    output.hash = load(values, 3);

    Ok(())
}

fn arena_byte(words: &[AtomicU64], layout: &Layout, offset: usize) -> u8 {
    load(words, layout.arena_start + offset / 8).to_le_bytes()[offset % 8]
}

fn reference(words: &[AtomicU64], position: &ReferencePosition, output: &mut ProgramReference) {
    output.generation = load(words, layout::HEADER_GENERATION);
    output.identity = std::array::from_fn(|index| load(words, layout::HEADER_IDENTITY + index));
    output.owner = position.owner;
    output.number = position.number;
}

pub(super) fn dictionary_usage(
    words: &[AtomicU64],
    layout: &Layout,
) -> io::Result<Option<DictionaryUsage>> {
    if layout.version != layout::LAYOUT_VERSION_IDENTITY {
        return Ok(None);
    }

    let entries = words[layout::HEADER_DICTIONARY_COUNT].load(Ordering::Acquire);
    let arena_bytes = load(words, layout::HEADER_ARENA_USED);

    if entries > u64_from_usize(layout.dictionary.capacity) {
        return invalid();
    }

    if arena_bytes > u64_from_usize(layout.arena_bytes) {
        return invalid();
    }

    if !arena_bytes.is_multiple_of(8) {
        return invalid();
    }

    Ok(Some(DictionaryUsage {
        entries,
        arena_bytes,
    }))
}

pub(super) fn copy_program(
    words: &[AtomicU64],
    layout: &Layout,
    program: &ProgramReference,
    owner: u64,
    buffers: &mut ProgramBuffers,
) -> io::Result<(usize, usize)> {
    let mut expected = ProgramReference::EMPTY;
    reference(
        words,
        &ReferencePosition {
            number: program.number,
            owner,
        },
        &mut expected,
    );

    if *program != expected {
        return invalid();
    }

    let mut entry = Descriptor::EMPTY;
    descriptor(words, layout, program.number, &mut entry)?;

    for (index, byte) in buffers.executable_name[..entry.executable_name_bytes]
        .iter_mut()
        .enumerate()
    {
        *byte = arena_byte(words, layout, entry.offset + index);
    }

    for (index, byte) in buffers.app_path[..entry.app_path_bytes]
        .iter_mut()
        .enumerate()
    {
        *byte = arena_byte(
            words,
            layout,
            entry.offset + entry.executable_name_bytes + index,
        );
    }

    ProgramBytes {
        executable_name: &buffers.executable_name[..entry.executable_name_bytes],
        app_path: if entry.app_path_bytes == 0 {
            None
        } else {
            Some(&buffers.app_path[..entry.app_path_bytes])
        },
    }
    .validate()
    .map_err(|_| invalid_error())?;

    Ok((entry.executable_name_bytes, entry.app_path_bytes))
}

pub(super) fn store_observation(record: &[AtomicU64], value: &IdentityObservation) {
    assert_eq!(record.len(), layout::PROCESS_IDENTITY_WORDS);

    let creation = value.creation();
    let words = [
        u64::from(value.pid.cast_unsigned()),
        creation.map_or(0, |creation| creation.unique_id),
        creation.map_or(0, |creation| {
            u64::from(creation.pid_version.cast_unsigned())
        }),
        match value.availability() {
            IdentityAvailability::Measured => 0,
            IdentityAvailability::Refused => 1,
            IdentityAvailability::Gone => 2,
            IdentityAvailability::Unsupported => 3,
            IdentityAvailability::Changed => 4,
            IdentityAvailability::Invalid => 5,
            IdentityAvailability::Exhausted => 6,
        } | if creation.is_some() { 256 } else { 0 },
        match value.usage {
            UsageAvailability::Measured => 0,
            UsageAvailability::Refused => 1,
            UsageAvailability::Gone => 2,
            UsageAvailability::NotApplicable => 3,
        },
        value.program().map_or(0, |program| program.number),
    ];

    for (word, value) in record.iter().zip(words) {
        word.store(value, Ordering::Relaxed);
    }
}

pub(super) struct IdentityRecord<'words> {
    pub words: &'words [AtomicU64],
    pub record: &'words [AtomicU64],
}

pub(super) fn load_observation(
    source: &IdentityRecord,
    layout: &Layout,
    owner: u64,
    buffers: &mut ProgramBuffers,
    output: &mut IdentityObservation,
) -> io::Result<()> {
    assert_eq!(source.record.len(), layout::PROCESS_IDENTITY_WORDS);

    let pid = load_observation_signed(load(source.record, 0))?;
    let version = load_observation_signed(load(source.record, 2))?;
    let flags = load(source.record, 3);

    if flags & !0x107 != 0 {
        return invalid();
    }

    let mut creation = None;
    load_observation_creation(source.record, flags, version, &mut creation)?;
    let usage = match load(source.record, 4) {
        0 => UsageAvailability::Measured,
        1 => UsageAvailability::Refused,
        2 => UsageAvailability::Gone,
        3 => UsageAvailability::NotApplicable,
        _ => return invalid(),
    };
    let number = load(source.record, 5);
    let availability = flags & 7;

    if availability == 0 {
        let Some(creation) = creation else {
            return invalid();
        };

        let mut program = ProgramReference::EMPTY;
        reference(
            source.words,
            &ReferencePosition { number, owner },
            &mut program,
        );
        validate_program(source.words, layout, &program, owner, buffers)?;

        output.identity = ProcessIdentity::Measured { creation, program };
    } else {
        if number != 0 {
            return invalid();
        }

        let reason = match availability {
            1 => IdentityUnavailable::Refused,
            2 => IdentityUnavailable::Gone,
            3 => IdentityUnavailable::Unsupported,
            4 => IdentityUnavailable::Changed,
            5 => IdentityUnavailable::Invalid,
            6 => IdentityUnavailable::Exhausted,
            _ => return invalid(),
        };

        output.identity = ProcessIdentity::Unavailable { creation, reason };
    }

    output.pid = pid;
    output.usage = usage;

    Ok(())
}

fn load_observation_signed(value: u64) -> io::Result<i32> {
    Ok(i32::from_ne_bytes(
        u32::try_from(value)
            .map_err(|_| invalid_error())?
            .to_ne_bytes(),
    ))
}

fn load_observation_creation(
    record: &[AtomicU64],
    flags: u64,
    version: i32,
    output: &mut Option<ProcessCreation>,
) -> io::Result<()> {
    if flags & 256 == 0 {
        if load(record, 1) != 0 {
            return invalid();
        }

        if version != 0 {
            return invalid();
        }

        *output = None;
    } else {
        *output = Some(ProcessCreation {
            unique_id: load(record, 1),
            pid_version: version,
        });
    }

    Ok(())
}

/// Copies the program's bytes into `buffers` and validates them. A caller passes one pair of
/// buffers for all the programs it validates, since zeroing a pair takes longer than copying a
/// program into it.
fn validate_program(
    words: &[AtomicU64],
    layout: &Layout,
    program: &ProgramReference,
    owner: u64,
    buffers: &mut ProgramBuffers,
) -> io::Result<()> {
    copy_program(words, layout, program, owner, buffers)?;

    Ok(())
}

pub(super) fn validate_observation(
    words: &[AtomicU64],
    layout: &Layout,
    value: &IdentityObservation,
    owner: u64,
    buffers: &mut ProgramBuffers,
) -> io::Result<()> {
    if let Some(program) = value.program() {
        validate_program(words, layout, program, owner, buffers)?;
    }

    Ok(())
}

pub(super) fn legacy(pid: i32, usage: UsageAvailability, output: &mut IdentityObservation) {
    output.pid = pid;
    output.usage = usage;
    output.identity = ProcessIdentity::Unavailable {
        creation: None,
        reason: IdentityUnavailable::Unsupported,
    };
}

fn load(words: &[AtomicU64], index: usize) -> u64 {
    words[index].load(Ordering::Relaxed)
}

fn invalid<T>() -> io::Result<T> {
    Err(invalid_error())
}

fn invalid_error() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

pub(super) fn event_pid(event: &crate::tracker::Event) -> i32 {
    match event {
        crate::tracker::Event::Start { pid, .. }
        | crate::tracker::Event::Exec { pid }
        | crate::tracker::Event::Exit { pid, .. } => *pid,
    }
}

fn event_usage(event: &crate::tracker::Event) -> UsageAvailability {
    match event {
        crate::tracker::Event::Exit { last: Ok(_), .. } => UsageAvailability::Measured,
        crate::tracker::Event::Exit {
            last: Err(crate::processes::Unreadable::Refused),
            ..
        } => UsageAvailability::Refused,
        crate::tracker::Event::Exit {
            last: Err(crate::processes::Unreadable::Gone),
            ..
        } => UsageAvailability::Gone,
        _ => UsageAvailability::NotApplicable,
    }
}

pub(super) fn store_event_identity(record: &[AtomicU64], value: &IdentityObservation) {
    let packed = [const { AtomicU64::new(0) }; 6];

    store_observation(&packed, value);

    for (target, source) in record[layout::EVENT_WORDS..].iter().zip(&packed[1..]) {
        target.store(source.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}

pub(super) fn load_event_identity(
    source: &IdentityRecord,
    layout: &Layout,
    event: &crate::tracker::Event,
    owner: u64,
    output: &mut IdentityObservation,
) -> io::Result<()> {
    if layout.version != layout::LAYOUT_VERSION_IDENTITY {
        legacy(event_pid(event), event_usage(event), output);
        return Ok(());
    }

    let packed = [const { AtomicU64::new(0) }; 6];
    packed[0].store(
        u64::from(event_pid(event).cast_unsigned()),
        Ordering::Relaxed,
    );
    for (target, source) in packed[1..]
        .iter()
        .zip(&source.record[layout::EVENT_WORDS..])
    {
        target.store(source.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    let mut executable_name = [0; PROGRAM_PATH_BYTES_MAX];
    let mut app_path = [0; PROGRAM_PATH_BYTES_MAX];
    load_observation(
        &IdentityRecord {
            words: source.words,
            record: &packed,
        },
        layout,
        owner,
        &mut ProgramBuffers {
            executable_name: &mut executable_name,
            app_path: &mut app_path,
        },
        output,
    )?;

    if output.usage != event_usage(event) {
        return invalid();
    }

    Ok(())
}

pub(super) fn store_processes(
    slot: &[AtomicU64],
    layout: &Layout,
    identities: &[IdentityObservation],
) {
    slot[layout::SLOT_IDENTITY_COUNT].store(u64_from_usize(identities.len()), Ordering::Relaxed);

    for (index, value) in identities.iter().enumerate() {
        store_observation(&slot[layout.process_identities.record(index)], value);
    }
}

pub(super) fn load_processes(
    source: &IdentityRecord,
    layout: &Layout,
    tick: &super::Tick,
    identities: &mut Vec<IdentityObservation>,
    owner: u64,
) -> io::Result<()> {
    identities.clear();

    if layout.version != layout::LAYOUT_VERSION_IDENTITY {
        for process in &tick.processes {
            let mut value = IdentityObservation::EMPTY;
            legacy(process.pid, UsageAvailability::Measured, &mut value);
            crate::push_without_growing(identities, value);
        }

        return Ok(());
    }

    let count = usize::try_from(load(source.record, layout::SLOT_IDENTITY_COUNT))
        .map_err(|_| invalid_error())?;
    if count > layout.process_identities.capacity {
        return invalid();
    }

    let mut executable_name = [0; PROGRAM_PATH_BYTES_MAX];
    let mut app_path = [0; PROGRAM_PATH_BYTES_MAX];
    for index in 0..count {
        let mut value = IdentityObservation::EMPTY;
        load_observation(
            &IdentityRecord {
                words: source.words,
                record: &source.record[layout.process_identities.record(index)],
            },
            layout,
            owner,
            &mut ProgramBuffers {
                executable_name: &mut executable_name,
                app_path: &mut app_path,
            },
            &mut value,
        )?;
        crate::push_without_growing(identities, value);
    }

    validate_process_rows(&tick.processes, identities)
}

fn validate_process_rows(
    processes: &[crate::processes::Process],
    identities: &[IdentityObservation],
) -> io::Result<()> {
    let mut measured = 0;
    let mut previous = None;

    for value in identities {
        if let Some(previous) = previous {
            if value.pid <= previous {
                return invalid();
            }
        }

        previous = Some(value.pid);
        match value.usage {
            UsageAvailability::Measured => {
                let Some(process) = processes.get(measured) else {
                    return invalid();
                };

                if process.pid != value.pid {
                    return invalid();
                }

                measured += 1;
            }
            UsageAvailability::NotApplicable => return invalid(),
            UsageAvailability::Refused | UsageAvailability::Gone => {}
        }
    }

    if measured != processes.len() {
        return invalid();
    }

    Ok(())
}

pub(super) fn validate_publication(
    words: &[AtomicU64],
    layout: &Layout,
    readings: &super::Readings,
    identities: &IdentityPublication,
    owner: u64,
) -> io::Result<()> {
    if layout.version != layout::LAYOUT_VERSION_IDENTITY {
        return invalid();
    }

    if identities.processes.len() > layout.process_identities.capacity {
        return invalid();
    }

    if identities.events.len() != readings.events.len() {
        return invalid();
    }

    validate_process_rows(readings.processes, identities.processes)?;

    let mut executable_name = [0; PROGRAM_PATH_BYTES_MAX];
    let mut app_path = [0; PROGRAM_PATH_BYTES_MAX];
    for value in identities.processes {
        validate_observation(
            words,
            layout,
            value,
            owner,
            &mut ProgramBuffers {
                executable_name: &mut executable_name,
                app_path: &mut app_path,
            },
        )?;
    }

    for (value, event) in identities.events.iter().zip(readings.events) {
        if value.pid != event_pid(event) {
            return invalid();
        }

        if value.usage != event_usage(event) {
            return invalid();
        }

        validate_observation(
            words,
            layout,
            value,
            owner,
            &mut ProgramBuffers {
                executable_name: &mut executable_name,
                app_path: &mut app_path,
            },
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::{DictionaryCapacity, Shape};

    fn dictionary(capacity: DictionaryCapacity) -> (Layout, Vec<AtomicU64>, DictionaryIndex) {
        let layout = Layout::identity(
            Shape {
                slot_count: 1,
                cpu_capacity: 1,
                interface_capacity: 1,
                core_capacity: 1,
                state_capacity: 1,
                fan_capacity: 1,
                process_capacity: 1,
                coalition_capacity: 1,
                event_capacity: 1,
            },
            capacity,
        )
        .unwrap();
        let words: Vec<_> = (0..layout.word_count())
            .map(|_| AtomicU64::new(0))
            .collect();
        words[layout::HEADER_GENERATION].store(7, Ordering::Relaxed);
        let mut index = DictionaryIndex {
            entries: Vec::new(),
            owner: 0,
        };
        index.initialize(layout.dictionary.capacity, 1);
        (layout, words, index)
    }

    fn observed(program: &ProgramReference) -> IdentityObservation {
        IdentityObservation {
            pid: 1,
            usage: UsageAvailability::Refused,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: 0,
                    pid_version: -1,
                },
                program: *program,
            },
        }
    }

    #[test]
    fn retains_raw_names_and_explicit_zero_creation_after_roundtrip() {
        // Goal: raw byte keys and creation zero values survive without sentinel inference.
        // Method: intern bytes, store a refused-usage identity, decode and copy its bytes.
        let (layout, words, mut index) = dictionary(DictionaryCapacity {
            entries: 2,
            arena_bytes: 64,
        });
        let bytes = ProgramBytes {
            executable_name: b"helper\xff",
            app_path: Some(b"/A.app"),
        };
        let mut program = None;
        index.intern(&words, &layout, &bytes, &mut program).unwrap();
        let program = program.unwrap();
        let value = observed(&program);
        let record = [const { AtomicU64::new(0) }; 6];
        store_observation(&record, &value);

        let mut loaded = IdentityObservation::EMPTY;
        let mut executable_name = [0; PROGRAM_PATH_BYTES_MAX];
        let mut app_path = [0; PROGRAM_PATH_BYTES_MAX];
        load_observation(
            &IdentityRecord {
                words: &words,
                record: &record,
            },
            &layout,
            1,
            &mut ProgramBuffers {
                executable_name: &mut executable_name,
                app_path: &mut app_path,
            },
            &mut loaded,
        )
        .unwrap();
        let mut name = [0; PROGRAM_PATH_BYTES_MAX];
        let mut app = [0; PROGRAM_PATH_BYTES_MAX];
        let lengths = copy_program(
            &words,
            &layout,
            &program,
            1,
            &mut ProgramBuffers {
                executable_name: &mut name,
                app_path: &mut app,
            },
        )
        .unwrap();

        assert_eq!(value, loaded);
        assert_eq!(b"helper\xff", &name[..lengths.0]);
        assert_eq!(b"/A.app", &app[..lengths.1]);
    }

    #[test]
    fn compares_original_bytes_even_when_hashes_collide() {
        // Goal: hash collision never assigns another executable's identity.
        // Method: force identical full hashes for two original byte strings.
        let (layout, words, mut index) = dictionary(DictionaryCapacity {
            entries: 2,
            arena_bytes: 64,
        });
        let first = ProgramBytes {
            executable_name: b"first",
            app_path: None,
        };
        let second = ProgramBytes {
            executable_name: b"other",
            app_path: None,
        };

        let mut one = None;
        index
            .intern_hashed(&words, &layout, &first, 9, &mut one)
            .unwrap();
        let one = one.unwrap();
        let mut two = None;
        index
            .intern_hashed(&words, &layout, &second, 9, &mut two)
            .unwrap();
        let two = two.unwrap();
        let mut repeated = None;
        index
            .intern_hashed(&words, &layout, &first, 9, &mut repeated)
            .unwrap();
        let repeated = repeated.unwrap();

        assert_ne!(one, two);
        assert_eq!(one, repeated);
    }

    #[test]
    fn refuses_a_corrupt_descriptor_instead_of_interning_a_duplicate() {
        let mut output = None;
        // Goal: dictionary corruption is reported without consuming another entry.
        // Method: damage the descriptor of a known key before interning it again.
        let (layout, words, mut index) = dictionary(DictionaryCapacity {
            entries: 2,
            arena_bytes: 64,
        });
        let bytes = ProgramBytes {
            executable_name: b"first",
            app_path: None,
        };
        index.intern(&words, &layout, &bytes, &mut output).unwrap();
        words[layout.dictionary.start + 1].store(0, Ordering::Relaxed);

        let result = index.intern(&words, &layout, &bytes, &mut output);

        assert_eq!(Err(IdentityAvailability::Invalid), result);
        assert_eq!(1, load(&words, layout::HEADER_DICTIONARY_COUNT));
    }

    #[test]
    fn retains_known_references_after_entry_exhaustion() {
        let mut output = None;
        // Goal: dictionary exhaustion refuses only previously unseen names.
        // Method: exhaust a one-entry dictionary, then repeat its known name.
        let (layout, words, mut index) = dictionary(DictionaryCapacity {
            entries: 1,
            arena_bytes: 64,
        });
        let first = ProgramBytes {
            executable_name: b"first",
            app_path: None,
        };
        let mut program = None;
        index.intern(&words, &layout, &first, &mut program).unwrap();
        let program = program.unwrap();

        let refused = index.intern(
            &words,
            &layout,
            &ProgramBytes {
                executable_name: b"other",
                app_path: None,
            },
            &mut output,
        );
        let mut repeated = None;
        index
            .intern(&words, &layout, &first, &mut repeated)
            .unwrap();
        let repeated = repeated.unwrap();

        assert_eq!(Err(IdentityAvailability::Exhausted), refused);
        assert_eq!(program, repeated);
    }

    #[test]
    fn retains_known_references_after_arena_exhaustion() {
        let mut output = None;
        // Goal: byte exhaustion does not invalidate already committed entries.
        // Method: fill the eight-byte arena with one key while entry capacity remains.
        let (layout, words, mut index) = dictionary(DictionaryCapacity {
            entries: 2,
            arena_bytes: 8,
        });
        let first = ProgramBytes {
            executable_name: b"first",
            app_path: None,
        };
        let mut program = None;
        index.intern(&words, &layout, &first, &mut program).unwrap();
        let program = program.unwrap();

        let refused = index.intern(
            &words,
            &layout,
            &ProgramBytes {
                executable_name: b"other",
                app_path: None,
            },
            &mut output,
        );
        let mut repeated = None;
        index
            .intern(&words, &layout, &first, &mut repeated)
            .unwrap();
        let repeated = repeated.unwrap();

        assert_eq!(Err(IdentityAvailability::Exhausted), refused);
        assert_eq!(program, repeated);
    }

    enum IdentityCorruption {
        AbsentCreation(usize),
        Observation { field: usize, value: u64 },
        Descriptor { field: usize, value: u64 },
    }

    fn refuses_corrupt_identity(corruption: &IdentityCorruption) {
        let (layout, words, mut index) = dictionary(DictionaryCapacity {
            entries: 1,
            arena_bytes: 64,
        });
        let mut program = None;
        index
            .intern(
                &words,
                &layout,
                &ProgramBytes {
                    executable_name: b"first",
                    app_path: None,
                },
                &mut program,
            )
            .unwrap();
        let program = program.unwrap();
        let record = [const { AtomicU64::new(0) }; 6];
        store_observation(&record, &observed(&program));
        match corruption {
            IdentityCorruption::AbsentCreation(field) => {
                record[1].store(0, Ordering::Relaxed);
                record[2].store(0, Ordering::Relaxed);
                record[3].store(1, Ordering::Relaxed);
                record[5].store(0, Ordering::Relaxed);
                record[*field].store(1, Ordering::Relaxed);
            }
            IdentityCorruption::Observation { field, value } => {
                record[*field].store(*value, Ordering::Relaxed);
            }
            IdentityCorruption::Descriptor { field, value } => {
                words[layout.dictionary.start + field].store(*value, Ordering::Relaxed);
            }
        }
        let mut observation = IdentityObservation::EMPTY;
        let mut executable_name = [0; PROGRAM_PATH_BYTES_MAX];
        let mut app_path = [0; PROGRAM_PATH_BYTES_MAX];

        let result = load_observation(
            &IdentityRecord {
                words: &words,
                record: &record,
            },
            &layout,
            1,
            &mut ProgramBuffers {
                executable_name: &mut executable_name,
                app_path: &mut app_path,
            },
            &mut observation,
        );

        assert_eq!(io::ErrorKind::InvalidData, result.unwrap_err().kind());
    }

    macro_rules! corrupt_identity_cases {
        ($($name:ident, $case:expr;)*) => {$(
            #[test]
            fn $name() {
                // Goal: malformed wire data never exposes an identity.
                // Method: corrupt one field in an otherwise valid record.
                refuses_corrupt_identity(&$case);
            }
        )*};
    }

    corrupt_identity_cases! {
        refuses_absent_creation_unique_id_padding, IdentityCorruption::AbsentCreation(1);
        refuses_absent_creation_pid_version_padding, IdentityCorruption::AbsentCreation(2);
        refuses_identity_pid_past_i32_bits,
            IdentityCorruption::Observation { field: 0, value: u64::MAX };
        refuses_identity_pid_version_past_i32_bits,
            IdentityCorruption::Observation { field: 2, value: u64::MAX };
        refuses_unknown_identity_availability,
            IdentityCorruption::Observation { field: 3, value: 7 };
        refuses_unknown_identity_flag_bits,
            IdentityCorruption::Observation { field: 3, value: 512 };
        refuses_unavailable_identity_with_a_program,
            IdentityCorruption::Observation { field: 3, value: 257 };
        refuses_unknown_usage_availability,
            IdentityCorruption::Observation { field: 4, value: 4 };
        refuses_measured_identity_without_a_program,
            IdentityCorruption::Observation { field: 5, value: 0 };
        refuses_uncommitted_program_reference,
            IdentityCorruption::Observation { field: 5, value: 2 };
        refuses_overflowing_descriptor_offset,
            IdentityCorruption::Descriptor { field: 0, value: u64::MAX - 7 };
        refuses_unaligned_descriptor_offset,
            IdentityCorruption::Descriptor { field: 0, value: 1 };
        refuses_descriptor_past_committed_arena,
            IdentityCorruption::Descriptor { field: 0, value: 64 };
        refuses_empty_descriptor_name,
            IdentityCorruption::Descriptor { field: 1, value: 0 };
        refuses_oversized_descriptor_name,
            IdentityCorruption::Descriptor { field: 1, value: 4096 };
        refuses_oversized_descriptor_app,
            IdentityCorruption::Descriptor { field: 2, value: 4096 };
    }

    #[test]
    fn retains_verified_creation_when_executable_read_is_refused() {
        // Goal: creation availability is independent of path availability and usage.
        // Method: store refusal with a present zero creation pair and no dictionary key.
        let (layout, words, _) = dictionary(DictionaryCapacity {
            entries: 1,
            arena_bytes: 8,
        });
        let value = IdentityObservation {
            pid: 0,
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Unavailable {
                creation: Some(ProcessCreation {
                    unique_id: 0,
                    pid_version: 0,
                }),
                reason: IdentityUnavailable::Refused,
            },
        };
        let record = [const { AtomicU64::new(0) }; 6];
        store_observation(&record, &value);

        let mut loaded = IdentityObservation::EMPTY;
        let mut executable_name = [0; PROGRAM_PATH_BYTES_MAX];
        let mut app_path = [0; PROGRAM_PATH_BYTES_MAX];
        load_observation(
            &IdentityRecord {
                words: &words,
                record: &record,
            },
            &layout,
            1,
            &mut ProgramBuffers {
                executable_name: &mut executable_name,
                app_path: &mut app_path,
            },
            &mut loaded,
        )
        .unwrap();

        assert_eq!(value, loaded);
    }

    #[test]
    fn retains_a_bundle_with_repeated_app_suffix_in_one_component() {
        // Goal: a repeated suffix inside one basename is not a nested bundle component.
        // Method: intern the outermost bundle bytes and copy them through the dictionary.
        let (layout, words, mut index) = dictionary(DictionaryCapacity {
            entries: 1,
            arena_bytes: 64,
        });
        let mut program = None;
        index
            .intern(
                &words,
                &layout,
                &ProgramBytes {
                    executable_name: b"helper",
                    app_path: Some(b"/Applications/A.app.app"),
                },
                &mut program,
            )
            .unwrap();
        let program = program.unwrap();
        let mut name = [0; PROGRAM_PATH_BYTES_MAX];
        let mut app = [0; PROGRAM_PATH_BYTES_MAX];

        let lengths = copy_program(
            &words,
            &layout,
            &program,
            1,
            &mut ProgramBuffers {
                executable_name: &mut name,
                app_path: &mut app,
            },
        )
        .unwrap();

        assert_eq!(b"helper", &name[..lengths.0]);
        assert_eq!(b"/Applications/A.app.app", &app[..lengths.1]);
    }

    fn refuses_invalid_program(bytes: &ProgramBytes) {
        let (layout, words, mut index) = dictionary(DictionaryCapacity {
            entries: 8,
            arena_bytes: 64,
        });
        let mut program = None;

        let result = index.intern(&words, &layout, bytes, &mut program);

        assert_eq!(Err(IdentityAvailability::Invalid), result);
        assert_eq!(0, load(&words, layout::HEADER_DICTIONARY_COUNT));
    }

    macro_rules! invalid_program_cases {
        ($($name:ident, $executable:expr, $app:expr;)*) => {$(
            #[test]
            fn $name() {
                // Goal: dictionary keys retain only bounded basenames and outermost App paths.
                // Method: offer one malformed byte sequence to the real dictionary.
                refuses_invalid_program(&ProgramBytes {
                    executable_name: $executable, app_path: $app,
                });
            }
        )*};
    }

    invalid_program_cases! {
        refuses_empty_program_name, &[], None;
        refuses_program_name_with_separator, b"a/b", None;
        refuses_program_name_with_nul, b"a\0b", None;
        refuses_oversized_program_name, &[b'x'; 4096], None;
        refuses_nested_bundle_components, b"a", Some(b"/A.app/Helper.app");
        refuses_program_path_past_bundle_component, b"a", Some(b"/A.app/Contents/a");
    }

    #[test]
    fn refuses_metadata_whose_usage_disagrees_with_its_exit_event() {
        // Goal: event usage availability agrees with the original numeric exit payload.
        // Method: pair measured exit metadata with a refused original exit record.
        let (layout, words, _) = dictionary(DictionaryCapacity {
            entries: 1,
            arena_bytes: 64,
        });
        let memory = crate::machine::Memory::default();
        let event = crate::tracker::Event::Exit {
            pid: 1,
            last: Err(crate::processes::Unreadable::Refused),
            noticed: crate::tracker::Noticed::OnExit,
        };
        let mut metadata = IdentityObservation::EMPTY;
        legacy(1, UsageAvailability::Measured, &mut metadata);
        let readings = super::super::Readings {
            run_tick: 1,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: std::slice::from_ref(&event),
            dropped: crate::ring::Dropped::default(),
        };

        let result = validate_publication(
            &words,
            &layout,
            &readings,
            &IdentityPublication {
                processes: &[],
                events: &[metadata],
            },
            1,
        );

        assert_eq!(io::ErrorKind::InvalidData, result.unwrap_err().kind());
    }
}
