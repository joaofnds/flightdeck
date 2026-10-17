use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::processes::{Process, Task, Unreadable, Usage};
use collector::ring::{
    DictionaryCapacity, Found, FoundEvent, Identity, Place, RingReader, RunIdentity, Shape, Tick,
    TickBuffers,
};
use collector::tracker::Noticed;

use super::*;

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Harness {
    writer: Writer,
    lock_path: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let lock_path = std::env::temp_dir().join(format!(
            "fd-publication-{}-{number}.lock",
            std::process::id()
        ));
        let place = Place {
            lock_path: lock_path.clone(),
            name_prefix: format!("/fdpb{}.{number}.", std::process::id()),
        };
        let shape = Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 4,
        };
        let identity = RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, number]),
            boot: Identity([4, 5]),
        };
        let writer = Writer::create_with_identity(
            &place,
            shape,
            DictionaryCapacity {
                entries: 4,
                arena_bytes: 128,
            },
            &identity,
            1,
        )
        .unwrap();

        Self { writer, lock_path }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        collector::ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_file(&self.lock_path).unwrap();
    }
}

fn usage() -> Usage {
    Usage {
        awake_ns_at_start: 11,
        user_ns: 12,
        system_ns: 13,
        energy_nj: 14,
        instructions: 15,
        super_instructions: 16,
        cycles: 17,
        idle_wakeups: 18,
        interrupt_wakeups: 19,
        pageins: 20,
        resident_bytes: 21,
        footprint_bytes: 22,
        disk_read_bytes: 23,
        disk_written_bytes: 24,
    }
}

fn process() -> Process {
    Process {
        pid: 2,
        usage: usage(),
        task: Task {
            threads: 31,
            faults: 32,
            context_switches: 33,
            mach_syscalls: 34,
            unix_syscalls: 35,
        },
    }
}

fn tick() -> Tick {
    Tick::from_buffers(TickBuffers {
        cpus: Vec::with_capacity(1),
        interfaces: Vec::with_capacity(1),
        gpu_active_ns: Vec::with_capacity(1),
        core_active_ns: vec![0; 1],
        core_state_counts: Vec::with_capacity(1),
        state_capacity: 1,
        fan_rpm: Vec::with_capacity(1),
        processes: Vec::with_capacity(1),
        coalitions: Vec::with_capacity(1),
    })
}

fn readings<'data>(
    memory: &'data collector::machine::Memory,
    processes: &'data [Process],
    events: &'data [Event],
) -> Readings<'data> {
    Readings {
        run_tick: 7,
        cpus: &[],
        memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes,
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events,
        dropped: Dropped::default(),
    }
}

macro_rules! event_cases {
    ($($name:ident, $event:expr, $usage:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: event publication retains numeric status without inventing identity.
            // Method: publish one literal event and decode its independently retained metadata.
            let mut harness = Harness::new();
            let mut publication = Publication::new(&harness.writer).unwrap();
            let events = [$event];
            let memory = collector::machine::Memory::default();
            let mut reader = RingReader::open_generation(harness.writer.name()).unwrap();

            publication.publish_readings(&mut harness.writer,
                &readings(&memory, &[], &events), None, &[]).unwrap();
            let mut identity = IdentityObservation::EMPTY;
            let found = reader.read_event_identity(&mut identity).unwrap();

            assert_eq!(found, FoundEvent::Event { number: 1, tick: 7, event: $event });
            assert_eq!(identity, IdentityObservation { pid: 4, usage: $usage,
                identity: ProcessIdentity::Unavailable { creation: None,
                    reason: IdentityUnavailable::Unsupported } });
        }
    )*};
}

event_cases! {
    leaves_start_identity_unsupported, Event::Start { pid: 4, coalition: Some(71) },
        UsageAvailability::NotApplicable;
    leaves_exec_identity_unsupported, Event::Exec { pid: 4 }, UsageAvailability::NotApplicable;
    leaves_final_usage_identity_unsupported, Event::Exit { pid: 4, last: Ok(usage()),
        noticed: Noticed::OnExit }, UsageAvailability::Measured;
    preserves_refused_final_usage_without_identity, Event::Exit { pid: 4,
        last: Err(Unreadable::Refused), noticed: Noticed::OnExit }, UsageAvailability::Refused;
    preserves_gone_final_usage_without_identity, Event::Exit { pid: 4,
        last: Err(Unreadable::Gone), noticed: Noticed::AlreadyExited }, UsageAvailability::Gone;
}

#[test]
fn preserves_independent_numeric_values_when_process_identity_is_unavailable() {
    // Goal: identity publication does not alter any numeric process counter or task field.
    // Method: publish independent literal numeric values and decode the resulting record.
    let mut harness = Harness::new();
    let mut publication = Publication::new(&harness.writer).unwrap();
    let memory = collector::machine::Memory::default();
    let mut reader = RingReader::open_generation(harness.writer.name()).unwrap();
    let mut tick = tick();
    let mut identities = Vec::with_capacity(1);
    let identity = IdentityObservation {
        pid: 2,
        usage: UsageAvailability::Measured,
        identity: ProcessIdentity::Unavailable {
            creation: None,
            reason: IdentityUnavailable::Unsupported,
        },
    };

    publication
        .publish_readings(
            &mut harness.writer,
            &readings(&memory, &[process()], &[]),
            None,
            &[identity],
        )
        .unwrap();
    let found = reader
        .read_identity_into(&mut tick, &mut identities)
        .unwrap();

    assert_eq!(
        found,
        Found::Publication {
            publication: 1,
            tick: 7
        }
    );
    assert_eq!(tick.processes, [process()]);
    assert_eq!(identities, [identity]);
}

#[test]
fn returns_publication_validation_errors_without_advancing_the_ring() {
    // Goal: malformed process alignment fails rather than publishing a false numeric fallback.
    // Method: offer a measured numeric row without its required identity row.
    let mut harness = Harness::new();
    let mut publication = Publication::new(&harness.writer).unwrap();
    let memory = collector::machine::Memory::default();

    let error = publication
        .publish_readings(
            &mut harness.writer,
            &readings(&memory, &[process()], &[]),
            None,
            &[],
        )
        .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        RingReader::open_generation(harness.writer.name())
            .unwrap()
            .publication_newest()
            .unwrap(),
        0
    );
}
