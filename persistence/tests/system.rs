#[cfg(test)]
mod query_support;

#[cfg(test)]
mod tests {
    use super::query_support::{Tables, table_source};
    use persistence::engine::Row;
    use persistence::query::TableSource;
    use persistence::schema::{
        BATTERY_FIELDS, CORE_STATE_FREQUENCY_FIELD, PROCESS_GPU_FIELD, SYSTEM_FIELDS, TABLES,
        TEMPERATURE_FIELDS, TICK_COLUMNS_MAX, Table,
    };
    use polars::prelude::DataType;

    #[test]
    fn gives_a_legacy_tick_typed_unavailable_system_readings() {
        // Goal: a recording made before the system readings stays readable with none invented.
        // Method: normalize an original tick and inspect every added column and its type.
        let tables = ticks();
        tables.insert("ticks", "1 AS publication", 1);

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let frame = normalized.query("SELECT * FROM normalized");

        for (name, kind) in SYSTEM_FIELDS {
            let column = frame.column(name).unwrap();
            assert_eq!(column.dtype(), &data_type(kind), "{name}");
            assert_eq!(column.null_count(), 1, "{name}");
        }
    }

    #[test]
    fn keeps_the_system_readings_a_tick_holds() {
        // Goal: a tick that carries the system readings reads back exactly as it was saved.
        // Method: stage every system column with values at their types' edges and read them.
        let tables = ticks();
        add_system_columns(&tables);
        tables.insert(
            "ticks",
            &format!("1 AS publication, {}", full_system_row()),
            1,
        );

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let frame = normalized.query(
            "SELECT load_average_15, boot_wall_ns, thermal_pressure, machine_chip, \
            machine_performance_level1_logical_cpus FROM normalized",
        );

        let row = Row::new(&frame, 0).unwrap();
        assert_eq!(row.get::<Option<f64>>(0).unwrap(), Some(2.25));
        assert_eq!(row.get::<Option<u64>>(1).unwrap(), Some(u64::MAX));
        assert_eq!(row.get::<Option<u8>>(2).unwrap(), Some(4));
        assert_eq!(
            row.get::<Option<String>>(3).unwrap().as_deref(),
            Some("Apple M5 Pro")
        );
        assert_eq!(row.get::<Option<u32>>(4).unwrap(), Some(12));
    }

    #[test]
    fn keeps_a_failed_system_reading_null_beside_the_others() {
        // Goal: a reading the collector did not get stays NULL rather than refusing the tick.
        // Method: stage every system column and leave the GPU memory and the levels NULL.
        let tables = ticks();
        add_system_columns(&tables);
        tables.insert(
            "ticks",
            "1 AS publication, 0.5::DOUBLE AS load_average_1",
            1,
        );

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let frame =
            normalized.query("SELECT load_average_1, gpu_memory_in_use_bytes FROM normalized");

        let row = Row::new(&frame, 0).unwrap();
        assert_eq!(row.get::<Option<f64>>(0).unwrap(), Some(0.5));
        assert_eq!(row.get::<Option<u64>>(1).unwrap(), None);
    }

    macro_rules! malformed_metadata {
        ($name:ident, $field:literal, $kind:literal) => {
            #[test]
            fn $name() {
                // Goal: system metadata is checked even when an empty source has no data.
                // Method: expose one system column with an invalid property.
                let tables = ticks();
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

    malformed_metadata!(rejects_a_lone_load_average, "load_average_1", "DOUBLE");
    malformed_metadata!(rejects_a_lone_machine_model, "machine_model", "VARCHAR");
    malformed_metadata!(
        rejects_a_load_average_spelled_in_capitals,
        "Load_Average_1",
        "DOUBLE"
    );
    malformed_metadata!(rejects_open_files_of_another_type, "open_files", "INTEGER");

    #[test]
    fn rejects_a_system_cell_no_collector_writes() {
        // Goal: a hand-made or damaged source cannot put a made-up reading in the group.
        // Method: admit the whole group with one bad cell at a time.
        let long = "a".repeat(collector::system::FACT_BYTES_MAX + 1);
        let cells = [
            "'NaN'::DOUBLE AS load_average_5".to_owned(),
            "-1.0::DOUBLE AS load_average_1".to_owned(),
            "'' AS machine_model".to_owned(),
            format!("'{long}' AS machine_chip"),
            "'Super' AS machine_performance_level0_name".to_owned(),
        ];
        for cell in cells {
            let tables = ticks();
            add_system_columns(&tables);
            tables.insert("ticks", &format!("1 AS publication, {cell}"), 1);

            let result = table_source(&tables, Table::Ticks, TableSource::Staged);

            assert!(
                matches!(result, Err(persistence::Error::Bound(_))),
                "{cell}: {:?}",
                result.err()
            );
        }
    }

    #[test]
    fn admits_system_readings_that_fill_the_column_bound() {
        // Goal: the bound refuses one column too many and no earlier.
        // Method: fill a source that has every other extension to exactly the room left.
        let tables = ticks_with_other_extensions(0);

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();

        assert_eq!(
            normalized.query("SELECT * FROM normalized").width(),
            TICK_COLUMNS_MAX
        );
    }

    #[test]
    fn refuses_system_readings_that_would_exceed_the_column_bound() {
        // Goal: the system readings join a tick source only where the whole bound still holds.
        // Method: fill a source that has every other extension to one column past the room.
        let tables = ticks_with_other_extensions(1);

        let result = table_source(&tables, Table::Ticks, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn gives_a_legacy_core_state_an_unknown_clock() {
        // Goal: a window sealed before the core clocks reads each state's clock as unknown.
        // Method: normalize an original core state row and read the added column.
        let tables = Tables::new();
        tables.create("core_states", TABLES[5].1);
        tables.insert("core_states", "1 AS publication, 0 AS core, 2 AS state", 1);

        let normalized = table_source(&tables, Table::CoreStates, TableSource::Staged).unwrap();
        let frame = normalized.query("SELECT frequency_khz FROM normalized");

        let column = frame.column(CORE_STATE_FREQUENCY_FIELD.0).unwrap();
        assert_eq!(column.dtype(), &DataType::UInt32);
        assert_eq!(column.null_count(), 1);
    }

    #[test]
    fn keeps_the_clock_a_core_state_holds() {
        // Goal: a saved clock reads back as it was saved.
        // Method: stage a core state with its clock and read it.
        let tables = Tables::new();
        tables.create("core_states", TABLES[5].1);
        tables.add_column("core_states", "frequency_khz", "UINTEGER");
        tables.insert(
            "core_states",
            "1 AS publication, 0 AS core, 2 AS state, 4608000 AS frequency_khz",
            1,
        );

        let normalized = table_source(&tables, Table::CoreStates, TableSource::Staged).unwrap();
        let frame = normalized.query("SELECT frequency_khz FROM normalized");

        let row = Row::new(&frame, 0).unwrap();
        assert_eq!(row.get::<Option<u32>>(0).unwrap(), Some(4_608_000));
    }

    #[test]
    fn rejects_a_core_state_clock_of_another_type() {
        // Goal: a clock column the writer never declares is refused rather than misread.
        // Method: expose the clock as a signed integer.
        let tables = Tables::new();
        tables.create("core_states", TABLES[5].1);
        tables.add_column("core_states", "frequency_khz", "BIGINT");

        let result = table_source(&tables, Table::CoreStates, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn gives_a_legacy_process_no_gpu_time() {
        // Goal: a window sealed before the GPU time reads each process's GPU time as unknown.
        // Method: normalize an original process row and read the added column.
        let tables = Tables::new();
        tables.create("processes", TABLES[7].1);
        tables.insert("processes", "1 AS publication, 7 AS pid", 1);

        let normalized = table_source(&tables, Table::Processes, TableSource::Staged).unwrap();
        let frame = normalized.query("SELECT gpu_ns FROM normalized");

        let column = frame.column(PROCESS_GPU_FIELD.0).unwrap();
        assert_eq!(column.dtype(), &DataType::UInt64);
        assert_eq!(column.null_count(), 1);
    }

    #[test]
    fn rejects_a_process_gpu_time_spelled_in_capitals() {
        // Goal: a GPU time column the writer never declares is refused rather than ignored.
        // Method: expose the column in capitals on a legacy process source.
        let tables = Tables::new();
        tables.create("processes", TABLES[7].1);
        tables.add_column("processes", "GPU_NS", "UBIGINT");

        let result = table_source(&tables, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    fn full_system_row() -> String {
        "0.5::DOUBLE AS load_average_1, 1.0::DOUBLE AS load_average_5, \
        2.25::DOUBLE AS load_average_15, 9000 AS open_files, 245760 AS open_files_limit, \
        4 AS thermal_pressure, 1024 AS gpu_memory_in_use_bytes, \
        2048 AS gpu_memory_allocated_bytes, 18446744073709551615::UBIGINT AS boot_wall_ns, \
        'Mac17,1' AS machine_model, 'Apple M5 Pro' AS machine_chip, \
        'arm64' AS machine_architecture, 18 AS machine_cpu_cores, 20 AS machine_gpu_cores, \
        51539607552 AS machine_memory_bytes, 'Super' AS machine_performance_level0_name, \
        6 AS machine_performance_level0_logical_cpus, \
        'Performance' AS machine_performance_level1_name, \
        12 AS machine_performance_level1_logical_cpus"
            .to_owned()
    }

    /// A tick source with the battery, the disks and the temperatures, and enough columns
    /// besides that the system readings take the room left and `past` columns more.
    fn ticks_with_other_extensions(past: usize) -> Tables {
        let tables = ticks();
        for (name, kind) in BATTERY_FIELDS {
            tables.add_column("ticks", name, kind);
        }
        tables.add_column("ticks", "physical_disks", "VARCHAR");
        for (name, kind) in TEMPERATURE_FIELDS {
            tables.add_column("ticks", name, kind);
        }
        let used =
            TABLES[0].1.split(',').count() + BATTERY_FIELDS.len() + 1 + TEMPERATURE_FIELDS.len();
        for extra in 0..TICK_COLUMNS_MAX - used - SYSTEM_FIELDS.len() + past {
            tables.add_column("ticks", &format!("extra_{extra}"), "UBIGINT");
        }
        tables
    }

    fn add_system_columns(tables: &Tables) {
        for (name, kind) in SYSTEM_FIELDS {
            tables.add_column("ticks", name, kind);
        }
    }

    fn ticks() -> Tables {
        let tables = Tables::new();
        tables.create("ticks", TABLES[0].1);
        tables
    }

    fn data_type(kind: &str) -> DataType {
        match kind {
            "UTINYINT" => DataType::UInt8,
            "UINTEGER" => DataType::UInt32,
            "UBIGINT" => DataType::UInt64,
            "DOUBLE" => DataType::Float64,
            "VARCHAR" => DataType::String,
            _ => panic!("no system field is {kind}"),
        }
    }
}
