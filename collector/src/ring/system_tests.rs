use super::*;
use crate::clock_states::ClockTables;
use crate::gpu_memory::GpuMemoryReading;
use crate::system::{FactText, MachineFacts, PerformanceLevel, SystemReading};

#[test]
fn carries_system_readings_after_the_unchanged_disks_extent() {
    // Goal: the system tail follows the exact disks extent and returns what was published.
    // Method: publish every reading, fact and process GPU time through a real ring and read.
    let mut fixture = Fixture::new();
    let (reading, machine) = full();
    fixture.publish(Some((&reading, &machine, &[Some(5), None])));
    let mut reader = Reader::open(&fixture.place.lock_path).unwrap();

    reader.read().unwrap();

    let tick = reader.tick();
    assert_eq!(tick.system, Some(SystemSnapshot { reading, machine }));
    assert_eq!(tick.process_gpu_ns(), [Some(5), None]);
    assert!(fixture.writer.system_supported());
    let layout = &fixture.writer.layout;
    assert_eq!(
        layout.system.as_ref().unwrap().record(1).start,
        layout.disks.as_ref().unwrap().word_count() + system::HEADER_WORDS
    );
}

#[test]
fn keeps_failed_readings_apart_from_zero_readings() {
    // Goal: a reading the collector could not take reads none, never a zero it never saw.
    // Method: publish zero readings, then readings that all failed, and read each back.
    let mut fixture = Fixture::new();
    let mut reader = Reader::open(&fixture.place.lock_path).unwrap();
    let zero = SystemReading {
        load_average: Some([0.0; 3]),
        open_files: Some(0),
        open_files_limit: Some(0),
        thermal_pressure: Some(0),
        gpu_memory: Some(GpuMemoryReading {
            in_use_bytes: Some(0),
            allocated_bytes: None,
        }),
        boot_wall_ns: Some(0),
    };
    let none = SystemReading::default();
    let machine = MachineFacts::default();

    fixture.publish(Some((&zero, &machine, &[Some(0), None])));
    reader.read().unwrap();
    let zeroes = reader.tick().system.unwrap();
    fixture.publish(Some((&none, &machine, &[None, None])));
    reader.read().unwrap();
    let nones = reader.tick().system.unwrap();

    assert_eq!(zeroes.reading, zero);
    assert_eq!(reader.tick().process_gpu_ns(), [None, None]);
    assert_eq!(nones.reading, none);
    assert_eq!(nones.machine, machine);
}

#[test]
fn a_publication_without_system_clears_a_reused_slot() {
    // Goal: a slot reused by a tick that published no system readings holds none of the old.
    // Method: wrap a populated slot of a two-slot ring with publications of none.
    let mut fixture = Fixture::new();
    let (reading, machine) = full();
    fixture.publish(Some((&reading, &machine, &[Some(5), None])));
    let mut reader = Reader::open(&fixture.place.lock_path).unwrap();
    reader.read().unwrap();

    fixture.publish(None);
    fixture.publish(None);
    reader.read().unwrap();
    reader.read().unwrap();

    assert_eq!(reader.tick().system, None);
    assert_eq!(reader.tick().process_gpu_ns(), []);
}

#[test]
fn a_disks_ring_reads_as_no_system_readings() {
    // Goal: a ring from before the system tail reads as having none, whatever a buffer held.
    // Method: read a disks writer's publication into a buffer that held system readings.
    let mut fixture = Fixture::old();
    fixture.publish_old();
    let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
    let mut tick = Tick::new(&fixture.writer.layout);
    tick.system = Some(SystemSnapshot::default());

    reader.read_into(&mut tick).unwrap();

    assert!(!fixture.writer.system_supported());
    assert_eq!(tick.system, None);
    assert_eq!(tick.process_gpu_ns(), []);
}

#[test]
fn refuses_gpu_times_for_other_than_the_published_processes() {
    // Goal: a process's GPU time can never land on another process's identity.
    // Method: publish two identities with one GPU time and expect the writer to refuse.
    let mut fixture = Fixture::new();
    let (reading, machine) = full();

    let published = fixture.publish_result(Some((&reading, &machine, &[Some(5)])));

    assert_eq!(published.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn stores_each_cores_state_clocks_from_its_state_count() {
    // Goal: a core's states take the clocks of the one table with as many entries.
    // Method: store a record for cores of two and three states, one count matching no table.
    let layout = record_layout();
    let words: Vec<AtomicU64> = (0..layout.record(1).len())
        .map(|_| AtomicU64::new(0))
        .collect();
    let clock = ClockTables::from_tables(vec![vec![1000, 2000], vec![500, 900, 1400]]);
    let (reading, machine) = full();
    let publication = SystemPublication {
        reading: &reading,
        machine: &machine,
        clock: Some(&clock),
        process_gpu_ns: &[],
    };
    let mut snapshot = None;
    let mut khz = [7; 2 * 3];
    let mut gpu_ns = Vec::with_capacity(2);

    system::store(&words, &layout, Some(&publication), [3, 1].into_iter());
    system::load(
        &words,
        &layout,
        &mut system::SystemLoad {
            snapshot: &mut snapshot,
            core_state_khz: &mut khz,
            stride: 3,
            process_gpu_ns: &mut gpu_ns,
        },
    )
    .unwrap();

    assert_eq!(khz, [500, 900, 1400, 0, 0, 0]);
    assert_eq!(snapshot.unwrap().machine, machine);
}

#[test]
fn clears_core_clocks_when_ioreport_is_unavailable_in_a_reused_record() {
    // Goal: an unavailable IOReport source leaves no frequencies from the previous sample.
    // Method: store measured core clocks, then no core states, reusing words and decode storage.
    let layout = record_layout();
    let words: Vec<AtomicU64> = (0..layout.record(1).len())
        .map(|_| AtomicU64::new(0))
        .collect();
    let clock = ClockTables::from_tables(vec![vec![500, 900, 1400]]);
    let (reading, machine) = full();
    let publication = SystemPublication {
        reading: &reading,
        machine: &machine,
        clock: Some(&clock),
        process_gpu_ns: &[],
    };
    let mut snapshot = None;
    let mut khz = [0; 6];
    let mut gpu_ns = Vec::with_capacity(2);
    let mut target = system::SystemLoad {
        snapshot: &mut snapshot,
        core_state_khz: &mut khz,
        stride: 3,
        process_gpu_ns: &mut gpu_ns,
    };
    system::store(&words, &layout, Some(&publication), [3].into_iter());
    system::load(&words, &layout, &mut target).unwrap();
    assert_eq!(target.core_state_khz, [500, 900, 1400, 0, 0, 0]);

    system::store(&words, &layout, Some(&publication), [].into_iter());
    system::load(&words, &layout, &mut target).unwrap();

    assert_eq!(target.core_state_khz, [0; 6]);
    assert_eq!(snapshot.unwrap().reading, reading);
}

#[test]
fn refuses_a_record_no_writer_of_this_format_stores() {
    // Goal: a record with bits this format never sets is refused rather than misread.
    // Method: set an unknown presence bit, then a GPU word without its presence bit.
    let layout = record_layout();
    let words: Vec<AtomicU64> = (0..layout.record(1).len())
        .map(|_| AtomicU64::new(0))
        .collect();
    let (reading, machine) = full();
    let publication = SystemPublication {
        reading: &reading,
        machine: &machine,
        clock: None,
        process_gpu_ns: &[Some(1), None],
    };
    system::store(&words, &layout, Some(&publication), [].into_iter());
    let presence = words[0].load(Ordering::Relaxed);
    let last = words.len() - 1;

    words[0].store(presence | 1 << 40, Ordering::Relaxed);
    let unknown_bit = load(&words, &layout);
    words[0].store(presence, Ordering::Relaxed);
    words[last - 1].store(3, Ordering::Relaxed);
    let bare_gpu_word = load(&words, &layout);

    assert_eq!(unknown_bit, Err(io::ErrorKind::InvalidData));
    assert_eq!(bare_gpu_word, Err(io::ErrorKind::InvalidData));
}

fn load(words: &[AtomicU64], layout: &system::SystemLayout) -> Result<(), io::ErrorKind> {
    let mut snapshot = None;
    let mut khz = [0; 2 * 3];
    let mut gpu_ns = Vec::with_capacity(2);
    system::load(
        words,
        layout,
        &mut system::SystemLoad {
            snapshot: &mut snapshot,
            core_state_khz: &mut khz,
            stride: 3,
            process_gpu_ns: &mut gpu_ns,
        },
    )
}

/// A record of two cores of three states and two processes, at the start of its words.
fn record_layout() -> system::SystemLayout {
    system::SystemLayout::plan(
        0,
        &system::SystemCapacities {
            slot_count: 1,
            cores: 2,
            states: 3,
            processes: 2,
        },
    )
    .unwrap()
}

fn full() -> (SystemReading, MachineFacts) {
    let reading = SystemReading {
        load_average: Some([1.5, 2.25, 3.125]),
        open_files: Some(12_345),
        open_files_limit: Some(122_880),
        thermal_pressure: Some(2),
        gpu_memory: Some(GpuMemoryReading {
            in_use_bytes: Some(u64::MAX),
            allocated_bytes: Some(1 << 33),
        }),
        boot_wall_ns: Some(1_760_000_000_123_456_000),
    };
    let text = |text: &str| FactText::new(text.as_bytes()).unwrap();
    let machine = MachineFacts {
        model: Some(text("Mac17,9")),
        chip: Some(text("Apple M5 Pro")),
        architecture: Some(text("arm64")),
        cpu_cores: Some(18),
        gpu_cores: Some(20),
        memory_bytes: Some(48 << 30),
        levels: Some([
            PerformanceLevel {
                name: text("Super"),
                logical_cpus: 6,
            },
            PerformanceLevel {
                name: FactText::new(&[b'p'; crate::system::FACT_BYTES_MAX]).unwrap(),
                logical_cpus: 12,
            },
        ]),
    };
    (reading, machine)
}

type Published<'a> = (&'a SystemReading, &'a MachineFacts, &'a [Option<u64>]);

struct Fixture {
    writer: Writer,
    place: Place,
}

impl Fixture {
    fn new() -> Self {
        Self::create(false)
    }

    fn old() -> Self {
        Self::create(true)
    }

    fn create(old: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        let place = Place {
            lock_path: std::env::temp_dir().join(format!(
                "flightdeck-system-ring-test-{}-{index}",
                std::process::id()
            )),
            name_prefix: "/fd.sy.".to_owned(),
        };
        let identity = RunIdentity {
            recording: Identity([11, 12]),
            run: Identity([21, 22]),
            boot: Identity([31, 32]),
        };
        let shape = Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 2,
            coalition_capacity: 1,
            event_capacity: 2,
        };
        let create = if old {
            Writer::create_with_disks
        } else {
            Writer::create_with_system
        };
        let writer = create(
            &place,
            shape,
            DictionaryCapacity {
                entries: 2,
                arena_bytes: 256,
            },
            &identity,
            100,
        )
        .unwrap();

        Self { writer, place }
    }

    fn publish(&mut self, system: Option<Published>) {
        self.publish_result(system).unwrap();
    }

    fn publish_old(&mut self) {
        self.publish_result(None).unwrap();
    }

    fn publish_result(&mut self, system: Option<Published>) -> io::Result<()> {
        let readings = Readings {
            run_tick: 7,
            cpus: &[],
            memory: &crate::machine::Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 2,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        };
        let refused = |pid| IdentityObservation {
            pid,
            usage: UsageAvailability::Refused,
            ..IdentityObservation::EMPTY
        };
        let processes = [refused(10), refused(20)];
        let identities = IdentityPublication {
            processes: &processes,
            events: &[],
        };
        let publication = system.map(|(reading, machine, process_gpu_ns)| SystemPublication {
            reading,
            machine,
            clock: None,
            process_gpu_ns,
        });
        self.writer.publish_with_disks(
            &readings,
            None,
            &identities,
            &SupplementalReadings {
                battery: None,
                disks: None,
                system: publication.as_ref(),
            },
        )
    }
}
