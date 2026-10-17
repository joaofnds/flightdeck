//! Window-owned bytes and bounded preparation before the source mapping is released.
use std::hash::{DefaultHasher, Hash, Hasher};

use collector::ring::{
    IdentityObservation, ProcessCreation, ProgramBuffers, ProgramBytes, ProgramReference,
    RingReader,
};

use crate::{Error, Result};

pub(crate) const PROGRAMS_MAX: usize = 65_536;
const PROGRAM_BYTES_MAX: usize = 64 * 1024 * 1024;
const LOOKUP_SLOTS: usize = PROGRAMS_MAX * 2;
const COPY_BYTES: usize = collector::ring::PROGRAM_PATH_BYTES_MAX;

/// The reader must be the exact handle that decoded both sets of references.
pub struct IdentitySource<'source> {
    pub reader: &'source RingReader,
    pub processes: &'source [IdentityObservation],
    pub events: &'source [IdentityObservation],
}

#[derive(Clone, Copy)]
pub(crate) struct OwnedIdentity {
    pub pid: i32,
    pub creation: Option<ProcessCreation>,
    pub availability: u8,
    pub usage: u8,
    pub program: Option<usize>,
}

// Fixed reservations let acceptance account for all retained dictionary storage.
pub(crate) struct Programs {
    bytes: Vec<u8>,
    entries: Vec<ProgramSpan>,
    slots: Vec<Option<usize>>,
}

#[derive(Clone, Copy)]
struct SlotEntry {
    first: usize,
    index: usize,
}

struct ProgramSpan {
    start: usize,
    name_end: usize,
    end: usize,
}

// Reader-scoped references exist only in this temporary preparation and never in Window.
pub(crate) struct Prepared {
    pub processes: Vec<OwnedIdentity>,
    pub events: Vec<OwnedIdentity>,
    programs: Vec<PendingProgram>,
    slots: Vec<Option<usize>>,
    references: Vec<Option<(ProgramReference, usize)>>,
    bytes: usize,
}

#[derive(Clone, Copy)]
enum IdentitySection {
    Processes,
    Events,
}

struct PendingProgram {
    bytes: Vec<u8>,
    name_end: usize,
}

const _: () = {
    assert!(LOOKUP_SLOTS.is_power_of_two());
    assert!(LOOKUP_SLOTS > PROGRAMS_MAX);
    assert!(PROGRAM_BYTES_MAX >= 2 * COPY_BYTES);
};

impl Programs {
    pub fn initialize(output: &mut Option<Box<Self>>) {
        assert!(output.is_none());

        let programs = output.insert(Box::new(Self {
            bytes: Vec::new(),
            entries: Vec::new(),
            slots: Vec::new(),
        }));

        programs.bytes = Vec::with_capacity(PROGRAM_BYTES_MAX);
        programs.entries = Vec::with_capacity(PROGRAMS_MAX);
        programs.slots = vec![None; LOOKUP_SLOTS];
    }

    pub const fn allocation_bytes() -> usize {
        size_of::<Self>()
            + PROGRAM_BYTES_MAX
            + PROGRAMS_MAX * size_of::<ProgramSpan>()
            + LOOKUP_SLOTS * size_of::<Option<usize>>()
    }

    pub fn usage(&self) -> collector::ring::DictionaryUsage {
        collector::ring::DictionaryUsage {
            entries: collector::u64_from_usize(self.entries.len()),
            arena_bytes: collector::u64_from_usize(self.bytes.len()),
        }
    }

    pub fn name(&self, index: usize) -> &[u8] {
        let span = &self.entries[index];
        &self.bytes[span.start..span.name_end]
    }

    pub fn app(&self, index: usize) -> Option<&[u8]> {
        let span = &self.entries[index];
        let app = &self.bytes[span.name_end..span.end];

        (!app.is_empty()).then_some(app)
    }

    fn find(&self, bytes: &ProgramBytes<'_>) -> Option<usize> {
        let name = bytes.executable_name;
        let app = bytes.app_path.unwrap_or_default();
        let first = slot_hash(&(name, app), LOOKUP_SLOTS);

        for offset in 0..LOOKUP_SLOTS {
            let slot = (first + offset) & (LOOKUP_SLOTS - 1);
            let index = self.slots[slot]?;
            if self.name(index) == name {
                if self.app(index) == (!app.is_empty()).then_some(app) {
                    return Some(index);
                }
            }
        }

        unreachable!("dictionary load remains below one half");
    }

    pub fn commit(&mut self, pending: &Prepared) {
        assert!(self.entries.len() + pending.programs.len() <= PROGRAMS_MAX);
        assert!(self.bytes.len() + pending.bytes <= PROGRAM_BYTES_MAX);

        for program in &pending.programs {
            let index = self.entries.len();
            let start = self.bytes.len();

            self.bytes.extend_from_slice(&program.bytes);
            self.entries.push(ProgramSpan {
                start,
                name_end: start + program.name_end,
                end: self.bytes.len(),
            });

            let name = &program.bytes[..program.name_end];
            let app = &program.bytes[program.name_end..];

            commit_insert_slot(
                &mut self.slots,
                SlotEntry {
                    first: slot_hash(&(name, app), LOOKUP_SLOTS),
                    index,
                },
            );
        }
    }
}

impl Prepared {
    pub fn peak_bytes(source: &IdentitySource<'_>) -> usize {
        let processes = source.processes.len();
        let events = source.events.len();
        let entries = (processes + events).min(PROGRAMS_MAX);
        let slots = (entries.max(1).next_power_of_two() * 2).min(LOOKUP_SLOTS);

        // Charge the largest possible pending byte payload before allocating any candidate.
        // Actual pending byte Vecs allocate only when a new program is encountered.
        (processes + events) * size_of::<OwnedIdentity>()
            + entries * size_of::<PendingProgram>()
            + slots * (size_of::<Option<usize>>() + size_of::<Option<(ProgramReference, usize)>>())
            + PROGRAM_BYTES_MAX
            + 2 * COPY_BYTES
            + size_of::<Self>()
            + size_of::<Option<Box<Self>>>()
    }

    pub fn initialize(
        output: &mut Option<Box<Self>>,
        source: &IdentitySource<'_>,
        programs: Option<&Programs>,
    ) -> Result<()> {
        assert!(output.is_none());

        let count = (source.processes.len() + source.events.len()).min(PROGRAMS_MAX);
        let slots = (count.max(1).next_power_of_two() * 2).min(LOOKUP_SLOTS);

        let prepared = output.insert(Box::new(Self {
            processes: Vec::new(),
            events: Vec::new(),
            programs: Vec::new(),
            slots: Vec::new(),
            references: Vec::new(),
            bytes: 0,
        }));

        prepared.processes = Vec::with_capacity(source.processes.len());
        prepared.events = Vec::with_capacity(source.events.len());
        prepared.programs = Vec::with_capacity(count);
        prepared.slots = vec![None; slots];
        prepared.references = vec![None; slots];

        for identity in source.processes {
            prepared.copy(
                source.reader,
                programs,
                identity,
                IdentitySection::Processes,
            )?;
        }
        for identity in source.events {
            prepared.copy(source.reader, programs, identity, IdentitySection::Events)?;
        }

        Ok(())
    }

    fn copy(
        &mut self,
        reader: &RingReader,
        programs: Option<&Programs>,
        identity: &IdentityObservation,
        section: IdentitySection,
    ) -> Result<()> {
        let program = match identity.program() {
            Some(reference) => Some(self.copy_program(reader, programs, reference)?),
            None => None,
        };
        let rows = match section {
            IdentitySection::Processes => &mut self.processes,
            IdentitySection::Events => &mut self.events,
        };

        rows.push(OwnedIdentity {
            pid: identity.pid,
            creation: identity.creation().copied(),
            availability: copy_identity_code(identity.availability()),
            usage: usage_code(identity.usage),
            program,
        });

        Ok(())
    }

    fn copy_program(
        &mut self,
        reader: &RingReader,
        programs: Option<&Programs>,
        reference: &ProgramReference,
    ) -> Result<usize> {
        let first = slot_hash(reference, self.references.len());

        for offset in 0..self.references.len() {
            let slot = (first + offset) & (self.references.len() - 1);
            if let Some((candidate, index)) = self.references[slot] {
                if candidate == *reference {
                    return Ok(index);
                }
            } else {
                let index = self.copy_program_bytes(reader, programs, reference)?;

                self.references[slot] = Some((*reference, index));

                return Ok(index);
            }
        }

        Err(Error::Bound("identity reference capacity"))
    }

    fn copy_program_bytes(
        &mut self,
        reader: &RingReader,
        programs: Option<&Programs>,
        reference: &ProgramReference,
    ) -> Result<usize> {
        let mut name = [0; COPY_BYTES];
        let mut app = [0; COPY_BYTES];

        let (name_length, app_length) = reader.copy_program(
            reference,
            &mut ProgramBuffers {
                executable_name: &mut name,
                app_path: &mut app,
            },
        )?;
        let bytes = ProgramBytes {
            executable_name: &name[..name_length],
            app_path: (app_length > 0).then_some(&app[..app_length]),
        };
        if let Some(programs) = programs {
            if let Some(index) = programs.find(&bytes) {
                return Ok(index);
            }
        }

        self.copy_program_bytes_pending(programs, &bytes)
    }

    fn copy_program_bytes_pending(
        &mut self,
        programs: Option<&Programs>,
        bytes: &ProgramBytes<'_>,
    ) -> Result<usize> {
        let name = bytes.executable_name;
        let app = bytes.app_path.unwrap_or_default();
        let first = slot_hash(&(name, app), self.slots.len());
        let existing = programs.map_or(0, |programs| programs.entries.len());

        for offset in 0..self.slots.len() {
            let slot = (first + offset) & (self.slots.len() - 1);
            match self.slots[slot] {
                Some(index) => {
                    let candidate = &self.programs[index];
                    if candidate.name_end == name.len() {
                        if candidate.bytes[..name.len()] == *name {
                            if candidate.bytes[name.len()..] == *app {
                                return Ok(existing + index);
                            }
                        }
                    }
                }
                None => return self.copy_program_bytes_pending_insert(programs, bytes, slot),
            }
        }

        Err(Error::Bound("identity program capacity"))
    }

    fn copy_program_bytes_pending_insert(
        &mut self,
        programs: Option<&Programs>,
        bytes: &ProgramBytes<'_>,
        slot: usize,
    ) -> Result<usize> {
        let name = bytes.executable_name;
        let app = bytes.app_path.unwrap_or_default();
        let existing = programs.map_or(0, |programs| programs.entries.len());
        if existing + self.programs.len() == PROGRAMS_MAX {
            return Err(Error::Bound("identity program capacity"));
        }

        let bytes = programs.map_or(0, |programs| programs.bytes.len());
        let added = name.len() + app.len();
        if bytes + self.bytes + added > PROGRAM_BYTES_MAX {
            return Err(Error::Bound("identity program bytes"));
        }

        let mut copied = Vec::with_capacity(added);
        copied.extend_from_slice(name);
        copied.extend_from_slice(app);

        let index = self.programs.len();

        self.programs.push(PendingProgram {
            bytes: copied,
            name_end: name.len(),
        });
        self.slots[slot] = Some(index);
        self.bytes += added;

        Ok(existing + index)
    }
}

fn slot_hash(value: &impl Hash, slots: usize) -> usize {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    let mask = collector::u64_from_usize(slots - 1);

    match usize::try_from(hasher.finish() & mask) {
        Ok(slot) => slot,
        Err(_) => unreachable!("masked slot fits memory index"),
    }
}

fn commit_insert_slot(slots: &mut [Option<usize>], entry: SlotEntry) {
    for offset in 0..slots.len() {
        let slot = (entry.first + offset) & (slots.len() - 1);
        if slots[slot].is_none() {
            slots[slot] = Some(entry.index);
            return;
        }
    }
    unreachable!("dictionary load remains below one half");
}

pub(crate) fn usage_code(value: collector::ring::UsageAvailability) -> u8 {
    use collector::ring::UsageAvailability;
    match value {
        UsageAvailability::Measured => crate::schema::availability::MEASURED,
        UsageAvailability::Refused => crate::schema::availability::REFUSED,
        UsageAvailability::Gone => crate::schema::availability::GONE,
        UsageAvailability::NotApplicable => crate::schema::availability::NOT_APPLICABLE,
    }
}

fn copy_identity_code(value: collector::ring::IdentityAvailability) -> u8 {
    use crate::schema::identity_availability;
    use collector::ring::IdentityAvailability;
    match value {
        IdentityAvailability::Measured => identity_availability::MEASURED,
        IdentityAvailability::Refused => identity_availability::REFUSED,
        IdentityAvailability::Gone => identity_availability::GONE,
        IdentityAvailability::Unsupported => identity_availability::UNSUPPORTED,
        IdentityAvailability::Changed => identity_availability::CHANGED,
        IdentityAvailability::Invalid => identity_availability::INVALID,
        IdentityAvailability::Exhausted => identity_availability::EXHAUSTED,
    }
}
