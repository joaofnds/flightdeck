#[cfg(test)]
mod query_support;
#[cfg(test)]
#[path = "query_support/saved.rs"]
mod saved;

#[cfg(test)]
mod tests {
    use super::query_support::{Tables, table_source};
    use super::saved;
    use persistence::engine::Row;
    use persistence::query::TableSource;
    use persistence::schema::Table;
    use polars::prelude::{Column, DataFrame, DataType, IntoColumn, NamedFrom, Series};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    const LEGACY_PROCESS: &str =
        "9,42,100,9007199254740993,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18";
    const IDENTITY_ROW: &str = "9 AS publication,42 AS pid,1 AS identity_version, \
        0 AS identity_availability,1 AS usage_availability,123 AS process_unique_id, \
        4 AS pid_version";

    #[test]
    fn preserves_legacy_usage_and_marks_identity_unsupported() {
        // Goal: old recordings remain readable without invented executable or App metadata.
        // Method: project a legacy process row through the same boundary as saved/live readers.
        let tables = tables(Table::Processes);
        insert_values(&tables, "processes", LEGACY_PROCESS);

        let normalized = table_source(&tables, Table::Processes, TableSource::Staged).unwrap();
        let frame = normalized.query(
            "SELECT user_ns, identity_version, identity_availability, usage_availability, \
            process_unique_id, pid_version, executable_name, app_path FROM normalized",
        );
        let row = single(&frame);
        let result = (
            row.get::<u64>(0).unwrap(),
            row.get::<Option<u8>>(1).unwrap(),
            row.get::<u8>(2).unwrap(),
            row.get::<u8>(3).unwrap(),
            row.get::<Option<u64>>(4).unwrap(),
            row.get::<Option<i32>>(5).unwrap(),
            row.get::<Option<Vec<u8>>>(6).unwrap(),
            row.get::<Option<Vec<u8>>>(7).unwrap(),
        );

        assert_eq!(
            result,
            (9_007_199_254_740_993, None, 3, 0, None, None, None, None)
        );
    }

    #[test]
    fn rejects_an_unknown_captured_identity_version() {
        // Goal: a reader must not guess the meaning of a newer identity encoding.
        // Method: append the complete typed extension with an unsupported version.
        let tables = identity_tables("BLOB");
        tables.insert(
            "processes",
            "9 AS publication,42 AS pid,9 AS identity_version,0 AS identity_availability, \
            1 AS usage_availability,123 AS process_unique_id,4 AS pid_version, \
            'worker'::BLOB AS executable_name",
            1,
        );

        let result = table_source(&tables, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn rejects_identity_columns_with_wrong_storage_types() {
        // Goal: raw path bytes cannot silently become Unicode strings at the read boundary.
        // Method: give a complete empty extension a text executable column instead of BLOB.
        let tables = identity_tables("VARCHAR");

        let result = table_source(&tables, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn rejects_measured_identity_without_its_process_creation() {
        // Goal: an executable must not be assigned to an unverified process creation.
        // Method: supply readable bytes but omit the kernel unique process identifier.
        let tables = identity_tables("BLOB");
        tables.insert(
            "processes",
            "9 AS publication,42 AS pid,1 AS identity_version,0 AS identity_availability, \
            1 AS usage_availability,4 AS pid_version,'worker'::BLOB AS executable_name",
            1,
        );

        let result = table_source(&tables, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn preserves_refused_usage_with_readable_non_utf8_identity() {
        // Goal: unavailable usage does not erase independently captured byte identity.
        // Method: read a verified identity-only row and compare bytes and NULL numeric gauges.
        let tables = identity_tables("BLOB");
        let mut row = tables.rows(IDENTITY_ROW, 1);
        set_bytes(&mut row, "executable_name", Some(b"worker\xff"));
        set_bytes(&mut row, "app_path", Some(b"/Applications/Engine.app"));
        tables.append("processes", &row);

        let normalized = table_source(&tables, Table::Processes, TableSource::Staged).unwrap();
        let frame = normalized.query(
            "SELECT executable_name,app_path,process_unique_id,pid_version, \
            user_ns,resident_bytes,usage_availability FROM normalized",
        );
        let row = single(&frame);
        let result = (
            row.get::<Vec<u8>>(0).unwrap(),
            row.get::<Vec<u8>>(1).unwrap(),
            row.get::<u64>(2).unwrap(),
            row.get::<i32>(3).unwrap(),
            row.get::<Option<u64>>(4).unwrap(),
            row.get::<Option<u64>>(5).unwrap(),
            row.get::<u8>(6).unwrap(),
        );

        assert_eq!(
            result,
            (
                b"worker\xff".to_vec(),
                b"/Applications/Engine.app".to_vec(),
                123,
                4,
                None,
                None,
                1
            )
        );
    }

    #[test]
    fn rejects_measured_identity_without_executable_bytes() {
        // Goal: a measured attribution must identify the executable it claims to have read.
        // Method: retain valid creation fields while omitting the executable BLOB.
        let tables = identity_tables("BLOB");
        tables.insert("processes", IDENTITY_ROW, 1);

        let result = table_source(&tables, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn rejects_measured_usage_with_a_null_numeric_payload() {
        // Goal: identity-only presence must not be mistaken for measured CPU/memory/energy.
        // Method: declare measured usage while every original usage/task field is NULL.
        let tables = identity_tables("BLOB");
        tables.insert(
            "processes",
            "9 AS publication,42 AS pid,1 AS identity_version,0 AS identity_availability, \
            0 AS usage_availability,123 AS process_unique_id,4 AS pid_version, \
            'worker'::BLOB AS executable_name",
            1,
        );

        let result = table_source(&tables, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn preserves_measured_usage_with_unavailable_identity() {
        // Goal: a failed path read cannot erase a measured process's original counters.
        // Method: normalize a complete numeric row with explicitly refused identity.
        let tables = identity_tables("BLOB");
        insert_values(
            &tables,
            "processes",
            &format!("{LEGACY_PROCESS},1,1,0,NULL,NULL,NULL,NULL"),
        );

        let normalized = table_source(&tables, Table::Processes, TableSource::Staged).unwrap();
        let frame = normalized
            .query("SELECT user_ns,identity_availability,executable_name FROM normalized");
        let row = single(&frame);
        let result = (
            row.get::<u64>(0).unwrap(),
            row.get::<u8>(1).unwrap(),
            row.get::<Option<Vec<u8>>>(2).unwrap(),
        );

        assert_eq!(result, (9_007_199_254_740_993, 1, None));
    }

    #[test]
    fn preserves_legacy_event_availability() {
        // Goal: start/exec events without usage stay unavailable through normalization.
        // Method: normalize a legacy start event and query its independent availability.
        let tables = tables(Table::Events);
        tables.insert(
            "events",
            "9 AS publication,7 AS event,8 AS run_tick,42 AS pid,1 AS kind,3 AS availability",
            1,
        );

        let normalized = table_source(&tables, Table::Events, TableSource::Staged).unwrap();
        let frame = normalized.query(
            "SELECT identity_version,identity_availability,usage_availability FROM normalized",
        );
        let row = single(&frame);
        let result = (
            row.get::<Option<u8>>(0).unwrap(),
            row.get::<u8>(1).unwrap(),
            row.get::<u8>(2).unwrap(),
        );

        assert_eq!(result, (None, 3, 3));
    }

    macro_rules! invalid_process {
        ($name:ident, $change:expr) => {
            #[test]
            fn $name() {
                // Goal: malformed identity/usage combinations fail before consumer queries bind.
                // Method: change one field in a readable identity with refused numeric usage.
                let tables = identity_tables("BLOB");
                let mut row = tables.rows(IDENTITY_ROW, 1);
                set_bytes(&mut row, "executable_name", Some(b"worker"));
                let change: fn(&Tables, &mut DataFrame) = $change;
                change(&tables, &mut row);
                tables.append("processes", &row);

                let result = table_source(&tables, Table::Processes, TableSource::Staged);

                assert!(
                    matches!(result, Err(persistence::Error::Bound(_))),
                    "{:?}",
                    result.err()
                );
            }
        };
    }

    invalid_process!(rejects_a_partial_identity_extension, |tables, _| {
        tables.change("processes", |processes| {
            processes.drop_in_place("app_path").unwrap();
        });
    });
    invalid_process!(rejects_a_null_identity_version, |_, row| {
        set_null(row, "identity_version");
    });
    invalid_process!(rejects_unknown_identity_availability, |_, row| {
        set_number(row, "identity_availability", 7);
    });
    invalid_process!(rejects_unknown_usage_availability, |_, row| {
        set_number(row, "usage_availability", 3);
    });
    invalid_process!(rejects_refused_usage_with_a_numeric_payload, |_, row| {
        set_number(row, "user_ns", 0);
    });
    invalid_process!(rejects_unavailable_identity_with_a_program, |_, row| {
        set_number(row, "identity_availability", 1);
    });
    invalid_process!(rejects_a_creation_without_its_pid_version, |_, row| {
        set_null(row, "pid_version");
    });
    invalid_process!(
        rejects_measured_identity_without_either_creation_field,
        |_, row| {
            set_null(row, "process_unique_id");
            set_null(row, "pid_version");
        }
    );
    invalid_process!(rejects_an_empty_executable, |_, row| {
        set_bytes(row, "executable_name", Some(b""));
    });
    invalid_process!(rejects_an_executable_containing_a_separator, |_, row| {
        set_bytes(row, "executable_name", Some(b"bin/worker"));
    });
    invalid_process!(rejects_an_executable_containing_a_nul, |_, row| {
        set_bytes(row, "executable_name", Some(b"worker\x00"));
    });
    invalid_process!(rejects_an_oversized_executable, |_, row| {
        set_bytes(row, "executable_name", Some(&[b'a'; 4096]));
    });
    invalid_process!(rejects_a_relative_app_path, |_, row| {
        set_bytes(row, "app_path", Some(b"Engine.app"));
    });
    invalid_process!(rejects_a_non_bundle_app_path, |_, row| {
        set_bytes(row, "app_path", Some(b"/Applications/Engine"));
    });
    invalid_process!(rejects_an_inner_bundle_as_the_app, |_, row| {
        set_bytes(row, "app_path", Some(b"/Engine.app/Helpers/Worker.app"));
    });
    invalid_process!(rejects_an_app_path_containing_a_nul, |_, row| {
        set_bytes(row, "app_path", Some(b"/Eng\x00ine.app"));
    });
    invalid_process!(rejects_an_oversized_app_path, |_, row| {
        let mut app = vec![b'a'; 4096];
        app[0] = b'/';
        app[4092..].copy_from_slice(b".app");
        set_bytes(row, "app_path", Some(&app));
    });
    invalid_process!(rejects_identity_without_a_publication, |_, row| {
        set_null(row, "publication");
    });
    invalid_process!(rejects_identity_without_a_pid, |_, row| {
        set_null(row, "pid");
    });

    #[test]
    fn reopens_legacy_and_augmented_parquet_without_changing_either_file() {
        // Goal: mixed histories retain exact original counters and captured non-Unicode bytes.
        // Method: query two real Parquet files through the public saved-table boundary.
        let files = ParquetFiles::new();
        let legacy = tables(Table::Processes);
        insert_values(
            &legacy,
            "processes",
            "9,41,100,9007199254740993,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18",
        );
        files.write(&legacy, "legacy");
        let augmented = identity_tables("BLOB");
        let mut row = augmented.rows(
            "10 AS publication,42 AS pid,100 AS awake_ns_at_start, \
            9007199254740995 AS user_ns,1 AS identity_version,0 AS identity_availability, \
            0 AS usage_availability,123 AS process_unique_id,-4 AS pid_version",
            1,
        );
        let others = Table::Processes.columns().skip(4).zip(2..);
        for (field, value) in others {
            set_number(&mut row, field, value);
        }
        set_bytes(&mut row, "executable_name", Some(b"worker\xff"));
        set_bytes(&mut row, "app_path", Some(b"/Applications/Engine.app"));
        augmented.append("processes", &row);
        files.write(&augmented, "augmented");
        let before = (files.read("legacy"), files.read("augmented"));
        let reader = Tables::new();

        let old = table_source(
            &reader,
            Table::Processes,
            TableSource::Parquet(&files.path("legacy")),
        )
        .unwrap();
        let new = table_source(
            &reader,
            Table::Processes,
            TableSource::Parquet(&files.path("augmented")),
        )
        .unwrap();
        let mut result = Vec::new();
        for normalized in [old, new] {
            let frame = normalized
                .query("SELECT user_ns,executable_name,identity_availability FROM normalized");
            for index in 0..frame.height() {
                let row = Row::new(&frame, index).unwrap();
                result.push((
                    row.get::<u64>(0).unwrap(),
                    row.get::<Option<Vec<u8>>>(1).unwrap(),
                    row.get::<u8>(2).unwrap(),
                ));
            }
        }
        result.sort_by_key(|row| row.0);

        assert_eq!(
            result,
            vec![
                (9_007_199_254_740_993, None, 3),
                (9_007_199_254_740_995, Some(b"worker\xff".to_vec()), 0)
            ]
        );
        assert_eq!((files.read("legacy"), files.read("augmented")), before);
    }

    #[test]
    fn preserves_captured_identity_on_an_event_without_usage() {
        // Goal: a start event can identify an App without inventing resource counters.
        // Method: normalize an extended start event whose original usage payload is NULL.
        let tables = identity_table(Table::Events, "BLOB");
        tables.insert(
            "events",
            "9 AS publication,7 AS event,8 AS run_tick,42 AS pid,1 AS kind, \
            3 AS availability,1 AS identity_version,0 AS identity_availability, \
            3 AS usage_availability,123 AS process_unique_id,4 AS pid_version, \
            'worker'::BLOB AS executable_name",
            1,
        );

        let normalized = table_source(&tables, Table::Events, TableSource::Staged).unwrap();
        let frame =
            normalized.query("SELECT executable_name,usage_availability,user_ns FROM normalized");
        let row = single(&frame);
        let result = (
            row.get::<Vec<u8>>(0).unwrap(),
            row.get::<u8>(1).unwrap(),
            row.get::<Option<u64>>(2).unwrap(),
        );

        assert_eq!(result, (b"worker".to_vec(), 3, None));
    }

    #[test]
    fn refuses_an_event_whose_two_usage_discriminators_disagree() {
        // Goal: old and identity-aware readers cannot disagree about whether usage was read.
        // Method: disagree between refused/gone codes with an otherwise valid NULL payload.
        let tables = identity_table(Table::Events, "BLOB");
        tables.insert(
            "events",
            "9 AS publication,42 AS pid,1 AS availability,1 AS identity_version, \
            0 AS identity_availability,2 AS usage_availability,123 AS process_unique_id, \
            4 AS pid_version,'worker'::BLOB AS executable_name",
            1,
        );

        let result = table_source(&tables, Table::Events, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    fn matches_retained_program_contract(bytes: &collector::ring::ProgramBytes, valid: bool) {
        let tables = identity_tables("BLOB");
        let mut row = tables.rows(IDENTITY_ROW, 1);
        set_bytes(&mut row, "executable_name", Some(bytes.executable_name));
        set_bytes(&mut row, "app_path", bytes.app_path);
        tables.append("processes", &row);

        let shared = bytes.validate();
        let saved = table_source(&tables, Table::Processes, TableSource::Staged);

        assert_eq!(valid, shared.is_ok());
        assert_eq!(valid, saved.is_ok());
    }

    macro_rules! retained_program_contract {
        ($($name:ident, $executable:expr, $app:expr, $valid:literal;)*) => {$(
            #[test]
            fn $name() {
                // Goal: live and saved readers agree on retained program bytes.
                // Method: validate one captured byte shape through the common owner and Polars.
                matches_retained_program_contract(&collector::ring::ProgramBytes {
                    executable_name: $executable, app_path: $app,
                }, $valid);
            }
        )*};
    }

    retained_program_contract! {
        preserves_repeated_app_suffix_in_one_component,
            b"helper", Some(b"/Applications/A.app.app"), true;
        rejects_nested_app_components_at_both_boundaries,
            b"helper", Some(b"/A.app/Helper.app"), false;
        preserves_non_utf8_program_bytes_at_both_boundaries,
            b"helper\xff", Some(b"/A\xff.app"), true;
        rejects_relative_app_bytes_at_both_boundaries,
            b"helper", Some(b"A.app"), false;
    }

    #[test]
    fn preserves_paths_at_the_capture_byte_limit() {
        // Goal: the reader accepts every path byte the bounded capture scratch can retain.
        // Method: normalize maximum-length executable and outer App bytes with signed version.
        let tables = identity_tables("BLOB");
        let executable = vec![0xff_u8; 4095];
        let mut app = vec![b'a'; 4095];
        app[0] = b'/';
        app[4091..].copy_from_slice(b".app");
        let mut row = tables.rows(IDENTITY_ROW, 1);
        set_number(&mut row, "pid_version", i64::from(i32::MIN));
        set_bytes(&mut row, "executable_name", Some(&executable));
        set_bytes(&mut row, "app_path", Some(&app));
        tables.append("processes", &row);

        let normalized = table_source(&tables, Table::Processes, TableSource::Staged).unwrap();
        let frame = normalized.query("SELECT executable_name,app_path,pid_version FROM normalized");
        let row = single(&frame);
        let result = (
            row.get::<Vec<u8>>(0).unwrap(),
            row.get::<Vec<u8>>(1).unwrap(),
            row.get::<i32>(2).unwrap(),
        );

        assert_eq!(result, (executable, app, i32::MIN));
    }

    #[test]
    fn refuses_a_table_exceeding_the_reader_row_bound() {
        // Goal: identity validation cannot scan an unbounded extended table.
        // Method: create one more row than the public reader's permitted million-row result.
        let tables = identity_tables("BLOB");
        tables.insert(
            "processes",
            &format!("{IDENTITY_ROW},'worker'::BLOB AS executable_name"),
            1_048_577,
        );

        let result = table_source(&tables, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn refuses_legacy_normalization_exceeding_the_column_bound() {
        // Goal: typed legacy metadata cannot expand a result past the viewer's 32 columns.
        // Method: add five otherwise unrelated columns to the 21-column legacy table.
        let tables = tables(Table::Processes);
        for field in ["extra1", "extra2", "extra3", "extra4", "extra5"] {
            tables.add_column("processes", field, "UBIGINT");
        }

        let result = table_source(&tables, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn refuses_a_case_variant_partial_identity_extension() {
        // Goal: case-insensitive SQL binding cannot hide identity metadata from validation.
        // Method: add only an uppercase version column that conflicts with the legacy alias.
        let tables = tables(Table::Processes);
        tables.add_column("processes", "IDENTITY_VERSION", "UTINYINT");
        tables.insert(
            "processes",
            "9 AS publication,42 AS pid,9 AS \"IDENTITY_VERSION\"",
            1,
        );

        let result = table_source(&tables, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn refuses_a_saved_case_variant_partial_identity_extension() {
        // Goal: saved input cannot disguise a partial extension through identifier casing.
        // Method: write mixed-case metadata to Parquet and open the public saved boundary.
        let files = ParquetFiles::new();
        let source = tables(Table::Processes);
        source.add_column("processes", "Identity_Version", "UTINYINT");
        source.insert(
            "processes",
            "9 AS publication,42 AS pid,9 AS \"Identity_Version\"",
            1,
        );
        files.write(&source, "case-variant");
        let reader = Tables::new();

        let result = table_source(
            &reader,
            Table::Processes,
            TableSource::Parquet(&files.path("case-variant")),
        );

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    #[test]
    fn refuses_noncanonical_spelling_in_a_complete_identity_extension() {
        // Goal: every supported source exposes the canonical field names consumers read.
        // Method: change only spelling in an otherwise supported typed extension.
        let tables = identity_tables("BLOB");
        tables.change("processes", |processes| {
            processes.rename("app_path", "APP_PATH".into()).unwrap();
        });

        let result = table_source(&tables, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{:?}",
            result.err()
        );
    }

    fn identity_tables(executable_type: &str) -> Tables {
        identity_table(Table::Processes, executable_type)
    }

    fn identity_table(table: Table, executable_type: &str) -> Tables {
        let tables = tables(table);
        let name = table.name();
        for (field, kind) in [
            ("identity_version", "UTINYINT"),
            ("identity_availability", "UTINYINT"),
            ("usage_availability", "UTINYINT"),
            ("process_unique_id", "UBIGINT"),
            ("pid_version", "INTEGER"),
            ("executable_name", executable_type),
            ("app_path", "BLOB"),
        ] {
            tables.add_column(name, field, kind);
        }
        tables
    }

    /// Inserts one row whose comma-separated values follow the table's column order.
    fn insert_values(tables: &Tables, name: &str, values: &str) {
        let frame = tables.frame(name);
        let values: Vec<_> = values.split(',').map(str::trim).collect();
        assert_eq!(values.len(), frame.width());

        let selection = frame
            .columns()
            .iter()
            .zip(values)
            .map(|(column, value)| format!("{value} AS {}", column.name()))
            .collect::<Vec<_>>()
            .join(",");
        tables.insert(name, &selection, 1);
    }

    fn set_bytes(row: &mut DataFrame, field: &str, value: Option<&[u8]>) {
        let column = Series::new(field.into(), [value]).into_column();
        row.with_column(column).unwrap();
    }

    fn set_number(row: &mut DataFrame, field: &str, value: i64) {
        let column = Series::new(field.into(), [value]).into_column();
        row.with_column(column).unwrap();
    }

    fn set_null(row: &mut DataFrame, field: &str) {
        let column = Column::full_null(field.into(), row.height(), &DataType::Null);
        row.with_column(column).unwrap();
    }

    fn single(frame: &DataFrame) -> Row<'_> {
        assert_eq!(frame.height(), 1);

        Row::new(frame, 0).unwrap()
    }

    struct ParquetFiles(PathBuf);
    static NEXT: AtomicU64 = AtomicU64::new(0);

    impl ParquetFiles {
        fn new() -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "flightdeck-identity-{}-{serial}",
                std::process::id()
            ));
            std::fs::create_dir(&directory).unwrap();
            Self(directory)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(format!("{name}.parquet"))
        }

        fn read(&self, name: &str) -> Vec<u8> {
            std::fs::read(self.path(name)).unwrap()
        }

        fn write(&self, tables: &Tables, name: &str) {
            saved::write(&mut tables.frame("processes"), &self.path(name));
        }
    }

    impl Drop for ParquetFiles {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn tables(table: Table) -> Tables {
        let tables = Tables::new();
        tables.create(table.name(), &table.declaration());
        tables
    }
}
