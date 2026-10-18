#[cfg(test)]
mod tests {
    use collector::battery::BatteryReading;
    use collector::ring::{Identity, RunIdentity, Shape};
    use duckdb::Connection;
    use duckdb::types::Value;
    use persistence::query::{TableSource, table_source};
    use persistence::schema::{BATTERY_FIELDS, TABLES, Table};
    use persistence::{Encoder, Limits, Observation, Window, WindowIdentity};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn preserves_legacy_ticks_with_typed_unavailable_battery_fields() {
        // Goal: old recordings stay readable without fabricated battery observations.
        // Method: normalize an original tick and inspect its nullable fields and exact counter.
        let connection = connection();
        connection
            .execute_batch(
                "INSERT INTO ticks(publication,gpu_energy_nj) VALUES(1,9007199254740993)",
            )
            .unwrap();

        let source = table_source(&connection, Table::Ticks, TableSource::Staged).unwrap();
        let actual = connection
            .query_row(
                &format!(
                    "SELECT gpu_energy_nj,battery_version,battery_source_continuous_ns, \
                battery_current_ma,typeof(battery_cycle_count),typeof(battery_charging) \
                FROM {source} LIMIT 1"
                ),
                [],
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, Option<u8>>(1)?,
                        row.get::<_, Option<u64>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(
            actual,
            (
                9_007_199_254_740_993,
                None,
                None,
                None,
                "UBIGINT".into(),
                "BOOLEAN".into()
            )
        );
    }

    fn connection() -> Connection {
        let config = duckdb::Config::default().threads(1).unwrap();
        let connection = Connection::open_in_memory_with_flags(config).unwrap();
        connection
            .execute_batch(
                "SET memory_limit='64MiB'; SET max_temp_directory_size='0B'; \
            SET autoinstall_known_extensions=false; SET autoload_known_extensions=false;",
            )
            .unwrap();
        connection
            .execute_batch(&format!("CREATE TABLE ticks ({})", TABLES[0].1))
            .unwrap();
        connection
    }

    #[test]
    fn preserves_complete_raw_nullable_battery_readings() {
        // Goal: exact counts, signed current, raw zero and independent absence survive reading.
        // Method: normalize an extended staged tick with values beyond floating-point precision.
        let connection = battery_connection();
        connection
            .execute_batch(
                "INSERT INTO ticks(publication,battery_version,battery_source_continuous_ns, \
            battery_charge_current,battery_charge_maximum,battery_full_charge_capacity, \
            battery_design_capacity,battery_cycle_count,battery_charging,battery_external_power, \
            battery_voltage_mv,battery_current_ma,battery_adapter_watts) VALUES \
            (1,1,0,0,0,9007199254740993,9007199254740991,18446744073709551615, \
            false,true,NULL,-9223372036854775808,0)",
            )
            .unwrap();

        let source = table_source(&connection, Table::Ticks, TableSource::Staged).unwrap();
        let actual = read_battery(&connection, &source);

        assert_eq!(actual, expected_reading());
    }

    #[test]
    fn preserves_an_extended_row_without_a_battery_snapshot() {
        // Goal: source absence is SQL NULL, with the complete extension version still present.
        // Method: normalize one versioned row whose payload is wholly absent.
        let connection = battery_connection();
        connection
            .execute_batch("INSERT INTO ticks(publication,battery_version) VALUES(1,1)")
            .unwrap();

        let source = table_source(&connection, Table::Ticks, TableSource::Staged).unwrap();
        let actual = read_battery(&connection, &source);

        let mut expected = vec![Value::Null; 12];
        expected[0] = Value::UTinyInt(1);
        assert_eq!(actual, expected);
    }

    macro_rules! malformed_extension {
        ($name:ident, $change:literal) => {
            #[test]
            fn $name() {
                // Goal: malformed optional data fails at admission before consumers bind queries.
                // Method: change one property of a complete typed battery table.
                let connection = battery_connection();
                connection.execute_batch($change).unwrap();

                let result = table_source(&connection, Table::Ticks, TableSource::Staged);

                assert!(
                    matches!(result, Err(persistence::Error::Bound(_))),
                    "{result:?}"
                );
            }
        };
    }

    malformed_extension!(
        rejects_partial_empty_battery_metadata,
        "ALTER TABLE ticks DROP COLUMN battery_current_ma"
    );
    malformed_extension!(
        rejects_wrong_types_even_in_empty_battery_metadata,
        "ALTER TABLE ticks ALTER COLUMN battery_current_ma TYPE UBIGINT"
    );
    malformed_extension!(
        rejects_wrong_spelling_even_in_empty_battery_metadata,
        "ALTER TABLE ticks RENAME COLUMN battery_charging TO Battery_Charging"
    );
    malformed_extension!(
        rejects_an_unknown_battery_version,
        "INSERT INTO ticks(publication,battery_version) VALUES(1,2)"
    );
    malformed_extension!(
        rejects_a_missing_battery_version,
        "INSERT INTO ticks(publication) VALUES(1)"
    );
    malformed_extension!(
        rejects_battery_payload_without_its_source_time,
        "INSERT INTO ticks(publication,battery_version,battery_cycle_count) VALUES(1,1,0)"
    );
    malformed_extension!(
        rejects_more_than_sixty_one_extended_ticks,
        "INSERT INTO ticks(publication,battery_version) SELECT range+1,1 FROM range(62)"
    );
    malformed_extension!(
        rejects_false_payload_without_source_time,
        "INSERT INTO ticks(publication,battery_version,battery_charging) VALUES(1,1,false)"
    );
    malformed_extension!(
        rejects_signed_current_without_source_time,
        "INSERT INTO ticks(publication,battery_version,battery_current_ma) VALUES(1,1,-1)"
    );
    malformed_extension!(
        rejects_a_battery_row_without_its_tick_publication,
        "INSERT INTO ticks(battery_version,battery_source_continuous_ns) VALUES(1,0)"
    );
    malformed_extension!(
        rejects_unknown_battery_column_names,
        "ALTER TABLE ticks ADD COLUMN battery_charge_typo UBIGINT"
    );
    malformed_extension!(
        rejects_extended_tick_tables_exceeding_the_column_bound,
        "ALTER TABLE ticks ADD COLUMN extra_one UBIGINT; \
    ALTER TABLE ticks ADD COLUMN extra_two UBIGINT"
    );

    #[test]
    fn preserves_a_source_timestamp_without_any_independent_payload_field() {
        // Goal: each field can remain unavailable while a real source snapshot still exists.
        // Method: normalize one timestamped reading with every independent field unavailable.
        let connection = battery_connection();
        connection.execute_batch(
        "INSERT INTO ticks(publication,battery_version,battery_source_continuous_ns) VALUES(1,1,0)"
    ).unwrap();

        let source = table_source(&connection, Table::Ticks, TableSource::Staged).unwrap();
        let actual = read_battery(&connection, &source);

        let mut expected = vec![Value::Null; 12];
        expected[0] = Value::UTinyInt(1);
        expected[1] = Value::UBigInt(0);
        assert_eq!(actual, expected);
    }

    #[test]
    fn refuses_legacy_projection_that_would_exceed_the_column_bound() {
        // Goal: typed normalization fits the same bounded column set as captured extensions.
        // Method: add two unrelated columns to an original table before typed projection.
        let connection = connection();
        connection
            .execute_batch(
                "ALTER TABLE ticks ADD COLUMN extra_one UBIGINT; \
        ALTER TABLE ticks ADD COLUMN extra_two UBIGINT",
            )
            .unwrap();

        let result = table_source(&connection, Table::Ticks, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn preserves_stale_and_future_source_times_without_erasing_other_history() {
        // Goal: history retains raw timestamps so consumers can apply freshness per field.
        // Method: normalize a 61-row window, including an old source and a future source.
        let connection = battery_connection();
        connection
            .execute_batch(
                "INSERT INTO ticks(publication,continuous_ns,battery_version, \
        battery_source_continuous_ns,battery_cycle_count) \
        SELECT range+1,70000000000,1,range*70000000000,range FROM range(61)",
            )
            .unwrap();

        let source = table_source(&connection, Table::Ticks, TableSource::Staged).unwrap();
        let actual = connection.query_row(&format!(
        "SELECT count(*),min(battery_source_continuous_ns),max(battery_source_continuous_ns), \
        sum(battery_cycle_count)::UBIGINT FROM {source}"
    ), [], |row| Ok((row.get::<_, u64>(0)?, row.get::<_, u64>(1)?,
        row.get::<_, u64>(2)?, row.get::<_, u64>(3)?))).unwrap();

        assert_eq!(actual, (61, 0, 4_200_000_000_000, 1830));
    }

    #[test]
    fn rejects_a_malformed_empty_saved_battery_table() {
        // Goal: saved metadata is validated even when no payload rows can reveal its defects.
        // Method: write an empty Parquet extension with a wrong boolean storage type.
        let directory = Directory::new();
        let connection = battery_connection();
        connection
            .execute_batch("ALTER TABLE ticks ALTER COLUMN battery_charging TYPE UTINYINT")
            .unwrap();
        let path = directory.0.join("ticks.parquet");
        let escaped = persistence::query::parquet_source(&path).unwrap();
        connection
            .execute_batch(&format!("COPY ticks TO '{escaped}' (FORMAT PARQUET)"))
            .unwrap();

        let result = table_source(&connection, Table::Ticks, TableSource::Parquet(&path));

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn accepts_complete_empty_battery_metadata() {
        // Goal: empty valid typed files remain readable.
        // Method: validate a complete zero-row tick extension.
        let connection = battery_connection();

        let result = table_source(&connection, Table::Ticks, TableSource::Staged);

        assert!(result.is_ok(), "{result:?}");
    }

    fn battery_connection() -> Connection {
        let connection = connection();
        for (field, kind) in BATTERY_FIELDS {
            connection
                .execute_batch(&format!("ALTER TABLE ticks ADD COLUMN {field} {kind}"))
                .unwrap();
        }
        connection
    }

    fn read_battery(connection: &Connection, source: &str) -> Vec<Value> {
        let fields = BATTERY_FIELDS.map(|(field, _)| field).join(",");
        connection
            .query_row(
                &format!("SELECT {fields} FROM {source} ORDER BY publication LIMIT 1"),
                [],
                |row| (0..12).map(|index| row.get(index)).collect(),
            )
            .unwrap()
    }

    #[test]
    fn stages_owned_battery_values_and_encodes_the_same_nullable_parquet() {
        // Goal: staging owns captured data and saved/live readers expose identical raw fields.
        // Method: overwrite the caller's reading after admission, encode, then compare sources.
        let directory = Directory::new();
        let mut window = window();
        let mut tick = persistence::tick_buffer(&shape()).unwrap();
        tick.battery = Some(reading());
        window.push_tick(&observation(1), &tick, &[]).unwrap();
        tick.battery = None;
        window.push_tick(&observation(2), &tick, &[]).unwrap();
        let connection = connection();
        connection.execute_batch("DROP TABLE ticks").unwrap();
        connection
            .execute_batch(&format!(
                "CREATE TABLE ticks ({})",
                persistence::table_columns(Table::Ticks, &window)
            ))
            .unwrap();
        persistence::append(&connection, Table::Ticks, &window).unwrap();

        let tables = Encoder.encode(&window, &directory.0).unwrap();
        let staged = table_source(&connection, Table::Ticks, TableSource::Staged).unwrap();
        let path = directory.0.join("ticks.parquet");
        let saved = table_source(&connection, Table::Ticks, TableSource::Parquet(&path)).unwrap();

        assert_eq!(tables.len(), 10);
        assert_eq!(read_battery(&connection, &staged), expected_reading());
        assert_eq!(read_battery(&connection, &saved), expected_reading());
        let mut unavailable = vec![Value::Null; 12];
        unavailable[0] = Value::UTinyInt(1);
        assert_eq!(
            read_battery(
                &connection,
                &format!("(SELECT * FROM {saved} WHERE publication=2)")
            ),
            unavailable
        );
        let absent: u64 = connection
            .query_row(
                &format!(
                    "SELECT count(*) FROM {saved} WHERE publication=2 \
            AND battery_version=1 AND battery_source_continuous_ns IS NULL \
            AND battery_cycle_count IS NULL AND battery_charging IS NULL \
            AND gpu_energy_nj IS NULL AND power_watts IS NULL AND soc_celsius IS NULL"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(absent, 1);
    }

    #[test]
    fn retains_the_original_schema_for_windows_without_battery_snapshots() {
        // Goal: the schema-1 ten tables and original nineteen tick columns stay intact.
        // Method: encode one unextended tick and inspect raw metadata before normalization.
        let directory = Directory::new();
        let mut window = window();
        let tick = persistence::tick_buffer(&shape()).unwrap();
        window.push_tick(&observation(1), &tick, &[]).unwrap();

        let tables = Encoder.encode(&window, &directory.0).unwrap();

        assert_eq!(persistence::SCHEMA_VERSION, 1);
        assert_eq!(Table::Ticks.columns().count(), 19);
        assert_eq!(
            persistence::table_columns(Table::Ticks, &window),
            TABLES[0].1
        );
        let names: Vec<_> = tables.iter().map(|table| table.name).collect();
        assert_eq!(
            names,
            [
                "ticks",
                "memory",
                "cpus",
                "interfaces",
                "gpu_states",
                "core_states",
                "fans",
                "processes",
                "coalitions",
                "events"
            ]
        );
        let connection = connection();
        let path = directory.0.join("ticks.parquet");
        let source = table_source(&connection, Table::Ticks, TableSource::Parquet(&path)).unwrap();
        let expected = vec![Value::Null; 12];
        assert_eq!(read_battery(&connection, &source), expected);
    }

    fn reading() -> BatteryReading {
        BatteryReading {
            source_continuous_ns: 0,
            charge_current: Some(0),
            charge_maximum: Some(0),
            full_charge_capacity: Some(9_007_199_254_740_993),
            design_capacity: Some(9_007_199_254_740_991),
            cycle_count: Some(u64::MAX),
            charging: Some(false),
            external_power: Some(true),
            voltage_mv: None,
            current_ma: Some(i64::MIN),
            adapter_watts: Some(0),
        }
    }

    fn expected_reading() -> Vec<Value> {
        vec![
            Value::UTinyInt(1),
            Value::UBigInt(0),
            Value::UBigInt(0),
            Value::UBigInt(0),
            Value::UBigInt(9_007_199_254_740_993),
            Value::UBigInt(9_007_199_254_740_991),
            Value::UBigInt(u64::MAX),
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Null,
            Value::BigInt(i64::MIN),
            Value::UBigInt(0),
        ]
    }

    fn shape() -> Shape {
        Shape {
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

    fn window() -> Window {
        Window::new(
            WindowIdentity {
                run: RunIdentity {
                    recording: Identity([1, 2]),
                    run: Identity([3, 4]),
                    boot: Identity([5, 6]),
                },
                window: 0,
            },
            Limits::from_shape(&shape()).unwrap(),
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

    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "battery-persistence-{}-{number}",
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
