#[cfg(test)]
#[path = "query_support/append.rs"]
mod append_support;
#[cfg(test)]
mod query_support;

#[cfg(test)]
mod tests {
    use super::query_support::normalized::Normalized;
    use super::query_support::{Tables, table_source};
    use collector::disks::{DISKS_MAX, DiskReading, DiskSnapshot, ScanState};
    use persistence::engine::Row;
    use persistence::query::TableSource;
    use persistence::schema::{BATTERY_FIELDS, TABLES, Table};

    #[test]
    fn preserves_maximum_disk_snapshot_and_legacy_typed_absence() {
        // Goal: maximum disk data fits the existing tick and text bounds without losing u64 bits.
        // Method: encode sixteen devices, normalize real SQL sources and decode the admitted cell.
        let snapshot = maximum_snapshot();
        let encoded = persistence::disks::encode(&snapshot).unwrap();
        let tables = tables();
        tables.add_column("ticks", "physical_disks", "VARCHAR");
        tables.insert(
            "ticks",
            &format!("1 AS publication,'{encoded}' AS physical_disks"),
            1,
        );

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let actual = disk_rows(&normalized)[0].0.clone().unwrap();
        let mut decoded = DiskSnapshot::default();
        persistence::disks::decode(&actual, &mut decoded).unwrap();

        assert_eq!(encoded.len(), 3904);
        assert_eq!(decoded, snapshot);
        assert_eq!(normalized.query("SELECT * FROM normalized").width(), 53);
        let legacy = table_source(&self::tables(), Table::Ticks, TableSource::Staged).unwrap();
        assert_eq!(legacy.query("SELECT * FROM normalized").width(), 53);
    }

    fn tables() -> Tables {
        let tables = Tables::new();
        tables.create("ticks", TABLES[0].1);
        tables
    }

    fn maximum_snapshot() -> DiskSnapshot {
        let mut snapshot = DiskSnapshot {
            source_continuous_ns: u64::MAX,
            state: ScanState::Complete,
            count: DISKS_MAX,
            ..DiskSnapshot::default()
        };
        for (index, record) in snapshot.records.iter_mut().enumerate() {
            let identity = u64::try_from(index).unwrap() + 1;
            *record = DiskReading {
                driver_id: identity,
                media_id: u64::MAX - identity,
                read_bytes: Some(u64::MAX),
                write_bytes: Some(0),
                read_operations: None,
                write_operations: Some(u64::MAX),
                read_errors: Some(0),
                write_errors: None,
                read_time_ns: Some(u64::MAX),
                write_time_ns: Some(1),
                ..DiskReading::default()
            };
            record.name[..31].fill(b'x');
        }
        snapshot
    }

    #[test]
    fn preserves_battery_columns_when_disk_extension_is_absent() {
        // Goal: disk normalization preserves every preexisting Battery field within the tick bound.
        // Method: normalize a complete Battery tick and inspect its exact high unsigned count.
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
        let actual = disk_rows(&normalized);

        assert_eq!(actual, [(None, Some(u64::MAX))]);
    }

    macro_rules! malformed_cell {
        ($name:ident, $change:expr) => {
            #[test]
            fn $name() {
                // Goal: malformed extensions fail admission before queries can consume them.
                // Method: corrupt one property of an otherwise valid maximum disk cell.
                let mut encoded = persistence::disks::encode(&maximum_snapshot()).unwrap();
                ($change)(&mut encoded);
                let tables = tables();
                tables.add_column("ticks", "physical_disks", "VARCHAR");
                tables.insert(
                    "ticks",
                    &format!("1 AS publication,'{encoded}' AS physical_disks"),
                    1,
                );

                let result = table_source(&tables, Table::Ticks, TableSource::Staged);

                assert!(
                    matches!(result, Err(persistence::Error::Bound(_))),
                    "{:?}",
                    result.err()
                );
            }
        };
    }

    fn word(text: &mut String, index: usize, value: u64) {
        let mut encoded = String::new();
        for byte in value.to_le_bytes() {
            use std::fmt::Write;
            write!(&mut encoded, "{byte:02x}").unwrap();
        }
        text.replace_range(index * 16..(index + 1) * 16, &encoded);
    }

    malformed_cell!(rejects_unknown_disk_codec_versions, |text: &mut String| {
        word(text, 0, 0);
    });
    malformed_cell!(rejects_overflowing_disk_counts, |text: &mut String| word(
        text, 3, 17
    ));
    malformed_cell!(rejects_unknown_disk_scan_states, |text: &mut String| word(
        text, 2, 4
    ));
    malformed_cell!(rejects_noncanonical_disk_flags, |text: &mut String| word(
        text,
        6,
        1 << 10
    ));
    malformed_cell!(rejects_duplicate_disk_identities, |text: &mut String| {
        word(text, 19, 1);
        word(text, 20, u64::MAX - 1);
    });
    malformed_cell!(rejects_nonzero_disk_name_padding, |text: &mut String| word(
        text,
        7,
        u64::from_le_bytes([b'a', 0, b'x', 0, 0, 0, 0, 0])
    ));
    malformed_cell!(
        rejects_disk_text_with_trailing_words,
        |text: &mut String| text.push_str("0000000000000000")
    );
    malformed_cell!(rejects_disk_text_with_short_words, |text: &mut String| text
        .truncate(text.len() - 1));
    malformed_cell!(rejects_disk_text_with_uppercase_hex, |text: &mut String| {
        *text = text.to_uppercase();
    });
    malformed_cell!(
        rejects_disk_text_with_nonhex_characters,
        |text: &mut String| text.replace_range(..1, "g")
    );

    macro_rules! malformed_metadata {
        ($name:ident, $field:literal, $kind:literal) => {
            #[test]
            fn $name() {
                // Goal: disk metadata is checked even when an empty source has no data to decode.
                // Method: expose the optional column with one invalid metadata property.
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

    malformed_metadata!(
        rejects_wrong_disk_column_spelling,
        "Physical_Disks",
        "VARCHAR"
    );
    malformed_metadata!(rejects_wrong_disk_column_type, "physical_disks", "BLOB");

    #[test]
    fn preserves_unavailable_rows_in_an_extended_disk_window() {
        // Goal: a gap stays SQL NULL without being converted into a zero-device scan.
        // Method: admit an extension with one unavailable tick and inspect its normalized row.
        let tables = tables();
        tables.add_column("ticks", "physical_disks", "VARCHAR");
        tables.insert("ticks", "1 AS publication", 1);

        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let actual = disk_rows(&normalized);

        assert_eq!(actual, [(None, None)]);
    }

    #[test]
    fn stages_and_saves_owned_disk_payload_independently_of_battery() {
        // Goal: staging and Parquet preserve maximum disk words, Battery values and missing rows.
        // Method: encode a mixed captured window and compare staged SQL with the saved tick source.
        let directory = Directory::new();
        let mut window = window();
        let mut tick = persistence::tick_buffer(&shape()).unwrap();
        tick.disks = Some(maximum_snapshot());
        tick.battery = Some(collector::battery::BatteryReading {
            source_continuous_ns: 1,
            cycle_count: Some(u64::MAX),
            ..collector::battery::BatteryReading::default()
        });
        window.push_tick(&observation(1), &tick, &[]).unwrap();
        tick.disks = None;
        tick.battery = None;
        window.push_tick(&observation(2), &tick, &[]).unwrap();
        let tables = Tables::new();
        tables.create("ticks", &persistence::table_columns(Table::Ticks, &window));
        super::append_support::append(&tables, Table::Ticks, &window);

        let saved_tables = persistence::Encoder.encode(&window, &directory.0).unwrap();
        let staged = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();
        let saved = table_source(
            &tables,
            Table::Ticks,
            TableSource::Parquet(&directory.0.join("ticks.parquet")),
        )
        .unwrap();
        let staged_rows = disk_rows(&staged);
        let saved_rows = disk_rows(&saved);

        assert_eq!(saved_tables.len(), 10);
        assert_eq!(staged_rows, saved_rows);
        let mut decoded = DiskSnapshot::default();
        persistence::disks::decode(staged_rows[0].0.as_ref().unwrap(), &mut decoded).unwrap();
        assert_eq!(decoded, maximum_snapshot());
        assert_eq!(staged_rows[0].1, Some(u64::MAX));
        assert_eq!(staged_rows[1], (None, None));
    }

    fn disk_rows(normalized: &Normalized) -> Vec<(Option<String>, Option<u64>)> {
        let frame = normalized.query(
            "SELECT physical_disks,battery_cycle_count FROM normalized ORDER BY publication",
        );

        (0..frame.height())
            .map(|index| {
                let row = Row::new(&frame, index).unwrap();
                (row.get(0).unwrap(), row.get(1).unwrap())
            })
            .collect()
    }

    #[test]
    fn writes_disks_without_requiring_battery_columns() {
        // Goal: optional extensions remain independent in raw files.
        // Method: stage a disk-only tick and inspect the encoder's original column definition.
        let mut window = window();
        let mut tick = persistence::tick_buffer(&shape()).unwrap();
        tick.disks = Some(maximum_snapshot());
        window.push_tick(&observation(1), &tick, &[]).unwrap();

        let columns = persistence::table_columns(Table::Ticks, &window);
        let tables = Tables::new();
        tables.create("ticks", &columns);
        super::append_support::append(&tables, Table::Ticks, &window);
        let normalized = table_source(&tables, Table::Ticks, TableSource::Staged).unwrap();

        assert_eq!(columns, format!("{}, physical_disks VARCHAR", TABLES[0].1));
        assert_eq!(disk_rows(&normalized)[0].1, None);
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
            let path = std::env::temp_dir()
                .join(format!("disk-persistence-{}-{number}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn clears_caller_owned_snapshot_after_invalid_hex() {
        // Goal: failed text decoding never leaves a previously valid snapshot available.
        // Method: reuse an occupied output buffer for a malformed hex digit.
        let mut decoded = maximum_snapshot();
        let encoded = "g".repeat(64);

        let result = persistence::disks::decode(&encoded, &mut decoded);

        assert!(matches!(result, Err(persistence::Error::Bound(_))));
        assert_eq!(decoded, DiskSnapshot::default());
    }

    #[test]
    fn clears_caller_owned_snapshot_after_partial_codec_decode() {
        // Goal: a late record error clears every record the shared decoder already filled.
        // Method: corrupt the final maximum-payload record after initializing a valid output.
        let mut decoded = maximum_snapshot();
        let mut encoded = persistence::disks::encode(&decoded).unwrap();
        word(&mut encoded, 4 + 15 * 15 + 2, 1 << 20);

        let result = persistence::disks::decode(&encoded, &mut decoded);

        assert!(matches!(result, Err(persistence::Error::Bound(_))));
        assert_eq!(decoded, DiskSnapshot::default());
    }
}
