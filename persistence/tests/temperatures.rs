#[cfg(test)]
#[path = "query_support/append.rs"]
mod append_support;
#[cfg(test)]
mod query_support;

#[cfg(test)]
mod tests {
    use super::query_support::normalized::Normalized;
    use super::query_support::{Tables, table_source};
    use collector::disks::{DiskSnapshot, ScanState};
    use persistence::engine::Row;
    use persistence::query::TableSource;
    use persistence::schema::{
        BATTERY_FIELDS, SYSTEM_FIELDS, TABLES, TEMPERATURE_FIELDS, TICK_COLUMNS_MAX, Table,
    };

    #[test]
    fn gives_a_legacy_tick_typed_unavailable_temperatures() {
        // Goal: a recording made before the temperatures stays readable with none invented.
        // Method: normalize an original tick and inspect the two added columns and their types.
        let tables = tables();
        tables.insert("ticks", "1 AS publication, 51.5::FLOAT AS soc_celsius", 1);

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let frame = normalized.query("SELECT cpu_celsius,gpu_celsius FROM normalized");

        assert_eq!(temperature_rows(&normalized), [(None, None)]);
        assert_eq!(
            frame.column("cpu_celsius").unwrap().dtype(),
            &polars::prelude::DataType::Float32
        );
        assert_eq!(
            frame.column("gpu_celsius").unwrap().dtype(),
            &polars::prelude::DataType::Float32
        );
    }

    #[test]
    fn keeps_every_tick_extension_within_the_column_bound() {
        // Goal: the battery, the disks and the temperatures fit one tick table together.
        // Method: normalize a legacy tick and a tick that carries all three extensions.
        let legacy = table_source(&tables(), Table::Ticks, TableSource::Staged).unwrap();
        let extended = Tables::new();
        let mut window = window();
        window
            .push_tick(&observation(1), &full_tick(), &[])
            .unwrap();
        extended.create("ticks", &persistence::table_columns(Table::Ticks, &window));
        super::append_support::append(&extended, Table::Ticks, &window);

        let extended = table_source(&extended, Table::Ticks, TableSource::Staged).unwrap();

        assert_eq!(legacy.query("SELECT * FROM normalized").width(), 53);
        assert_eq!(extended.query("SELECT * FROM normalized").width(), 53);
    }

    #[test]
    fn saves_the_temperatures_a_window_captured() {
        // Goal: staging and Parquet keep each temperature, and a missing sensor stays NULL.
        // Method: encode ticks with both, with one, and with none, then compare both sources.
        let directory = Directory::new();
        let mut window = window();
        let mut tick = persistence::tick_buffer(&shape()).unwrap();
        tick.cpu_celsius = Some(71.5);
        tick.gpu_celsius = Some(63.25);
        window.push_tick(&observation(1), &tick, &[]).unwrap();
        tick.cpu_celsius = None;
        window.push_tick(&observation(2), &tick, &[]).unwrap();
        tick.gpu_celsius = None;
        window.push_tick(&observation(3), &tick, &[]).unwrap();
        let tables = Tables::new();
        tables.create("ticks", &persistence::table_columns(Table::Ticks, &window));
        super::append_support::append(&tables, Table::Ticks, &window);

        persistence::Encoder.encode(&window, &directory.0).unwrap();
        let staged = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let saved = table_source(
            &tables,
            Table::Ticks,
            TableSource::Parquet(&directory.0.join("ticks.parquet")),
        )
        .unwrap();

        let expected = [(Some(71.5), Some(63.25)), (None, Some(63.25)), (None, None)];
        assert_eq!(temperature_rows(&staged), expected);
        assert_eq!(temperature_rows(&saved), expected);
    }

    #[test]
    fn leaves_the_columns_out_of_a_window_that_captured_no_temperature() {
        // Goal: a window with no temperature keeps the columns it had before the extension.
        // Method: stage a tick without temperatures and read its declared column list.
        let mut window = window();
        let tick = persistence::tick_buffer(&shape()).unwrap();
        window.push_tick(&observation(1), &tick, &[]).unwrap();

        let columns = persistence::table_columns(Table::Ticks, &window);

        assert_eq!(columns, TABLES[0].1);
    }

    #[test]
    fn declares_both_temperatures_where_a_window_captured_one() {
        // Goal: the extension is whole or absent, so the normalizer never sees half of it.
        // Method: stage a tick that has only the GPU sensor and read its declared columns.
        let mut window = window();
        let mut tick = persistence::tick_buffer(&shape()).unwrap();
        tick.gpu_celsius = Some(60.0);
        window.push_tick(&observation(1), &tick, &[]).unwrap();

        let columns = persistence::table_columns(Table::Ticks, &window);

        assert_eq!(
            columns,
            format!("{}, cpu_celsius FLOAT, gpu_celsius FLOAT", TABLES[0].1)
        );
    }

    #[test]
    fn preserves_battery_columns_when_the_temperatures_are_absent() {
        // Goal: temperature normalization leaves every earlier extension as it found it.
        // Method: normalize a battery tick and read back its exact high unsigned count.
        let tables = tables();
        for (name, kind) in BATTERY_FIELDS {
            tables.add_column("ticks", name, kind);
        }
        tables.insert(
            "ticks",
            "1 AS publication,1 AS battery_version,2 AS battery_source_continuous_ns, \
            18446744073709551615::UBIGINT AS battery_cycle_count",
            1,
        );

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let frame = normalized.query("SELECT battery_cycle_count FROM normalized");

        let row = Row::new(&frame, 0).unwrap();
        assert_eq!(row.get::<Option<u64>>(0).unwrap(), Some(u64::MAX));
        assert_eq!(temperature_rows(&normalized), [(None, None)]);
    }

    macro_rules! malformed_metadata {
        ($name:ident, $field:literal, $kind:literal) => {
            #[test]
            fn $name() {
                // Goal: temperature metadata is checked even when an empty source has no data.
                // Method: expose one temperature column with an invalid property.
                let tables = tables();
                tables.add_column("ticks", $field, $kind);

                let result = table_source(&tables, Table::Ticks, TableSource::Staged);

                assert!(
                    matches!(result, Err(persistence::Error::Bound(_))),
                    "{:?}",
                    result.err()
                );
            }
        };
    }

    malformed_metadata!(rejects_a_lone_cpu_temperature, "cpu_celsius", "FLOAT");
    malformed_metadata!(rejects_a_lone_gpu_temperature, "gpu_celsius", "FLOAT");
    malformed_metadata!(
        rejects_a_cpu_temperature_spelled_in_capitals,
        "CPU_Celsius",
        "FLOAT"
    );
    malformed_metadata!(
        rejects_a_cpu_temperature_of_another_type,
        "cpu_celsius",
        "UBIGINT"
    );

    #[test]
    fn rejects_a_temperature_that_is_no_finite_number_above_zero() {
        // Goal: a hand-made or damaged source cannot put a made-up reading in a temperature.
        // Method: admit both columns with one negative cell and one that is not a number.
        for cell in ["-1.0::FLOAT", "'NaN'::FLOAT", "0.0::FLOAT"] {
            let tables = tables();
            tables.add_column("ticks", "cpu_celsius", "FLOAT");
            tables.add_column("ticks", "gpu_celsius", "FLOAT");
            tables.insert(
                "ticks",
                &format!("1 AS publication, {cell} AS cpu_celsius"),
                1,
            );

            let result = table_source(&tables, Table::Ticks, TableSource::Staged);

            assert!(
                matches!(result, Err(persistence::Error::Bound(_))),
                "{cell}: {:?}",
                result.err()
            );
        }
    }

    #[test]
    fn refuses_temperatures_that_would_exceed_the_column_bound() {
        // Goal: the temperatures join a tick source only where the whole bound still holds.
        // Method: fill a source that has the other extensions up to one column under the bound.
        let tables = tables();
        for (name, kind) in BATTERY_FIELDS {
            tables.add_column("ticks", name, kind);
        }
        tables.add_column("ticks", "physical_disks", "VARCHAR");
        let used = TABLES[0].1.split(',').count() + BATTERY_FIELDS.len() + 1;
        for extra in 0..=TICK_COLUMNS_MAX - used - TEMPERATURE_FIELDS.len() {
            tables.add_column("ticks", &format!("extra_{extra}"), "UBIGINT");
        }

        let result = table_source(&tables, Table::Ticks, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn admits_temperatures_that_fill_the_column_bound() {
        // Goal: the bound refuses one column too many and no earlier.
        // Method: fill a source that has the other extensions to exactly the room the
        // temperatures and the system readings leave.
        let tables = tables();
        for (name, kind) in BATTERY_FIELDS {
            tables.add_column("ticks", name, kind);
        }
        tables.add_column("ticks", "physical_disks", "VARCHAR");
        let used = TABLES[0].1.split(',').count() + BATTERY_FIELDS.len() + 1;
        for extra in 0..TICK_COLUMNS_MAX - used - TEMPERATURE_FIELDS.len() - SYSTEM_FIELDS.len() {
            tables.add_column("ticks", &format!("extra_{extra}"), "UBIGINT");
        }

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();

        assert_eq!(
            normalized.query("SELECT * FROM normalized").width(),
            TICK_COLUMNS_MAX
        );
    }

    fn temperature_rows(normalized: &Normalized) -> Vec<(Option<f32>, Option<f32>)> {
        let frame =
            normalized.query("SELECT cpu_celsius,gpu_celsius FROM normalized ORDER BY publication");

        (0..frame.height())
            .map(|index| {
                let row = Row::new(&frame, index).unwrap();
                (row.get(0).unwrap(), row.get(1).unwrap())
            })
            .collect()
    }

    fn tables() -> Tables {
        let tables = Tables::new();
        tables.create("ticks", TABLES[0].1);
        tables
    }

    fn full_tick() -> collector::ring::Tick {
        let mut tick = persistence::tick_buffer(&shape()).unwrap();
        tick.cpu_celsius = Some(70.0);
        tick.gpu_celsius = Some(60.0);
        tick.battery = Some(collector::battery::BatteryReading {
            source_continuous_ns: 1,
            ..collector::battery::BatteryReading::default()
        });
        tick.disks = Some(DiskSnapshot {
            source_continuous_ns: 1,
            state: ScanState::Complete,
            ..DiskSnapshot::default()
        });
        tick
    }

    fn shape() -> collector::ring::Shape {
        collector::ring::Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 2,
        }
    }

    fn window() -> persistence::Window {
        use collector::ring::{Identity, RunIdentity};
        persistence::Window::new(
            persistence::WindowIdentity {
                run: RunIdentity {
                    recording: Identity([1, 2]),
                    run: Identity([3, 4]),
                    boot: Identity([5, 6]),
                },
                window: 0,
            },
            persistence::Limits::from_shape(&shape()).unwrap(),
        )
    }

    fn observation(publication: u64) -> persistence::Observation {
        persistence::Observation {
            publication,
            wall_ns: Some(publication),
            continuous_ns: Some(publication),
            awake_ns: Some(publication),
        }
    }

    struct Directory(std::path::PathBuf);

    impl Directory {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "temperature-persistence-{}-{number}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}
