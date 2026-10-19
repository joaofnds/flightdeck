use super::*;
use collector::machine::{CPUTicks, Memory};
use collector::ring::{Identity, RunIdentity, Shape, Tick, TickBuffers};
use persistence::{Begin, Encoder, Limits, Store, Window, WindowIdentity};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Recording {
    root: PathBuf,
    window: Window,
}

impl Recording {
    fn new() -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("flightdeck-scalar-{}-{serial}", std::process::id()));
        let store = Store::open(&root).unwrap();
        let identity = WindowIdentity {
            run: RunIdentity {
                recording: store.recording(),
                run: Identity([11, 12]),
                boot: Identity([13, 14]),
            },
            window: 0,
        };
        let shape = Shape {
            slot_count: 2,
            cpu_capacity: 2,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 2,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 8,
        };
        let mut window = Window::new(identity, Limits::from_shape(&shape).unwrap());
        for publication in 1..=2 {
            let tick = tick(publication == 2);
            window
                .push_tick(
                    &persistence::Observation {
                        publication,
                        wall_ns: Some((999 + publication) * 1_000_000_000),
                        continuous_ns: Some((publication * 2 - 1) * 1_000_000_000),
                        awake_ns: Some(publication * 1_000_000_000),
                    },
                    &tick,
                    &[],
                )
                .unwrap();
        }
        Self { root, window }
    }

    fn project(&self) -> Tables {
        let connection = crate::data::connection();
        let control = Arc::new(crate::Cancellation::default());
        let operation = control.operation(0).unwrap();
        let mut tables = Tables::new();
        for (kind, _, order) in persistence::schema::TABLES {
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

    fn seal_with(&self, adjust: impl FnOnce(&Path)) {
        let store = Store::open(&self.root).unwrap();
        let identity = self.window.identity;
        let Begin::Pending(pending) = store.begin(&identity).unwrap() else {
            panic!("fixture already sealed");
        };
        let tables = Encoder.encode(&self.window, pending.path()).unwrap();
        adjust(pending.path());
        store
            .commit(
                pending,
                persistence::Manifest {
                    identity,
                    publication_first: 1,
                    publication_end: 3,
                    events_first: 0,
                    events_end: 0,
                    wall_ns: None,
                    continuous_ns: None,
                    awake_ns: None,
                    losses: persistence::Losses::default(),
                    data_directory: String::new(),
                    tables,
                },
            )
            .unwrap();
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
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
    // Method: admit a two-column empty Parquet before pinning it, then select publicly.
    use polars::polars_utils::compression::ZstdLevel;
    use polars::prelude::{
        Column, DataFrame, ParquetCompression, ParquetWriter, StatisticsOptions,
    };

    let recording = Recording::new();
    recording.seal_with(|pending| {
        let fans = pending.join("fans.parquet");
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
        ParquetWriter::new(std::fs::File::create(&fans).unwrap())
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
