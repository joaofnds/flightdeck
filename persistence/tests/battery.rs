#[cfg(test)]
#[path = "query_support/append.rs"]
mod append_support;
#[cfg(test)]
mod query_support;
#[cfg(test)]
#[path = "query_support/saved.rs"]
mod saved;

#[cfg(test)]
mod tests {
    use super::query_support::normalized::Normalized;
    use super::query_support::{Tables, table_source};
    use super::saved;
    use collector::battery::BatteryReading;
    use collector::ring::{Identity, RunIdentity, Shape};
    use persistence::query::TableSource;
    use persistence::schema::{BATTERY_FIELDS, TABLES, Table};
    use persistence::{Encoder, Limits, Observation, Window, WindowIdentity};
    use polars::prelude::{AnyValue, DataFrame, DataType};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn preserves_legacy_ticks_with_typed_unavailable_battery_fields() {
        // Goal: old recordings stay readable without fabricated battery observations.
        // Method: normalize an original tick and inspect its nullable fields and exact counter.
        let tables = tables();
        tables.insert(
            "ticks",
            "1 AS publication, 9007199254740993 AS gpu_energy_nj",
            1,
        );

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let frame = normalized.query(
            "SELECT gpu_energy_nj,battery_version,battery_source_continuous_ns, \
            battery_current_ma,battery_cycle_count,battery_charging FROM normalized LIMIT 1",
        );

        assert_eq!(
            row(&frame),
            [
                AnyValue::UInt64(9_007_199_254_740_993),
                AnyValue::Null,
                AnyValue::Null,
                AnyValue::Null,
                AnyValue::Null,
                AnyValue::Null,
            ]
        );
        assert_eq!(
            frame.column("battery_cycle_count").unwrap().dtype(),
            &DataType::UInt64
        );
        assert_eq!(
            frame.column("battery_charging").unwrap().dtype(),
            &DataType::Boolean
        );
    }

    fn tables() -> Tables {
        let tables = Tables::new();
        tables.create("ticks", TABLES[0].1);
        tables
    }

    #[test]
    fn preserves_complete_raw_nullable_battery_readings() {
        // Goal: exact counts, signed current, raw zero and independent absence survive reading.
        // Method: normalize an extended staged tick with values beyond floating-point precision.
        let tables = battery_tables();
        tables.insert(
            "ticks",
            "1 AS publication,1 AS battery_version,0 AS battery_source_continuous_ns, \
            0 AS battery_charge_current,0 AS battery_charge_maximum, \
            9007199254740993 AS battery_full_charge_capacity, \
            9007199254740991 AS battery_design_capacity, \
            18446744073709551615::UBIGINT AS battery_cycle_count,false AS battery_charging, \
            true AS battery_external_power,NULL AS battery_voltage_mv, \
            (-9223372036854775808)::BIGINT AS battery_current_ma,0 AS battery_adapter_watts",
            1,
        );

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let actual = read_battery(&normalized, "");

        assert_eq!(actual, expected_reading());
    }

    #[test]
    fn preserves_an_extended_row_without_a_battery_snapshot() {
        // Goal: source absence is SQL NULL, with the complete extension version still present.
        // Method: normalize one versioned row whose payload is wholly absent.
        let tables = battery_tables();
        tables.insert("ticks", "1 AS publication,1 AS battery_version", 1);

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let actual = read_battery(&normalized, "");

        let mut expected = vec![AnyValue::Null; 12];
        expected[0] = AnyValue::UInt8(1);
        assert_eq!(actual, expected);
    }

    macro_rules! malformed_extension {
        ($name:ident, $change:expr) => {
            #[test]
            fn $name() {
                // Goal: malformed optional data fails at admission before consumers bind queries.
                // Method: change one property of a complete typed battery table.
                let tables = battery_tables();
                let change: fn(&Tables) = $change;
                change(&tables);

                let result = table_source(&tables, Table::Ticks, TableSource::Staged);

                assert!(
                    matches!(result, Err(persistence::Error::Bound(_))),
                    "{:?}",
                    result.err()
                );
            }
        };
    }

    malformed_extension!(rejects_partial_empty_battery_metadata, |tables| {
        tables.change("ticks", |ticks| {
            ticks.drop_in_place("battery_current_ma").unwrap();
        });
    });
    malformed_extension!(
        rejects_wrong_types_even_in_empty_battery_metadata,
        |tables| {
            tables.change("ticks", |ticks| {
                retype(ticks, "battery_current_ma", &DataType::UInt64);
            });
        }
    );
    malformed_extension!(
        rejects_wrong_spelling_even_in_empty_battery_metadata,
        |tables| {
            tables.change("ticks", |ticks| {
                ticks
                    .rename("battery_charging", "Battery_Charging".into())
                    .unwrap();
            });
        }
    );
    malformed_extension!(rejects_an_unknown_battery_version, |tables| {
        tables.insert("ticks", "1 AS publication,2 AS battery_version", 1);
    });
    malformed_extension!(rejects_a_missing_battery_version, |tables| {
        tables.insert("ticks", "1 AS publication", 1);
    });
    malformed_extension!(rejects_battery_payload_without_its_source_time, |tables| {
        tables.insert(
            "ticks",
            "1 AS publication,1 AS battery_version,0 AS battery_cycle_count",
            1,
        );
    });
    malformed_extension!(rejects_more_than_sixty_one_extended_ticks, |tables| {
        tables.insert("ticks", "i+1 AS publication,1 AS battery_version", 62);
    });
    malformed_extension!(rejects_false_payload_without_source_time, |tables| {
        tables.insert(
            "ticks",
            "1 AS publication,1 AS battery_version,false AS battery_charging",
            1,
        );
    });
    malformed_extension!(rejects_signed_current_without_source_time, |tables| {
        tables.insert(
            "ticks",
            "1 AS publication,1 AS battery_version,-1 AS battery_current_ma",
            1,
        );
    });
    malformed_extension!(
        rejects_a_battery_row_without_its_tick_publication,
        |tables| {
            tables.insert(
                "ticks",
                "1 AS battery_version,0 AS battery_source_continuous_ns",
                1,
            );
        }
    );
    malformed_extension!(rejects_unknown_battery_column_names, |tables| {
        tables.add_column("ticks", "battery_charge_typo", "UBIGINT");
    });
    malformed_extension!(
        rejects_extended_tick_tables_exceeding_the_column_bound,
        |tables| {
            tables.add_column("ticks", "extra_one", "UBIGINT");
            tables.add_column("ticks", "extra_two", "UBIGINT");
        }
    );

    #[test]
    fn preserves_a_source_timestamp_without_any_independent_payload_field() {
        // Goal: each field can remain unavailable while a real source snapshot still exists.
        // Method: normalize one timestamped reading with every independent field unavailable.
        let tables = battery_tables();
        tables.insert(
            "ticks",
            "1 AS publication,1 AS battery_version,0 AS battery_source_continuous_ns",
            1,
        );

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let actual = read_battery(&normalized, "");

        let mut expected = vec![AnyValue::Null; 12];
        expected[0] = AnyValue::UInt8(1);
        expected[1] = AnyValue::UInt64(0);
        assert_eq!(actual, expected);
    }

    #[test]
    fn refuses_legacy_projection_that_would_exceed_the_column_bound() {
        // Goal: typed normalization fits the same bounded column set as captured extensions.
        // Method: add two unrelated columns to an original table before typed projection.
        let tables = tables();
        tables.add_column("ticks", "extra_one", "UBIGINT");
        tables.add_column("ticks", "extra_two", "UBIGINT");

        let result = table_source(&tables, Table::Ticks, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn preserves_stale_and_future_source_times_without_erasing_other_history() {
        // Goal: history retains raw timestamps so consumers can apply freshness per field.
        // Method: normalize a 61-row window, including an old source and a future source.
        let tables = battery_tables();
        tables.insert(
            "ticks",
            "i+1 AS publication,70000000000 AS continuous_ns,1 AS battery_version, \
            i*70000000000 AS battery_source_continuous_ns,i AS battery_cycle_count",
            61,
        );

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let frame = normalized.query(
            "SELECT count(*),min(battery_source_continuous_ns), \
            max(battery_source_continuous_ns),sum(battery_cycle_count) FROM normalized",
        );

        assert_eq!(
            row(&frame),
            [
                AnyValue::Int64(61),
                AnyValue::UInt64(0),
                AnyValue::UInt64(4_200_000_000_000),
                AnyValue::UInt64(1830),
            ]
        );
    }

    #[test]
    fn rejects_a_malformed_empty_saved_battery_table() {
        // Goal: saved metadata is validated even when no payload rows can reveal its defects.
        // Method: write an empty Parquet extension with a wrong boolean storage type.
        let directory = Directory::new();
        let tables = battery_tables();
        tables.change("ticks", |ticks| {
            retype(ticks, "battery_charging", &DataType::UInt8);
        });
        let path = directory.0.join("ticks.parquet");
        saved::write(&mut tables.frame("ticks"), &path);

        let result = table_source(&tables, Table::Ticks, TableSource::Parquet(&path));

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn accepts_complete_empty_battery_metadata() {
        // Goal: empty valid typed files remain readable.
        // Method: validate a complete zero-row tick extension.
        let tables = battery_tables();

        let result = table_source(&tables, Table::Ticks, TableSource::Staged);

        assert!(result.is_ok(), "{:?}", result.err());
    }

    fn battery_tables() -> Tables {
        let tables = tables();
        for (field, kind) in BATTERY_FIELDS {
            tables.add_column("ticks", field, kind);
        }
        tables
    }

    fn read_battery(normalized: &Normalized, filter: &str) -> Vec<AnyValue<'static>> {
        let fields = BATTERY_FIELDS.map(|(field, _)| field).join(",");
        let frame = normalized.query(&format!(
            "SELECT {fields} FROM normalized {filter} ORDER BY publication LIMIT 1"
        ));

        row(&frame)
    }

    fn row(frame: &DataFrame) -> Vec<AnyValue<'static>> {
        assert_eq!(frame.height(), 1);

        frame
            .columns()
            .iter()
            .map(|column| column.get(0).unwrap().into_static())
            .collect()
    }

    /// Casts one column of a scratch table exactly, as `ALTER COLUMN ... TYPE` does.
    fn retype(table: &mut DataFrame, field: &str, data_type: &DataType) {
        let column = table.column(field).unwrap().strict_cast(data_type).unwrap();
        table.with_column(column).unwrap();
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
        let tables = Tables::new();
        tables.create("ticks", &persistence::table_columns(Table::Ticks, &window));
        super::append_support::append(&tables, Table::Ticks, &window);

        let saved_tables = Encoder.encode(&window, &directory.0).unwrap();
        let staged = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let path = directory.0.join("ticks.parquet");
        let saved = table_source(&tables, Table::Ticks, TableSource::Parquet(&path)).unwrap();

        assert_eq!(saved_tables.len(), 10);
        assert_eq!(read_battery(&staged, ""), expected_reading());
        assert_eq!(read_battery(&saved, ""), expected_reading());
        let mut unavailable = vec![AnyValue::Null; 12];
        unavailable[0] = AnyValue::UInt8(1);
        assert_eq!(read_battery(&saved, "WHERE publication=2"), unavailable);
        let absent = saved.query(
            "SELECT count(*) FROM normalized WHERE publication=2 \
            AND battery_version=1 AND battery_source_continuous_ns IS NULL \
            AND battery_cycle_count IS NULL AND battery_charging IS NULL \
            AND gpu_energy_nj IS NULL AND power_watts IS NULL AND soc_celsius IS NULL",
        );
        assert_eq!(row(&absent), [AnyValue::Int64(1)]);
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
        let path = directory.0.join("ticks.parquet");
        let normalized =
            table_source(&Tables::new(), Table::Ticks, TableSource::Parquet(&path)).unwrap();
        let expected = vec![AnyValue::Null; 12];
        assert_eq!(read_battery(&normalized, ""), expected);
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

    fn expected_reading() -> Vec<AnyValue<'static>> {
        vec![
            AnyValue::UInt8(1),
            AnyValue::UInt64(0),
            AnyValue::UInt64(0),
            AnyValue::UInt64(0),
            AnyValue::UInt64(9_007_199_254_740_993),
            AnyValue::UInt64(9_007_199_254_740_991),
            AnyValue::UInt64(u64::MAX),
            AnyValue::Boolean(false),
            AnyValue::Boolean(true),
            AnyValue::Null,
            AnyValue::Int64(i64::MIN),
            AnyValue::UInt64(0),
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
