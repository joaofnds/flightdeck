use super::*;
use crate::session::Session;
use collector::machine::{CPUTicks, Memory};
use collector::recording::RegisteredRun;
use collector::ring::{
    Dropped, Found, ObservationTimes, Place, Readings, RingReader, Shape, Tick, TickBuffers, Writer,
};
use persistence::{Limits, Store, Window, WindowIdentity};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Recording {
    root: PathBuf,
    writer: Writer,
    window: Window,
}

impl Recording {
    fn new() -> Self {
        Self::with_ticks(2, |publication| tick(publication == 2))
    }

    fn with_ticks(count: u64, reading: impl Fn(u64) -> Tick) -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("flightdeck-scalar-{}-{serial}", std::process::id()));
        let store = Store::open(&root).unwrap();
        let shape = Shape {
            slot_count: 8,
            cpu_capacity: 2,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 2,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 8,
        };
        let place = Place {
            lock_path: root.join("ring.lock"),
            name_prefix: format!("/fdscalar{}x{serial}", std::process::id()),
        };
        let run = collector::recording::new_run_identity(store.recording()).unwrap();
        let mut writer = Writer::create_recording(&place, shape, &run, 0).unwrap();
        let mut reader = RingReader::open_generation(writer.name()).unwrap();
        let mut read = persistence::tick_buffer(&shape).unwrap();
        let mut window = Window::new(
            WindowIdentity { run, window: 0 },
            Limits::from_shape(&shape).unwrap(),
        );
        for publication in 1..=count {
            let tick = reading(publication);
            let times = ObservationTimes {
                wall_ns: (999 + publication) * 1_000_000_000,
                continuous_ns: (publication * 2 - 1) * 1_000_000_000,
                awake_ns: publication * 1_000_000_000,
            };
            with_ticks_publish(&mut writer, &tick, &times);
            assert!(matches!(
                reader.read_into(&mut read).unwrap(),
                Found::Publication { .. }
            ));
            window
                .push_tick(
                    &persistence::Observation {
                        publication,
                        wall_ns: Some(times.wall_ns),
                        continuous_ns: Some(times.continuous_ns),
                        awake_ns: Some(times.awake_ns),
                    },
                    &read,
                    &[],
                )
                .unwrap();
        }
        Self {
            root,
            writer,
            window,
        }
    }

    fn project(&self) -> Tables {
        let connection = crate::data::connection();
        let control = Arc::new(crate::Cancellation::default());
        let operation = control.operation(0).unwrap();
        let mut tables = Tables::new();
        for kind in persistence::schema::TABLES {
            let order = kind.order();
            let name = kind.name();
            if TABLE_NAMES.contains(&name) {
                connection
                    .register(name, persistence::frame(kind, &self.window).unwrap())
                    .unwrap();
                let table = crate::data::query(
                    &connection,
                    &format!("SELECT * FROM {name} ORDER BY {order}"),
                    crate::data::ROWS_MAX,
                    &operation,
                )
                .unwrap();
                tables.insert((0, name), partition(&table).unwrap());
            }
        }
        tables
    }

    fn seal(&self) {
        self.seal_with(|_pending| {});
    }

    /// Saves the ring's readings with the persister's own worker, then gives `adjust` the directory
    /// they lie in.
    fn seal_with(&self, adjust: impl FnOnce(&Path)) {
        let store = Store::open(&self.root).unwrap();
        let run = self.window.identity.run;
        let registered = RegisteredRun {
            identity: run,
            ring_name: self.writer.name().to_owned(),
            collector_pid: 0,
        };
        persistence::worker::seal_held_windows(
            &store,
            &registered,
            None,
            &mut persistence::worker::HeldWindows::default(),
            &crate::live::identity_fixture::Underived,
        )
        .unwrap();
        let commitment = crate::live::identity_fixture::saved_commitment(&self.root, &run, 0);
        adjust(commitment.parent().unwrap());
    }
}

/// Publishes what `tick` holds, leaving out the sources it says went unread.
fn with_ticks_publish(writer: &mut Writer, tick: &Tick, times: &ObservationTimes) {
    writer.publish_observed(
        &Readings {
            run_tick: tick.run_tick,
            cpus: &tick.cpus,
            memory: &tick.memory,
            interfaces: &[],
            ioreport: tick
                .ioreport_read
                .then_some(collector::ioreport::IOReportReadings {
                    gpu_energy_nj: tick.gpu_energy_nj,
                    gpu_active_ns: &tick.gpu_active_ns,
                    cores: &[],
                }),
            smc: tick.smc_read.then_some(collector::smc::SmcReadings {
                power_watts: tick.power_watts,
                soc_celsius: tick.soc_celsius,
                cpu_celsius: tick.cpu_celsius,
                gpu_celsius: tick.gpu_celsius,
                fan_rpm: &tick.fan_rpm,
            }),
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        },
        times,
    );
}

impl Drop for Recording {
    fn drop(&mut self) {
        collector::ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn tick(current: bool) -> Tick {
    let mut tick = Tick::from_buffers(TickBuffers {
        cpus: vec![
            CPUTicks {
                user: if current { 100 } else { 0 },
                system: 0,
                nice: 0,
                idle: 0,
            },
            CPUTicks {
                user: if current { 100 } else { 0 },
                system: 0,
                nice: 0,
                idle: if current { 100 } else { 0 },
            },
        ],
        interfaces: Vec::new(),
        gpu_active_ns: if current {
            vec![1_500_000_000, 2_300_000_000]
        } else {
            vec![1_000_000_000, 2_000_000_000]
        },
        core_active_ns: Vec::new(),
        core_state_counts: Vec::new(),
        state_capacity: 2,
        fan_rpm: Vec::new(),
        processes: Vec::new(),
        coalitions: Vec::new(),
    });
    tick.run_tick = if current { 2 } else { 1 };
    tick.memory = Memory {
        total_bytes: 9_007_199_254_740_993,
        free_bytes: 2,
        active_bytes: 3,
        inactive_bytes: 4,
        speculative_bytes: 5,
        wired_bytes: 6,
        compressed_bytes: 7,
        anonymous_bytes: 8,
        file_backed_bytes: 9,
        purgeable_bytes: 10,
        swap_total_bytes: 11,
        swap_used_bytes: 12,
        swapped_in_bytes: if current { 1124 } else { 100 },
        swapped_out_bytes: if current { 2248 } else { 200 },
        pageins: if current { 7 } else { 1 },
        pageouts: 2,
        compressions: if current { 11 } else { 3 },
        decompressions: if current { 6 } else { 4 },
        ..Memory::default()
    };
    tick.ioreport_read = true;
    tick.gpu_energy_nj = if current {
        5_000_000_000
    } else {
        1_000_000_000
    };
    tick.smc_read = true;
    tick.power_watts = 0.0;
    tick.soc_celsius = 45.0;
    tick.cpu_celsius = current.then_some(61.5);
    tick.gpu_celsius = current.then_some(52.25);
    tick
}

#[test]
fn preserves_optional_source_failure_and_recovery_in_live_and_saved_projection() {
    // Goal: sealing preserves absence, fresh recovery, and the missing counter predecessor.
    // Method: encode true/false/true/true availability with distinct gauges and absolute counters.
    let recording = Recording::with_ticks(4, recovery_tick);
    let tables = recording.project();
    let observations: [Observation; 4] = std::array::from_fn(|index| {
        let publication = u64::try_from(index).unwrap() + 1;
        Observation {
            window: 0,
            publication,
            run_tick: publication,
            wall: (999 + publication) * 1_000_000_000,
            continuous: Some((publication * 2 - 1) * 1_000_000_000),
        }
    });
    recording.seal();
    let mut session = Session::open(&recording.root).unwrap();

    for (index, observation) in observations.iter().enumerate() {
        let previous = index.checked_sub(1).map(|index| &observations[index]);
        let mut live = point(&tables, observation, previous, 1).unwrap();
        live["run"] = json!(
            collector::recording::identity_text(recording.window.identity.run.run).to_string()
        );
        live["boot"] = json!(
            collector::recording::identity_text(recording.window.identity.run.boot).to_string()
        );
        let saved = session
            .select(1000 + u64::try_from(index).unwrap(), 0)
            .unwrap();
        assert_eq!(saved["summary"], live);
        assert_eq!(
            live["power_watts"],
            [json!(0.0), Value::Null, json!(30.0), json!(0.0)][index]
        );
        assert_eq!(
            live["fan_rpm"],
            [json!([0.0]), Value::Null, json!([30.0]), json!([0.0])][index]
        );
        assert_eq!(
            live["gpu_watts"],
            [Value::Null, Value::Null, Value::Null, json!(0.5)][index]
        );
        assert_eq!(
            live["gpu_active_percent"],
            [Value::Null, Value::Null, Value::Null, json!(10.0)][index]
        );
        assert_eq!(live["memory_free_bytes"], 2);
    }
    assert_failed_encoding(&tables);
}

fn assert_failed_encoding(tables: &Tables) {
    let failed = &tables[&(0, "ticks")][&2];
    for field in [
        "gpu_energy_nj",
        "power_watts",
        "soc_celsius",
        "cpu_celsius",
        "gpu_celsius",
    ] {
        let column = failed
            .columns
            .iter()
            .position(|name| name == field)
            .unwrap();
        assert_eq!(failed.rows[0][column], None);
    }
    assert!(!tables[&(0, "gpu_states")].contains_key(&2));
    assert!(!tables[&(0, "fans")].contains_key(&2));
}

fn recovery_tick(publication: u64) -> Tick {
    let mut tick = tick(publication == 4);
    tick.run_tick = publication;
    tick.smc_read = publication != 2;
    tick.ioreport_read = publication != 2;
    let gauge = if publication == 3 { 30.0 } else { 0.0 };
    tick.power_watts = gauge;
    tick.soc_celsius = 40.0 + gauge;
    tick.cpu_celsius = (publication != 2).then_some(40.0 + gauge);
    tick.gpu_celsius = (publication != 2).then_some(40.0 + gauge);
    tick.fan_rpm = vec![gauge];
    tick.gpu_energy_nj = publication * 1_000_000_000;
    tick.gpu_active_ns = vec![publication * 100_000_000; 2];
    tick
}

#[test]
fn keeps_the_unsealed_encoder_projection_equal_to_saved_resource_scalars() {
    // Goal: sealing cannot change any public scalar field or source identity.
    // Method: project one source window through the live encoder, then seal and query it.
    let recording = Recording::new();
    let tables = recording.project();
    let previous = Observation {
        window: 0,
        publication: 1,
        run_tick: 1,
        wall: 1_000_000_000_000,
        continuous: Some(1_000_000_000),
    };
    let current = Observation {
        window: 0,
        publication: 2,
        run_tick: 2,
        wall: 1_001_000_000_000,
        continuous: Some(3_000_000_000),
    };
    let mut unsealed = point(&tables, &current, Some(&previous), 1).unwrap();
    unsealed["run"] =
        json!(collector::recording::identity_text(recording.window.identity.run.run).to_string());
    unsealed["boot"] =
        json!(collector::recording::identity_text(recording.window.identity.run.boot).to_string());
    let expected = unsealed.clone();
    unsealed["observation"] = json!({"internal": "retained only inside live cache"});
    recording.seal();
    let mut session = Session::open(&recording.root).unwrap();

    let response = session.select(1001, 0).unwrap();

    assert_eq!(public_point(&unsealed).unwrap(), expected);
    assert_eq!(response["summary"], expected);
    assert_eq!(expected["cpu_percent_one_core"], 150.0);
    assert_eq!(response["trends"][0]["cpu_percent_one_core"], Value::Null);
    assert_eq!(response["trends"][1]["cpu_percent_one_core"], 150.0);
    assert_eq!(response["summary"]["fan_rpm"], json!([]));
    assert_eq!(response["summary"]["gpu_active_percent"], 40.0);
    assert_eq!(response["summary"]["gpu_watts"], 2.0);
    assert_eq!(response["summary"]["cpu_celsius"], 61.5);
    assert_eq!(response["summary"]["gpu_celsius"], 52.25);
    assert_eq!(response["trends"][0]["cpu_celsius"], Value::Null);
    assert_eq!(response["trends"][1]["gpu_celsius"], 52.25);
    assert_eq!(response["summary"]["memory_pageins_per_second"], 3.0);
    assert_eq!(response["summary"]["network_rx_bps"], 0.0);
    assert_eq!(
        response
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "alternatives",
            "context_last_second",
            "context_seconds",
            "second",
            "selected",
            "summary",
            "trends"
        ]
    );
}

#[test]
fn refuses_an_admitted_empty_fan_source_without_its_rpm_column() {
    // Goal: an empty malformed saved fan source never becomes measured fanless.
    // Method: save the minute, replace its fans with a two-column empty Parquet, then select
    // publicly.
    use polars::polars_utils::compression::ZstdLevel;
    use polars::prelude::{
        Column, DataFrame, ParquetCompression, ParquetWriter, StatisticsOptions,
    };

    let recording = Recording::new();
    recording.seal_with(|ready| {
        let fans = ready.join("fans.parquet");
        assert!(fans.is_file());
        std::fs::remove_file(&fans).unwrap();
        let mut frame = DataFrame::new(
            0,
            vec![
                Column::new("publication".into(), Vec::<u64>::new()),
                Column::new("fan".into(), Vec::<u8>::new()),
            ],
        )
        .unwrap();
        ParquetWriter::new(collector::recording::private::open_private_file(&fans, true).unwrap())
            .with_compression(ParquetCompression::Zstd(Some(
                ZstdLevel::try_new(3).unwrap(),
            )))
            .with_statistics(StatisticsOptions {
                min_value: true,
                max_value: true,
                distinct_count: false,
                null_count: true,
                binary_statistics_truncate_length: None,
            })
            .with_row_group_size(Some(16_384))
            .with_data_page_size(None)
            .set_parallel(false)
            .with_key_value_metadata(None)
            .with_context_info(None)
            .finish(&mut frame)
            .unwrap();
    });
    let mut session = Session::open(&recording.root).unwrap();
    let fans = session.table(0, "fans").unwrap();
    assert_eq!(fans.columns, ["publication", "fan"]);
    assert!(fans.rows.is_empty());
    let ticks = session.table(0, "ticks").unwrap();
    assert_eq!(ticks.rows.len(), 2);
    assert_eq!(
        ticks.field(&ticks.rows[0], "smc_read").unwrap(),
        Some("true")
    );
    assert_eq!(
        ticks.field(&ticks.rows[1], "smc_read").unwrap(),
        Some("true")
    );

    let result = session.select(1001, 0);

    assert!(
        matches!(result, Err(Error::Bound("missing schema column"))),
        "{result:?}"
    );
}
