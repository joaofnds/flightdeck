use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use collector::battery::{Battery, BatteryReading, BatterySource, Error, READ_INTERVAL_NS};
use collector::clock::Timebase;
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

    fn battery(collector: &Collector) -> Self {
        Self::sources(collector, false)
    }

    fn disks(collector: &Collector) -> Self {
        Self::sources(collector, true)
    }

    fn sources(collector: &Collector, disks: bool) -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let lock_path = std::env::temp_dir().join(format!(
            "fd-battery-publication-{}-{number}.lock",
            std::process::id()
        ));
        let place = Place {
            lock_path: lock_path.clone(),
            name_prefix: format!("/fdbp{}.{number}.", std::process::id()),
        };
        let capacity = collector.capacities();
        let shape = Shape {
            slot_count: 2,
            cpu_capacity: collector::u64_from_usize(capacity.cpus),
            interface_capacity: collector::u64_from_usize(capacity.interfaces),
            core_capacity: collector::u64_from_usize(capacity.cores),
            state_capacity: collector::u64_from_usize(capacity.states),
            fan_capacity: collector::u64_from_usize(capacity.fans),
            process_capacity: collector::u64_from_usize(capacity.processes),
            coalition_capacity: collector::u64_from_usize(capacity.coalitions),
            event_capacity: 2 * collector::u64_from_usize(capacity.events),
        };
        let identity = RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, number]),
            boot: Identity([4, 5]),
        };
        let create = if disks {
            Writer::create_with_disks
        } else {
            Writer::create_with_battery
        };
        let writer = create(
            &place,
            shape,
            DictionaryCapacity {
                entries: 256,
                arena_bytes: 65_536,
            },
            &identity,
            0,
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
                &readings(&memory, &[], &events), None, &[],
                &[collector::tracker::EventObservation::from($event).identity],
                &SupplementalReadings::default()).unwrap();
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

macro_rules! exit_source_cases {
    ($($name:ident, $creation:expr, $last:expr, $usage:expr, $expected:expr, $reason:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: publication preserves source exit association and independent numeric status.
            // Method: publish fixed source metadata and read the real v4 event identity back.
            let mut harness = Harness::new();
            let mut publication = Publication::new(&harness.writer).unwrap();
            let event = Event::Exit { pid: 4, last: $last, noticed: Noticed::OnExit };
            let source = EventIdentity { pid: 4, creation: EventCreation::Exit($creation) };
            let memory = collector::machine::Memory::default();
            let mut reader = RingReader::open_generation(harness.writer.name()).unwrap();

            publication.publish_readings(&mut harness.writer,
                &readings(&memory, &[], &[event]), None, &[], &[source],
                &SupplementalReadings::default()).unwrap();
            let mut identity = IdentityObservation::EMPTY;
            let found = reader.read_event_identity(&mut identity).unwrap();

            assert_eq!(found, FoundEvent::Event { number: 1, tick: 7,
                event: Event::Exit { pid: 4, last: $last, noticed: Noticed::OnExit } });
            assert_eq!(identity, IdentityObservation { pid: 4, usage: $usage,
                identity: ProcessIdentity::Unavailable { creation: $expected, reason: $reason } });
        }
    )*};
}

const EXIT_CREATION: collector::ring::ProcessCreation = collector::ring::ProcessCreation {
    unique_id: 93,
    pid_version: 8,
};

exit_source_cases! {
    preserves_source_verified_exit_creation, Ok(EXIT_CREATION), Ok(usage()),
        UsageAvailability::Measured, Some(EXIT_CREATION), IdentityUnavailable::Unsupported;
    preserves_refused_exit_creation, Err(IdentityUnavailable::Refused), Ok(usage()),
        UsageAvailability::Measured, None, IdentityUnavailable::Refused;
    preserves_gone_exit_creation, Err(IdentityUnavailable::Gone), Ok(usage()),
        UsageAvailability::Measured, None, IdentityUnavailable::Gone;
    preserves_changed_exit_creation, Err(IdentityUnavailable::Changed), Ok(usage()),
        UsageAvailability::Measured, None, IdentityUnavailable::Changed;
    preserves_refused_final_usage_and_unavailable_exit_witness,
        Err(IdentityUnavailable::Unsupported), Err(Unreadable::Refused),
        UsageAvailability::Refused, None, IdentityUnavailable::Unsupported;
    preserves_gone_final_usage_and_unavailable_exit_witness,
        Err(IdentityUnavailable::Unsupported), Err(Unreadable::Gone),
        UsageAvailability::Gone, None, IdentityUnavailable::Unsupported;
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
            &[],
            &SupplementalReadings::default(),
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
            &[],
            &SupplementalReadings::default(),
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

const BATTERY: BatteryReading = BatteryReading {
    source_continuous_ns: 0,
    charge_current: Some(0),
    charge_maximum: Some(100),
    full_charge_capacity: Some(9000),
    design_capacity: Some(8000),
    cycle_count: Some(11),
    charging: Some(false),
    external_power: Some(true),
    voltage_mv: Some(12000),
    current_ma: Some(-250),
    adapter_watts: None,
};

struct StubBattery;

impl BatterySource for StubBattery {
    fn read_into(&mut self, now_ns: u64, reading: &mut BatteryReading) -> Result<bool, Error> {
        if now_ns >= READ_INTERVAL_NS {
            return Err(Error::Native(-1));
        }

        *reading = BATTERY;
        reading.source_continuous_ns = now_ns;

        Ok(true)
    }
}

#[test]
fn publishes_acquired_and_held_battery_until_the_scheduled_refresh_fails() {
    // Goal: the collector acquisition path publishes real source time and clears failed refreshes.
    // Method: use real OS collection with an injected battery and decode three public ring records.
    let mut collector = Collector::new(Timebase::read().unwrap(), Duration::ZERO).unwrap();
    collector.smc = None;
    collector.ioreport = None;
    collector.battery = Some(Battery::from_source(Box::new(StubBattery)));
    let mut harness = Harness::battery(&collector);
    let mut publication = Publication::new(&harness.writer).unwrap();
    let mut reader = RingReader::open_generation(harness.writer.name()).unwrap();
    let mut tick = battery_tick(&reader.shape());
    let mut times = ObservationTimes {
        wall_ns: 100,
        continuous_ns: 0,
        awake_ns: 0,
    };

    publish_battery_tick(&mut publication, &mut harness, &mut collector, &times, 0);
    let first = reader.read_into(&mut tick).unwrap();

    assert_eq!(
        first,
        Found::Publication {
            publication: 1,
            tick: 0
        }
    );
    assert_eq!(tick.battery, Some(BATTERY));
    assert_eq!(tick.times, Some(times));
    times.continuous_ns = READ_INTERVAL_NS - 1;
    times.awake_ns = READ_INTERVAL_NS - 1;
    publish_battery_tick(&mut publication, &mut harness, &mut collector, &times, 1);
    let held = reader.read_into(&mut tick).unwrap();
    assert_eq!(
        held,
        Found::Publication {
            publication: 2,
            tick: 1
        }
    );
    assert_eq!(tick.battery, Some(BATTERY));
    times.continuous_ns = READ_INTERVAL_NS;
    times.awake_ns = READ_INTERVAL_NS;
    publish_battery_tick(&mut publication, &mut harness, &mut collector, &times, 2);
    let failed = reader.read_into(&mut tick).unwrap();
    assert_eq!(
        failed,
        Found::Publication {
            publication: 3,
            tick: 2
        }
    );
    assert_eq!(tick.battery, None);
}

fn publish_battery_tick(
    publication: &mut Publication,
    harness: &mut Harness,
    collector: &mut Collector,
    times: &ObservationTimes,
    run_tick: u64,
) {
    publication
        .read(
            collector,
            &mut Meter::default(),
            &mut harness.writer,
            Some(times),
        )
        .unwrap();
    publication
        .publish(&mut harness.writer, collector, run_tick, Some(times))
        .unwrap();
}

fn battery_tick(shape: &Shape) -> Tick {
    let count = |value| usize::try_from(value).unwrap();
    Tick::from_buffers(TickBuffers {
        cpus: Vec::with_capacity(count(shape.cpu_capacity)),
        interfaces: Vec::with_capacity(count(shape.interface_capacity)),
        gpu_active_ns: Vec::with_capacity(count(shape.state_capacity)),
        core_active_ns: vec![0; count(shape.core_capacity * shape.state_capacity)],
        core_state_counts: Vec::with_capacity(count(shape.core_capacity)),
        state_capacity: count(shape.state_capacity),
        fan_rpm: Vec::with_capacity(count(shape.fan_capacity)),
        processes: Vec::with_capacity(count(shape.process_capacity)),
        coalitions: Vec::with_capacity(count(shape.coalition_capacity)),
    })
}

#[test]
fn publishes_each_disk_attempt_at_the_tick_start_and_clears_failed_acquisition() {
    // Goal: the actual producer publishes acquired disk counters and the following failure.
    // Method: inject the source at the OS boundary, collect real ticks, and decode a real tail.
    use collector::disks::{DiskSnapshot, DiskSource, Disks, ScanState};
    struct StubDisks;
    impl DiskSource for StubDisks {
        fn read_into(
            &mut self,
            now_ns: u64,
            snapshot: &mut DiskSnapshot,
        ) -> Result<(), collector::disks::Error> {
            if now_ns == 2 {
                return Err(collector::disks::Error::Native(-71));
            }
            snapshot.source_continuous_ns = now_ns;
            snapshot.state = ScanState::Complete;
            snapshot.count = 1;
            snapshot.records[0].driver_id = 5;
            snapshot.records[0].media_id = 7;
            snapshot.records[0].name[..5].copy_from_slice(b"disk0");
            snapshot.records[0].read_bytes = Some(u64::MAX);
            Ok(())
        }
    }
    let mut collector = Collector::new(Timebase::read().unwrap(), Duration::ZERO).unwrap();
    collector.smc = None;
    collector.ioreport = None;
    collector.battery = None;
    collector.disks = Some(Disks::from_source(Box::new(StubDisks)));
    let mut harness = Harness::disks(&collector);
    let mut publication = Publication::new(&harness.writer).unwrap();
    let mut reader = RingReader::open_generation(harness.writer.name()).unwrap();
    let mut tick = battery_tick(&reader.shape());
    let mut times = ObservationTimes {
        wall_ns: 100,
        continuous_ns: 1,
        awake_ns: 1,
    };

    publish_battery_tick(&mut publication, &mut harness, &mut collector, &times, 0);
    reader.read_into(&mut tick).unwrap();

    let first = tick.disks.unwrap();
    assert_eq!(first.source_continuous_ns, 1);
    assert_eq!(first.records[0].read_bytes, Some(u64::MAX));
    times.continuous_ns = 2;
    publish_battery_tick(&mut publication, &mut harness, &mut collector, &times, 1);
    reader.read_into(&mut tick).unwrap();
    assert_eq!(
        tick.disks,
        Some(DiskSnapshot {
            source_continuous_ns: 2,
            ..DiskSnapshot::default()
        })
    );
}
