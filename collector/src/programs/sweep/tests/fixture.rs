use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::processes::{self, Process, Task, Unreadable, Usage};
use crate::ring::{
    DictionaryCapacity, Identity, Place, ProcessCreation, RunIdentity, Shape, Writer,
};

use super::super::*;

static NEXT: AtomicU64 = AtomicU64::new(1);

pub struct Harness {
    pub writer: Writer,
    pub sweep: Sweep,
    pub numeric: processes::Sweep,
    pub listing: Vec<i32>,
    pub calls: u32,
    lock_path: PathBuf,
}

impl Harness {
    pub fn new(entries: u64) -> Self {
        Self::with_capacity(DictionaryCapacity {
            entries,
            arena_bytes: 128,
        })
    }

    pub fn with_capacity(capacity: DictionaryCapacity) -> Self {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let lock_path = std::env::temp_dir().join(format!(
            "flightdeck-sweep-{}-{number}.lock",
            std::process::id()
        ));
        let place = Place {
            lock_path: lock_path.clone(),
            name_prefix: format!("/fdsw{}.{number}.", std::process::id()),
        };
        let shape = Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 4,
            coalition_capacity: 1,
            event_capacity: 4,
        };
        let identity = RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, number]),
            boot: Identity([4, 5]),
        };
        let writer = Writer::create_with_identity(&place, shape, capacity, &identity, 1).unwrap();

        Self {
            writer,
            sweep: Sweep::new(4),
            numeric: processes::Sweep::new(4),
            listing: Vec::with_capacity(4),
            calls: 0,
            lock_path,
        }
    }

    pub fn run(&mut self, pids: &[i32], numeric: Result<Process, Unreadable>) {
        self.run_amended(pids, numeric, |_, _, _| {});
    }

    pub fn run_amended(
        &mut self,
        pids: &[i32],
        numeric: Result<Process, Unreadable>,
        mut amend: impl FnMut(i32, &mut Acquisition, &mut [u8; 4096]),
    ) {
        self.listing.clear();
        self.listing.extend_from_slice(pids);
        let calls = &mut self.calls;
        self.sweep
            .run_with(
                &mut self.numeric,
                &mut self.listing,
                &mut self.writer,
                |pid, scratch, output| {
                    *calls += 1;
                    scratch[..15].copy_from_slice(b"/A.app/bin/x\xff\0\0");
                    output.creation_before = Ok(ProcessCreation {
                        unique_id: u64::try_from(pid).unwrap() + 7,
                        pid_version: pid + 1,
                    });
                    output.creation_after = output.creation_before;
                    output.readiness = Ok(());
                    output.path = Some(Ok(13));
                    output.candidate = true;
                    amend(pid, output, scratch);
                    match numeric {
                        Ok(mut process) => {
                            process.pid = pid;
                            process.usage.user_ns = u64::try_from(pid).unwrap();
                            Ok(Ok(process))
                        }
                        Err(error) => Ok(Err(error)),
                    }
                },
            )
            .unwrap();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        crate::ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_file(&self.lock_path).unwrap();
    }
}

pub fn process() -> Process {
    Process {
        pid: 2,
        usage: Usage {
            awake_ns_at_start: 1,
            user_ns: 2,
            system_ns: 3,
            energy_nj: 4,
            instructions: 5,
            super_instructions: 6,
            cycles: 7,
            idle_wakeups: 8,
            interrupt_wakeups: 9,
            pageins: 10,
            resident_bytes: 11,
            footprint_bytes: 12,
            disk_read_bytes: 13,
            disk_written_bytes: 14,
        },
        task: Task {
            threads: 1,
            faults: 2,
            context_switches: 3,
            mach_syscalls: 4,
            unix_syscalls: 5,
        },
    }
}

impl Harness {
    pub fn publish(&mut self) {
        let memory = crate::machine::Memory::default();
        let readings = crate::ring::Readings {
            run_tick: 1,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &self.numeric.readings,
            processes_refused: self.numeric.refused,
            processes_gone: self.numeric.gone,
            coalitions: &[],
            events: &[],
            dropped: crate::ring::Dropped::default(),
        };
        let identities = crate::ring::IdentityPublication {
            processes: self.sweep.observations(),
            events: &[],
        };

        self.writer
            .publish_with_identity(&readings, None, &identities)
            .unwrap();
    }
}

pub fn tick() -> crate::ring::Tick {
    crate::ring::Tick::from_buffers(crate::ring::TickBuffers {
        cpus: Vec::with_capacity(1),
        interfaces: Vec::with_capacity(1),
        gpu_active_ns: Vec::with_capacity(1),
        core_active_ns: vec![0; 1],
        core_state_counts: Vec::with_capacity(1),
        state_capacity: 1,
        fan_rpm: Vec::with_capacity(1),
        processes: Vec::with_capacity(4),
        coalitions: Vec::with_capacity(1),
    })
}
