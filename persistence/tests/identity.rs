#[cfg(test)]
mod tests {
    use duckdb::Connection;
    use persistence::query::{TableSource, table_source};
    use persistence::schema::{TABLES, Table};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn preserves_legacy_usage_and_marks_identity_unsupported() {
        // Goal: old recordings remain readable without invented executable or App metadata.
        // Method: project a legacy process row through the same boundary as saved/live readers.
        let connection = connection(Table::Processes);
        connection
            .execute_batch(
                "INSERT INTO processes VALUES \
            (9,42,100,9007199254740993,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18)",
            )
            .unwrap();

        let source = table_source(&connection, Table::Processes, TableSource::Staged).unwrap();
        let result = connection
            .query_row(
                &format!(
                    "SELECT user_ns, identity_version, identity_availability, usage_availability, \
                process_unique_id, pid_version, executable_name, app_path FROM {source}"
                ),
                [],
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, Option<u8>>(1)?,
                        row.get::<_, u8>(2)?,
                        row.get::<_, u8>(3)?,
                        row.get::<_, Option<u64>>(4)?,
                        row.get::<_, Option<i32>>(5)?,
                        row.get::<_, Option<Vec<u8>>>(6)?,
                        row.get::<_, Option<Vec<u8>>>(7)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(
            result,
            (9_007_199_254_740_993, None, 3, 0, None, None, None, None)
        );
    }

    #[test]
    fn rejects_an_unknown_captured_identity_version() {
        // Goal: a reader must not guess the meaning of a newer identity encoding.
        // Method: append the complete typed extension with an unsupported version.
        let connection = identity_connection("BLOB");
        connection
            .execute_batch(
                "INSERT INTO processes(publication,pid,identity_version,identity_availability, \
            usage_availability,process_unique_id,pid_version,executable_name) \
            VALUES(9,42,9,0,1,123,4,'worker'::BLOB)",
            )
            .unwrap();

        let result = table_source(&connection, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_identity_columns_with_wrong_storage_types() {
        // Goal: raw path bytes cannot silently become Unicode strings at the read boundary.
        // Method: give a complete empty extension a text executable column instead of BLOB.
        let connection = identity_connection("VARCHAR");

        let result = table_source(&connection, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_measured_identity_without_its_process_creation() {
        // Goal: an executable must not be assigned to an unverified process creation.
        // Method: supply readable bytes but omit the kernel unique process identifier.
        let connection = identity_connection("BLOB");
        connection
            .execute_batch(
                "INSERT INTO processes(publication,pid,identity_version,identity_availability, \
            usage_availability,pid_version,executable_name) \
            VALUES(9,42,1,0,1,4,'worker'::BLOB)",
            )
            .unwrap();

        let result = table_source(&connection, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn preserves_refused_usage_with_readable_non_utf8_identity() {
        // Goal: unavailable usage does not erase independently captured byte identity.
        // Method: read a verified identity-only row and compare bytes and NULL numeric gauges.
        let connection = identity_connection("BLOB");
        connection
            .execute_batch(
                r"INSERT INTO processes(publication,pid,identity_version,identity_availability,
            usage_availability,process_unique_id,pid_version,executable_name,app_path)
            VALUES(9,42,1,0,1,123,4,'worker\xFF'::BLOB,'/Applications/Engine.app'::BLOB)",
            )
            .unwrap();

        let source = table_source(&connection, Table::Processes, TableSource::Staged).unwrap();
        let result = connection
            .query_row(
                &format!(
                    "SELECT executable_name,app_path,process_unique_id,pid_version, \
                user_ns,resident_bytes,usage_availability FROM {source}"
                ),
                [],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, u64>(2)?,
                        row.get::<_, i32>(3)?,
                        row.get::<_, Option<u64>>(4)?,
                        row.get::<_, Option<u64>>(5)?,
                        row.get::<_, u8>(6)?,
                    ))
                },
            )
            .unwrap();

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
        let connection = identity_connection("BLOB");
        connection
            .execute_batch(
                "INSERT INTO processes(publication,pid,identity_version,identity_availability, \
            usage_availability,process_unique_id,pid_version) VALUES(9,42,1,0,1,123,4)",
            )
            .unwrap();

        let result = table_source(&connection, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_measured_usage_with_a_null_numeric_payload() {
        // Goal: identity-only presence must not be mistaken for measured CPU/memory/energy.
        // Method: declare measured usage while every original usage/task field is NULL.
        let connection = identity_connection("BLOB");
        connection
            .execute_batch(
                "INSERT INTO processes(publication,pid,identity_version,identity_availability, \
            usage_availability,process_unique_id,pid_version,executable_name) \
            VALUES(9,42,1,0,0,123,4,'worker'::BLOB)",
            )
            .unwrap();

        let result = table_source(&connection, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn preserves_measured_usage_with_unavailable_identity() {
        // Goal: a failed path read cannot erase a measured process's original counters.
        // Method: normalize a complete numeric row with explicitly refused identity.
        let connection = identity_connection("BLOB");
        connection
            .execute_batch(
                "INSERT INTO processes VALUES \
        (9,42,100,9007199254740993,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18, \
        1,1,0,NULL,NULL,NULL,NULL)",
            )
            .unwrap();

        let source = table_source(&connection, Table::Processes, TableSource::Staged).unwrap();
        let result = connection
            .query_row(
                &format!("SELECT user_ns,identity_availability,executable_name FROM {source}"),
                [],
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, u8>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(result, (9_007_199_254_740_993, 1, None));
    }

    #[test]
    fn preserves_legacy_event_availability() {
        // Goal: start/exec events without usage stay unavailable through normalization.
        // Method: normalize a legacy start event and query its independent availability.
        let connection = connection(Table::Events);
        connection
            .execute_batch(
                "INSERT INTO events(publication,event,run_tick,pid,kind,availability) \
        VALUES(9,7,8,42,1,3)",
            )
            .unwrap();

        let source = table_source(&connection, Table::Events, TableSource::Staged).unwrap();
        let result = connection
            .query_row(
                &format!(
                    "SELECT identity_version,identity_availability,usage_availability \
            FROM {source}"
                ),
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<u8>>(0)?,
                        row.get::<_, u8>(1)?,
                        row.get::<_, u8>(2)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(result, (None, 3, 3));
    }

    macro_rules! invalid_process {
    ($name:ident, $change:expr) => {
        #[test]
        fn $name() {
            // Goal: malformed identity/usage combinations fail before consumer queries bind.
            // Method: change one field in a readable identity with refused numeric usage.
            let connection = identity_connection("BLOB");
            connection
                .execute_batch(
                    "INSERT INTO processes(publication,pid,identity_version,identity_availability, \
                usage_availability,process_unique_id,pid_version,executable_name) \
                VALUES(9,42,1,0,1,123,4,'worker'::BLOB)",
                )
                .unwrap();
            connection.execute_batch($change).unwrap();

            let result = table_source(&connection, Table::Processes, TableSource::Staged);

            assert!(
                matches!(result, Err(persistence::Error::Bound(_))),
                "{result:?}"
            );
        }
    };
}

    invalid_process!(
        rejects_a_partial_identity_extension,
        "ALTER TABLE processes DROP COLUMN app_path"
    );
    invalid_process!(
        rejects_a_null_identity_version,
        "UPDATE processes SET identity_version=NULL"
    );
    invalid_process!(
        rejects_unknown_identity_availability,
        "UPDATE processes SET identity_availability=7"
    );
    invalid_process!(
        rejects_unknown_usage_availability,
        "UPDATE processes SET usage_availability=3"
    );
    invalid_process!(
        rejects_refused_usage_with_a_numeric_payload,
        "UPDATE processes SET user_ns=0"
    );
    invalid_process!(
        rejects_unavailable_identity_with_a_program,
        "UPDATE processes SET identity_availability=1"
    );
    invalid_process!(
        rejects_a_creation_without_its_pid_version,
        "UPDATE processes SET pid_version=NULL"
    );
    invalid_process!(
        rejects_measured_identity_without_either_creation_field,
        "UPDATE processes SET process_unique_id=NULL,pid_version=NULL"
    );
    invalid_process!(
        rejects_an_empty_executable,
        "UPDATE processes SET executable_name=''::BLOB"
    );
    invalid_process!(
        rejects_an_executable_containing_a_separator,
        "UPDATE processes SET executable_name='bin/worker'::BLOB"
    );
    invalid_process!(
        rejects_an_executable_containing_a_nul,
        r"UPDATE processes SET executable_name='worker\x00'::BLOB"
    );
    invalid_process!(
        rejects_an_oversized_executable,
        "UPDATE processes SET executable_name=repeat('a',4096)::BLOB"
    );
    invalid_process!(
        rejects_a_relative_app_path,
        "UPDATE processes SET app_path='Engine.app'::BLOB"
    );
    invalid_process!(
        rejects_a_non_bundle_app_path,
        "UPDATE processes SET app_path='/Applications/Engine'::BLOB"
    );
    invalid_process!(
        rejects_an_inner_bundle_as_the_app,
        "UPDATE processes SET app_path='/Engine.app/Helpers/Worker.app'::BLOB"
    );
    invalid_process!(
        rejects_an_app_path_containing_a_nul,
        r"UPDATE processes SET app_path='/Eng\x00ine.app'::BLOB"
    );
    invalid_process!(
        rejects_an_oversized_app_path,
        "UPDATE processes SET app_path=('/' || repeat('a',4091) || '.app')::BLOB"
    );
    invalid_process!(
        rejects_identity_without_a_publication,
        "UPDATE processes SET publication=NULL"
    );
    invalid_process!(
        rejects_identity_without_a_pid,
        "UPDATE processes SET pid=NULL"
    );

    #[test]
    fn reopens_legacy_and_augmented_parquet_without_changing_either_file() {
        // Goal: mixed histories retain exact original counters and captured non-Unicode bytes.
        // Method: query two real Parquet files through the public saved-table boundary.
        let files = ParquetFiles::new();
        let legacy = connection(Table::Processes);
        legacy
            .execute_batch(
                "INSERT INTO processes VALUES \
        (9,41,100,9007199254740993,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18)",
            )
            .unwrap();
        files.write(&legacy, "legacy");
        let augmented = identity_connection("BLOB");
        augmented
            .execute_batch(
                r"INSERT INTO processes VALUES
        (10,42,100,9007199254740995,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,
        1,0,0,123,-4,'worker\xFF'::BLOB,'/Applications/Engine.app'::BLOB)",
            )
            .unwrap();
        files.write(&augmented, "augmented");
        let before = (files.read("legacy"), files.read("augmented"));
        let reader = connection(Table::Ticks);

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
        let mut statement = reader
            .prepare(&format!(
                "SELECT user_ns,executable_name,identity_availability FROM {old} UNION ALL \
        SELECT user_ns,executable_name,identity_availability FROM {new} ORDER BY user_ns"
            ))
            .unwrap();
        let result: Vec<_> = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, u8>(2)?,
                ))
            })
            .unwrap()
            .collect::<duckdb::Result<_>>()
            .unwrap();

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
        let connection = identity_table(Table::Events, "BLOB");
        connection
            .execute_batch(
                "INSERT INTO events(publication,event,run_tick,pid,kind, \
        availability,identity_version,identity_availability,usage_availability, \
        process_unique_id,pid_version,executable_name) \
        VALUES(9,7,8,42,1,3,1,0,3,123,4,'worker'::BLOB)",
            )
            .unwrap();

        let source = table_source(&connection, Table::Events, TableSource::Staged).unwrap();
        let result = connection
            .query_row(
                &format!(
                    "SELECT executable_name,usage_availability, \
        user_ns FROM {source}"
                ),
                [],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, u8>(1)?,
                        row.get::<_, Option<u64>>(2)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(result, (b"worker".to_vec(), 3, None));
    }

    #[test]
    fn refuses_an_event_whose_two_usage_discriminators_disagree() {
        // Goal: old and identity-aware readers cannot disagree about whether usage was read.
        // Method: disagree between refused/gone codes with an otherwise valid NULL payload.
        let connection = identity_table(Table::Events, "BLOB");
        connection
            .execute_batch(
                "INSERT INTO events(publication,pid,availability, \
        identity_version,identity_availability,usage_availability,process_unique_id, \
        pid_version,executable_name) VALUES(9,42,1,1,0,2,123,4,'worker'::BLOB)",
            )
            .unwrap();

        let result = table_source(&connection, Table::Events, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn preserves_paths_at_the_capture_byte_limit() {
        // Goal: the reader accepts every path byte the bounded capture scratch can retain.
        // Method: normalize maximum-length executable and outer App bytes with signed version.
        let connection = identity_connection("BLOB");
        let executable = vec![0xff_u8; 4095];
        let mut app = vec![b'a'; 4095];
        app[0] = b'/';
        app[4091..].copy_from_slice(b".app");
        connection
            .execute(
                "INSERT INTO processes(publication,pid,identity_version, \
        identity_availability,usage_availability,process_unique_id,pid_version, \
        executable_name,app_path) VALUES(9,42,1,0,1,123,-2147483648,?,?)",
                duckdb::params![&executable, &app],
            )
            .unwrap();

        let source = table_source(&connection, Table::Processes, TableSource::Staged).unwrap();
        let result = connection
            .query_row(
                &format!("SELECT executable_name,app_path,pid_version FROM {source}"),
                [],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, i32>(2)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(result, (executable, app, i32::MIN));
    }

    #[test]
    fn refuses_a_table_exceeding_the_reader_row_bound() {
        // Goal: identity validation cannot scan an unbounded extended table.
        // Method: create one more row than the public reader's permitted million-row result.
        let connection = identity_connection("BLOB");
        connection
            .execute_batch(
                "INSERT INTO processes(publication,pid,identity_version, \
        identity_availability,usage_availability,process_unique_id,pid_version,executable_name) \
        SELECT 9,42,1,0,1,123,4,'worker'::BLOB FROM range(1048577)",
            )
            .unwrap();

        let result = table_source(&connection, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn refuses_legacy_normalization_exceeding_the_column_bound() {
        // Goal: typed legacy metadata cannot expand a result past the viewer's 32 columns.
        // Method: add five otherwise unrelated columns to the 21-column legacy table.
        let connection = connection(Table::Processes);
        connection
            .execute_batch(
                "ALTER TABLE processes ADD COLUMN extra1 UBIGINT; \
            ALTER TABLE processes ADD COLUMN extra2 UBIGINT; \
            ALTER TABLE processes ADD COLUMN extra3 UBIGINT; \
            ALTER TABLE processes ADD COLUMN extra4 UBIGINT; \
            ALTER TABLE processes ADD COLUMN extra5 UBIGINT",
            )
            .unwrap();

        let result = table_source(&connection, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn refuses_a_case_variant_partial_identity_extension() {
        // Goal: case-insensitive SQL binding cannot hide identity metadata from validation.
        // Method: add only an uppercase version column that conflicts with the legacy alias.
        let connection = connection(Table::Processes);
        connection
            .execute_batch(
                "ALTER TABLE processes ADD COLUMN IDENTITY_VERSION UTINYINT; \
            INSERT INTO processes(publication,pid,IDENTITY_VERSION) VALUES(9,42,9)",
            )
            .unwrap();

        let result = table_source(&connection, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn refuses_a_saved_case_variant_partial_identity_extension() {
        // Goal: saved input cannot disguise a partial extension through identifier casing.
        // Method: write mixed-case metadata to Parquet and open the public saved boundary.
        let files = ParquetFiles::new();
        let source = connection(Table::Processes);
        source
            .execute_batch(
                "ALTER TABLE processes ADD COLUMN Identity_Version UTINYINT; \
            INSERT INTO processes(publication,pid,Identity_Version) VALUES(9,42,9)",
            )
            .unwrap();
        files.write(&source, "case-variant");
        let reader = connection(Table::Ticks);

        let result = table_source(
            &reader,
            Table::Processes,
            TableSource::Parquet(&files.path("case-variant")),
        );

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn refuses_noncanonical_spelling_in_a_complete_identity_extension() {
        // Goal: every supported source exposes the canonical field names consumers read.
        // Method: change only spelling in an otherwise supported typed extension.
        let connection = identity_connection("BLOB");
        connection
            .execute_batch("ALTER TABLE processes RENAME app_path TO APP_PATH")
            .unwrap();

        let result = table_source(&connection, Table::Processes, TableSource::Staged);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    fn identity_connection(executable_type: &str) -> Connection {
        identity_table(Table::Processes, executable_type)
    }

    fn identity_table(table: Table, executable_type: &str) -> Connection {
        let connection = connection(table);
        let name = table.name();
        connection
            .execute_batch(&format!(
                "ALTER TABLE {name} ADD COLUMN identity_version UTINYINT; \
            ALTER TABLE {name} ADD COLUMN identity_availability UTINYINT; \
            ALTER TABLE {name} ADD COLUMN usage_availability UTINYINT; \
            ALTER TABLE {name} ADD COLUMN process_unique_id UBIGINT; \
            ALTER TABLE {name} ADD COLUMN pid_version INTEGER; \
            ALTER TABLE {name} ADD COLUMN executable_name {executable_type}; \
            ALTER TABLE {name} ADD COLUMN app_path BLOB"
            ))
            .unwrap();
        connection
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

        fn write(&self, connection: &Connection, name: &str) {
            let path = persistence::query::parquet_source(&self.path(name)).unwrap();
            connection
                .execute_batch(&format!(
                    "COPY processes TO '{path}' (FORMAT PARQUET, COMPRESSION ZSTD)"
                ))
                .unwrap();
        }
    }

    impl Drop for ParquetFiles {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn connection(table: Table) -> Connection {
        let config = duckdb::Config::default().threads(1).unwrap();
        let connection = Connection::open_in_memory_with_flags(config).unwrap();
        let (_, columns, _) = TABLES
            .iter()
            .find(|entry| entry.0.name() == table.name())
            .unwrap();
        connection
            .execute_batch(&format!("CREATE TABLE {} ({columns})", table.name()))
            .unwrap();
        connection
    }
}
