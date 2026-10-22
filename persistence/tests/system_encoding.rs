#[cfg(test)]
#[path = "query_support/normalized.rs"]
mod normalized;

#[cfg(test)]
mod tests {
    use super::normalized::{Normalized, normalize};
    use collector::gpu_memory::GpuMemoryReading;
    use collector::machine::Memory;
    use collector::ring::{
        DictionaryCapacity, Dropped, Found, GPU_NS_MAX, Identity, IdentityObservation,
        IdentityPublication, Place, Readings, RingReader, RunIdentity, Shape, SupplementalReadings,
        SystemPublication, UsageAvailability, Writer,
    };
    use collector::system::{FactText, MachineFacts, PerformanceLevel, SystemReading};
    use persistence::engine::{Engine, Row};
    use persistence::query::TableSource;
    use persistence::schema::{IDENTITY_FIELDS, TABLES, Table};
    use persistence::{Encoder, IdentitySource, Limits, Observation, Window, WindowIdentity};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    const SHAPE: Shape = Shape {
        slot_count: 4,
        cpu_capacity: 1,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 2,
        coalition_capacity: 1,
        event_capacity: 2,
    };
    const RUN: RunIdentity = RunIdentity {
        recording: Identity([11, 12]),
        run: Identity([21, 22]),
        boot: Identity([31, 32]),
    };

    #[test]
    fn saves_the_system_readings_a_window_captured() {
        // Goal: a sealed window reads back each system reading and fact the collector
        // published, and a reading it did not get, or a tick that published none, as NULL.
        // Method: stage full, failed and absent readings from a real ring, encode the window
        // to Parquet and read it through the reader's normalization.
        let mut fixture = Fixture::new();
        let (reading, machine) = full();
        let failed = (SystemReading::default(), MachineFacts::default());
        fixture.publish(Some((&reading, &machine, &[None, None])));
        fixture.publish(Some((&failed.0, &failed.1, &[None, None])));
        fixture.publish(None);
        let window = fixture.stage(3);

        let ticks = fixture.encoded(&window, Table::Ticks);

        let long = "p".repeat(collector::system::FACT_BYTES_MAX);
        let full_row = SystemRow {
            load_average_15: Some(3.125),
            open_files: Some(12_345),
            thermal_pressure: Some(2),
            gpu_memory_in_use_bytes: Some(u64::MAX),
            gpu_memory_allocated_bytes: None,
            boot_wall_ns: Some(1_760_000_000_123_456_000),
            machine_chip: Some("Apple M5 Pro".to_owned()),
            machine_memory_bytes: Some(48 << 30),
            machine_performance_level1_name: Some(long),
            machine_performance_level1_logical_cpus: Some(12),
        };
        assert_eq!(
            system_rows(&ticks),
            [full_row, SystemRow::default(), SystemRow::default()]
        );
    }

    #[test]
    fn saves_each_process_gpu_time_beside_its_identity() {
        // Goal: a process's GPU time reads back on its own process row, and a process with
        // no GPU clients reads NULL.
        // Method: publish two identities with one GPU time, encode and read by pid.
        let mut fixture = Fixture::new();
        let (reading, machine) = full();
        fixture.publish(Some((&reading, &machine, &[Some(GPU_NS_MAX), None])));
        let window = fixture.stage(1);

        let processes = fixture.encoded(&window, Table::Processes);

        let frame = processes.query("SELECT pid, gpu_ns FROM normalized ORDER BY pid");
        let rows: Vec<(i32, Option<u64>)> = (0..frame.height())
            .map(|index| {
                let row = Row::new(&frame, index).unwrap();
                (row.get(0).unwrap(), row.get(1).unwrap())
            })
            .collect();
        assert_eq!(rows, [(10, Some(GPU_NS_MAX)), (20, None)]);
    }

    #[test]
    fn leaves_the_columns_out_of_a_window_that_captured_no_system_readings() {
        // Goal: a window with no system readings keeps the columns it had before them.
        // Method: stage a tick of a system ring that published none and read the declared
        // columns of each table the readings extend.
        let mut fixture = Fixture::new();
        fixture.publish(None);
        let window = fixture.stage(1);

        let ticks = persistence::table_columns(Table::Ticks, &window);
        let processes = persistence::table_columns(Table::Processes, &window);
        let core_states = persistence::table_columns(Table::CoreStates, &window);

        let mut expected_processes = TABLES[7].1.to_owned();
        for (name, kind) in IDENTITY_FIELDS {
            expected_processes.push_str(", ");
            expected_processes.push_str(name);
            expected_processes.push(' ');
            expected_processes.push_str(kind);
        }
        assert_eq!(ticks, TABLES[0].1);
        assert_eq!(processes, expected_processes);
        assert_eq!(core_states, TABLES[5].1);
    }

    #[test]
    fn refuses_process_gpu_time_without_the_identities_it_follows() {
        // Goal: a GPU time never reaches a window that cannot say whose process it was.
        // Method: read a tick with GPU times from a real ring and stage it without identities.
        let mut fixture = Fixture::new();
        let (reading, machine) = full();
        fixture.publish(Some((&reading, &machine, &[Some(1), None])));
        let mut reader = RingReader::open(&fixture.place.lock_path).unwrap();
        let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
        let mut identities = Vec::with_capacity(2);
        let found = reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap();
        assert!(matches!(found, Found::Publication { .. }));
        let mut window = window();

        let result = window.push_tick(&observation(1), &tick, &[]);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
        assert_eq!(window.tick_count(), 0);
    }

    /// The readings a test compares, each NULL where the window holds none.
    #[derive(Debug, Default, PartialEq)]
    struct SystemRow {
        load_average_15: Option<f64>,
        open_files: Option<u64>,
        thermal_pressure: Option<u8>,
        gpu_memory_in_use_bytes: Option<u64>,
        gpu_memory_allocated_bytes: Option<u64>,
        boot_wall_ns: Option<u64>,
        machine_chip: Option<String>,
        machine_memory_bytes: Option<u64>,
        machine_performance_level1_name: Option<String>,
        machine_performance_level1_logical_cpus: Option<u32>,
    }

    fn system_rows(ticks: &Normalized) -> Vec<SystemRow> {
        let frame = ticks.query(
            "SELECT load_average_15, open_files, thermal_pressure, gpu_memory_in_use_bytes, \
            gpu_memory_allocated_bytes, boot_wall_ns, machine_chip, machine_memory_bytes, \
            machine_performance_level1_name, machine_performance_level1_logical_cpus \
            FROM normalized ORDER BY publication",
        );
        (0..frame.height())
            .map(|index| {
                let row = Row::new(&frame, index).unwrap();
                SystemRow {
                    load_average_15: row.get(0).unwrap(),
                    open_files: row.get(1).unwrap(),
                    thermal_pressure: row.get(2).unwrap(),
                    gpu_memory_in_use_bytes: row.get(3).unwrap(),
                    gpu_memory_allocated_bytes: row.get(4).unwrap(),
                    boot_wall_ns: row.get(5).unwrap(),
                    machine_chip: row.get(6).unwrap(),
                    machine_memory_bytes: row.get(7).unwrap(),
                    machine_performance_level1_name: row.get(8).unwrap(),
                    machine_performance_level1_logical_cpus: row.get(9).unwrap(),
                }
            })
            .collect()
    }

    fn full() -> (SystemReading, MachineFacts) {
        let reading = SystemReading {
            load_average: Some([1.5, 2.25, 3.125]),
            open_files: Some(12_345),
            open_files_limit: Some(122_880),
            thermal_pressure: Some(2),
            gpu_memory: Some(GpuMemoryReading {
                in_use_bytes: Some(u64::MAX),
                allocated_bytes: None,
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
                    name: FactText::new(&[b'p'; collector::system::FACT_BYTES_MAX]).unwrap(),
                    logical_cpus: 12,
                },
            ]),
        };
        (reading, machine)
    }

    fn window() -> Window {
        Window::new(
            WindowIdentity {
                run: RUN,
                window: 0,
            },
            Limits::from_shape(&SHAPE).unwrap(),
        )
    }

    fn observation(publication: u64) -> Observation {
        Observation {
            publication,
            wall_ns: Some(publication),
            continuous_ns: Some(publication),
            awake_ns: Some(publication),
        }
    }

    type Published<'a> = (&'a SystemReading, &'a MachineFacts, &'a [Option<u64>]);

    struct Fixture {
        directory: PathBuf,
        place: Place,
        writer: Option<Writer>,
        run_tick: u64,
    }

    impl Fixture {
        fn new() -> Self {
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir()
                .join(format!("system-encoding-{}-{number}", std::process::id()));
            std::fs::create_dir(&directory).unwrap();
            let place = Place {
                lock_path: directory.join("lock"),
                name_prefix: "fdsysenc".into(),
            };
            let writer = Writer::create_with_system(
                &place,
                SHAPE,
                DictionaryCapacity {
                    entries: 2,
                    arena_bytes: 256,
                },
                &RUN,
                0,
            )
            .unwrap();
            Self {
                directory,
                place,
                writer: Some(writer),
                run_tick: 0,
            }
        }

        /// Publishes two refused processes, pids 10 and 20, with `system` beside them.
        fn publish(&mut self, system: Option<Published>) {
            let refused = |pid| IdentityObservation {
                pid,
                usage: UsageAvailability::Refused,
                ..IdentityObservation::EMPTY
            };
            let processes = [refused(10), refused(20)];
            let publication = system.map(|(reading, machine, process_gpu_ns)| SystemPublication {
                reading,
                machine,
                clock: None,
                process_gpu_ns,
            });
            self.writer
                .as_mut()
                .unwrap()
                .publish_with_disks(
                    &Readings {
                        run_tick: self.run_tick,
                        cpus: &[],
                        memory: &Memory::default(),
                        interfaces: &[],
                        ioreport: None,
                        smc: None,
                        processes: &[],
                        processes_refused: 2,
                        processes_gone: 0,
                        coalitions: &[],
                        events: &[],
                        dropped: Dropped::default(),
                    },
                    None,
                    &IdentityPublication {
                        processes: &processes,
                        events: &[],
                    },
                    &SupplementalReadings {
                        battery: None,
                        disks: None,
                        system: publication.as_ref(),
                    },
                )
                .unwrap();
            self.run_tick += 1;
        }

        /// A window of the first `count` publications, staged as the persister stages them.
        fn stage(&self, count: u64) -> Window {
            let mut reader = RingReader::open(&self.place.lock_path).unwrap();
            let mut window = window();
            for publication in 1..=count {
                let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
                let mut identities = Vec::with_capacity(2);
                let found = reader
                    .read_identity_into(&mut tick, &mut identities)
                    .unwrap();
                assert!(matches!(found, Found::Publication { .. }));
                window
                    .push_tick_with_identity(
                        &observation(publication),
                        &tick,
                        &[],
                        &IdentitySource {
                            reader: &reader,
                            processes: &identities,
                            events: &[],
                        },
                    )
                    .unwrap();
            }
            window
        }

        /// The window's `table`, sealed to Parquet and read back as a recording is read.
        fn encoded(&self, window: &Window, table: Table) -> Normalized {
            let pending = self.directory.join("encoded");
            std::fs::create_dir(&pending).unwrap();
            Encoder.encode(window, &pending).unwrap();
            let path = pending.join(format!("{}.parquet", table.name()));
            normalize(&Engine::new(), table, TableSource::Parquet(&path)).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let writer = self.writer.take().unwrap();
            let name = writer.name().to_owned();
            drop(writer);
            collector::ring::unlink_generation(&name).unwrap();
            std::fs::remove_dir_all(&self.directory).unwrap();
        }
    }
}
