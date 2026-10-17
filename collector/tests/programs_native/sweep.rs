use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Instant;

use collector::ring::{self, DictionaryCapacity, Identity, Place, RunIdentity, Shape, Writer};

use super::Fixture;
use crate::allocation;

const SHAPE: Shape = Shape {
    slot_count: 2,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 1,
    coalition_capacity: 1,
    event_capacity: 2,
};

impl Fixture {
    pub fn exercise_sweep(&self) {
        let place = Place {
            lock_path: self.root.join("sweep.lock"),
            name_prefix: format!("/fdhost.{}.", self.pid()),
        };
        let identity = RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        };
        let capacity = DictionaryCapacity {
            entries: 2,
            arena_bytes: 4096,
        };
        let mut owned = OwnedRing {
            writer: Writer::create_with_identity(&place, SHAPE, capacity, &identity, 1).unwrap(),
            lock_path: place.lock_path,
        };
        let mut reader = ring::RingReader::open_generation(owned.writer.name()).unwrap();
        let mut numeric = collector::processes::Sweep::new(1);
        let mut programs = collector::programs::sweep::Sweep::new(1);
        let mut pids = vec![self.pid()];
        let timebase = collector::clock::Timebase::read().unwrap();
        let mut copied = Copied::EMPTY;
        let mut previous = None;
        for tick in 1..=2 {
            let started = Instant::now();
            let allocations = allocation::calls_in(|| {
                programs
                    .run(timebase, &mut numeric, &mut pids, &mut owned.writer)
                    .unwrap();
                publish(&mut owned.writer, tick, &numeric, &programs);
            });
            let elapsed_ns = started.elapsed().as_nanos();
            assert_eq!(0, allocations);
            assert_eq!(1, programs.diagnostics().measured);
            assert_eq!(1, numeric.readings.len());
            let observation = programs.observations()[0];
            assert_eq!(Some(&self.ack.creation), observation.creation());
            let reference = observation.program().copied().unwrap();
            if let Some(previous) = previous {
                assert_eq!(previous, reference);
            }
            previous = Some(reference);
            copied.check(&mut reader, tick, &reference, self);
            eprintln!(
                "actual_sweep tick={tick} pid={} creation={:?} reference={reference:?} \
                dictionary={:?} elapsed_ns={elapsed_ns} rust_allocator_calls={allocations}",
                self.pid(),
                self.ack.creation,
                owned.writer.dictionary_usage().unwrap()
            );
        }
    }
}

fn publish(
    writer: &mut Writer,
    tick: u64,
    numeric: &collector::processes::Sweep,
    programs: &collector::programs::sweep::Sweep,
) {
    let memory = collector::machine::Memory::default();
    let readings = ring::Readings {
        run_tick: tick,
        cpus: &[],
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &numeric.readings,
        processes_refused: numeric.refused,
        processes_gone: numeric.gone,
        coalitions: &[],
        events: &[],
        dropped: ring::Dropped::default(),
    };
    let identities = ring::IdentityPublication {
        processes: programs.observations(),
        events: &[],
    };
    writer
        .publish_with_identity(&readings, None, &identities)
        .unwrap();
}

struct Copied {
    executable: [u8; 4095],
    app: [u8; 4095],
}

impl Copied {
    const EMPTY: Self = Self {
        executable: [0; 4095],
        app: [0; 4095],
    };

    fn check(
        &mut self,
        reader: &mut ring::RingReader,
        tick_number: u64,
        reference: &ring::ProgramReference,
        fixture: &Fixture,
    ) {
        let mut tick = ring::Tick::from_buffers(ring::TickBuffers {
            cpus: Vec::with_capacity(1),
            interfaces: Vec::with_capacity(1),
            gpu_active_ns: Vec::with_capacity(1),
            core_active_ns: vec![0; 1],
            core_state_counts: Vec::with_capacity(1),
            state_capacity: 1,
            fan_rpm: Vec::with_capacity(1),
            processes: Vec::with_capacity(1),
            coalitions: Vec::with_capacity(1),
        });
        let mut identities = Vec::with_capacity(1);
        let found = reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap();
        assert_eq!(
            ring::Found::Publication {
                publication: tick_number,
                tick: tick_number
            },
            found
        );
        assert_eq!(fixture.pid(), tick.processes[0].pid);
        assert_eq!(fixture.pid(), identities[0].pid);
        assert_eq!(ring::UsageAvailability::Measured, identities[0].usage);
        assert_eq!(Some(&fixture.ack.creation), identities[0].creation());
        let decoded_reference = identities[0].program().unwrap();
        // References deliberately bind to their owning writer/reader, even on this same ring.
        assert_ne!(reference, decoded_reference);
        let lengths = reader
            .copy_program(
                decoded_reference,
                &mut ring::ProgramBuffers {
                    executable_name: &mut self.executable,
                    app_path: &mut self.app,
                },
            )
            .unwrap();
        assert_eq!(b"identity_probe", &self.executable[..lengths.0]);
        assert_eq!(fixture.app_a.as_os_str().as_bytes(), &self.app[..lengths.1]);
        eprintln!(
            "sweep_copied tick={tick_number} executable={:?} app={:?}",
            &self.executable[..lengths.0],
            &self.app[..lengths.1]
        );
    }
}

struct OwnedRing {
    writer: Writer,
    lock_path: PathBuf,
}

impl Drop for OwnedRing {
    fn drop(&mut self) {
        ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_file(&self.lock_path).unwrap();
    }
}
