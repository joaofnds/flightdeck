//! Literal metric expectations independent of the production encoder and projection.
use super::*;
use polars::prelude::*;

macro_rules! literal_columns {
    ($rows:expr; $($name:literal => $value:expr),+ $(,)?) => {
        vec![$(Column::new($name.into(), $rows.iter().map($value).collect::<Vec<_>>())),+]
    };
}

pub(super) fn check(
    object: &Path,
    captured: bool,
    count: usize,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    assert!((1..=61).contains(&count));
    for name in [
        "ticks",
        "memory",
        "cpus",
        "interfaces",
        "gpu_states",
        "core_states",
        "fans",
        "processes",
        "coalitions",
    ] {
        let expected = expected(name, captured, count);
        let table = crate::engine::AdmittedTable::from_file_budgeted(
            File::open(object.join("ready").join(format!("{name}.parquet"))).unwrap(),
            budget,
        )
        .unwrap();
        let names: Vec<_> = expected
            .columns()
            .iter()
            .map(|column| column.name().as_str())
            .collect();
        let actual = table.read_metadata_budgeted(&names, None).unwrap();
        assert_eq!(
            table.fields().count(),
            expected.width(),
            "{name} column count"
        );
        assert_eq!(actual.height(), expected.height(), "{name} row count");
        for column in expected.columns() {
            let actual = actual.column(column.name().as_str()).unwrap();
            assert_eq!(
                actual.dtype(),
                column.dtype(),
                "{name}/{} type",
                column.name()
            );
            assert!(
                actual.equals_missing(column),
                "{name}/{} literal values",
                column.name()
            );
        }
    }
}

pub(super) fn expected(name: &str, captured: bool, count: usize) -> DataFrame {
    let columns = match name {
        "ticks" => ticks(count),
        "memory" => memory(count),
        "cpus" => cpus(count),
        "interfaces" => interfaces(count),
        "gpu_states" => gpu(count),
        "core_states" => cores(count),
        "fans" => fans(count),
        "processes" => processes(count, captured),
        "coalitions" => coalitions(count),
        _ => unreachable!(),
    };
    DataFrame::new(columns[0].len(), columns).unwrap()
}

fn rows(count: usize, keys: u64) -> Vec<(u64, u64)> {
    assert!(count <= 61);
    assert!(keys <= 40);
    (0..keys)
        .flat_map(|key| (0..count).map(move |tick| (collector::u64_from_usize(tick) + 2, key)))
        .collect()
}

fn ticks(count: usize) -> Vec<Column> {
    let rows = rows(count, 1);
    literal_columns!(rows;
        "publication" => |&(publication, _)| publication,
        "run_tick" => |&(publication, _)| publication - 2,
        "wall_ns" => |&(publication, _)| Some(publication + 8),
        "continuous_ns" => |&(publication, _)| Some((publication - 2).min(59) * 1_000_000_000 + 1),
        "awake_ns" => |&(publication, _)| Some((publication - 2) * 1_000_000_000 + 3),
        "ioreport_read" => |_| true,
        "smc_read" => |&(publication, _)| publication % 2 == 0,
        "gpu_energy_nj" => |_| Some(123u64),
        "power_watts" => |&(publication, _)| (publication % 2 == 0).then_some(42.25f32),
        "soc_celsius" => |&(publication, _)| (publication % 2 == 0).then_some(51.5f32),
        "processes_refused" => |_| 0u32,
        "processes_gone" => |_| 0u32,
        "events_first" => |&(publication, _)| if publication == 2 { 1u64 } else { 3 },
        "events_end" => |_| 3u64,
        "dropped_starts" => |_| 0u64,
        "dropped_execs" => |_| 0u64,
        "dropped_exits" => |_| 0u64,
        "gpu_state_count" => |_| 3u32,
        "core_count" => |_| 2u32,
        "cpu_celsius" => |&(publication, _)| (publication % 2 == 0).then_some(52.25f32),
        "gpu_celsius" => |_| None::<f32>,
    )
}

fn memory(count: usize) -> Vec<Column> {
    let rows = rows(count, 1);
    literal_columns!(rows;
        "publication" => |&(publication, _)| publication,
        "total_bytes" => |_| 1u64,
        "free_bytes" => |_| 2u64,
        "active_bytes" => |_| 3u64,
        "inactive_bytes" => |_| 4u64,
        "speculative_bytes" => |_| 5u64,
        "wired_bytes" => |_| 6u64,
        "compressed_bytes" => |_| 7u64,
        "anonymous_bytes" => |_| 8u64,
        "file_backed_bytes" => |_| 9u64,
        "purgeable_bytes" => |_| 10u64,
        "swap_total_bytes" => |_| 11u64,
        "swap_used_bytes" => |_| 12u64,
        "swapped_in_bytes" => |_| 13u64,
        "swapped_out_bytes" => |_| 14u64,
        "pageins" => |_| 15u64,
        "pageouts" => |_| 16u64,
        "compressions" => |_| 17u64,
        "decompressions" => |_| 18u64,
        "pressure" => |_| 1u8,
    )
}

fn cpus(count: usize) -> Vec<Column> {
    let rows = rows(count, 2);
    literal_columns!(rows;
        "publication" => |&(publication, _)| publication,
        "cpu" => |&(_, key)| u32::try_from(key).unwrap(),
        "user_ticks" => |_| 1u32,
        "system_ticks" => |_| 2u32,
        "nice_ticks" => |_| 3u32,
        "idle_ticks" => |_| u32::MAX,
    )
}

fn interfaces(count: usize) -> Vec<Column> {
    let rows = rows(count, 2);
    literal_columns!(rows;
        "publication" => |&(publication, _)| publication,
        "interface" => |&(_, key)| u16::try_from(key).unwrap(),
        "name" => |_| "fixture0",
        "received_bytes" => |&(_, key)| key + 1,
        "sent_bytes" => |&(_, key)| key + 2,
        "received_packets" => |&(_, key)| key + 3,
        "sent_packets" => |&(_, key)| key + 4,
        "receive_errors" => |&(_, key)| key + 5,
        "send_errors" => |&(_, key)| key + 6,
        "receive_drops" => |&(_, key)| key + 7,
    )
}

fn gpu(count: usize) -> Vec<Column> {
    let rows = rows(count, 3);
    literal_columns!(rows;
        "publication" => |&(publication, _)| publication,
        "state" => |&(_, key)| u32::try_from(key).unwrap(),
        "active_ns" => |_| 41u64,
    )
}

fn cores(count: usize) -> Vec<Column> {
    let rows = rows(count, 6);
    literal_columns!(rows;
        "publication" => |&(publication, _)| publication,
        "core" => |&(_, key)| u32::try_from(key / 3).unwrap(),
        "state" => |&(_, key)| u32::try_from(key % 3).unwrap(),
        "active_ns" => |_| 42u64,
    )
}

fn fans(count: usize) -> Vec<Column> {
    let rows: Vec<_> = rows(count, 2)
        .into_iter()
        .filter(|(publication, _)| publication % 2 == 0)
        .collect();
    literal_columns!(rows;
        "publication" => |&(publication, _)| publication,
        "fan" => |&(_, key)| u32::try_from(key).unwrap(),
        "rpm" => |_| 1234.5f32,
    )
}

fn processes(count: usize, captured: bool) -> Vec<Column> {
    let rows = rows(count, 40);
    let mut columns = literal_columns!(rows;
        "publication" => |&(publication, _)| publication,
        "pid" => |&(_, key)| i32::try_from(key).unwrap(),
        "awake_ns_at_start" => |&(_, key)| Some(key * 100 + 1),
        "user_ns" => |&(_, key)| Some(key * 100 + 2),
        "system_ns" => |&(_, key)| Some(key * 100 + 3),
        "energy_nj" => |&(_, key)| Some(key * 100 + 4),
        "instructions" => |&(_, key)| Some(key * 100 + 5),
        "super_instructions" => |&(_, key)| Some(key * 100 + 6),
        "cycles" => |&(_, key)| Some(key * 100 + 7),
        "idle_wakeups" => |&(_, key)| Some(key * 100 + 8),
        "interrupt_wakeups" => |&(_, key)| Some(key * 100 + 9),
        "pageins" => |&(_, key)| Some(key * 100 + 10),
        "resident_bytes" => |&(_, key)| Some(key * 100 + 11),
        "footprint_bytes" => |&(_, key)| Some(key * 100 + 12),
        "disk_read_bytes" => |&(_, key)| Some(key * 100 + 13),
        "disk_written_bytes" => |&(_, key)| Some(key * 100 + 14),
        "threads" => |_| Some(-1i32),
        "faults" => |_| Some(2i32),
        "context_switches" => |_| Some(3i32),
        "mach_syscalls" => |_| Some(i32::MAX),
        "unix_syscalls" => |_| Some(i32::MIN),
    );
    if captured {
        columns.extend(literal_columns!(rows;
            "identity_version" => |_| 1u8,
            "identity_availability" => |_| Some(0u8),
            "usage_availability" => |_| Some(0u8),
            "process_unique_id" => |&(_, key)| Some(key + 100),
            "pid_version" => |_| Some(-7i32),
            "executable_name" => |_| Some(b"metric-\xff".as_slice()),
            "app_path" => |_| Some(b"/Apps/Metric-\xfe.app".as_slice()),
        ));
    }
    columns
}

fn coalitions(count: usize) -> Vec<Column> {
    let rows = rows(count, 8);
    literal_columns!(rows;
        "publication" => |&(publication, _)| publication,
        "coalition" => |&(_, key)| key,
        "tasks_started" => |&(_, key)| key + 1,
        "tasks_exited" => |&(_, key)| key + 2,
        "cpu_ns" => |&(_, key)| key + 3,
        "interrupt_wakeups" => |&(_, key)| key + 4,
        "idle_wakeups" => |&(_, key)| key + 5,
        "disk_read_bytes" => |&(_, key)| key + 6,
        "disk_written_bytes" => |&(_, key)| key + 7,
        "gpu_ns" => |&(_, key)| key + 8,
        "energy_nj" => |&(_, key)| key + 9,
    )
}

#[test]
fn ordinary_literal_metric_oracle_numeric() {
    // Goal: every persisted metric column agrees with independent typed fixture values.
    // Method: recover two numeric ticks with different optional groups and compare all rows.
    complete(false, 2);
}

#[test]
fn ordinary_literal_metric_oracle_captured() {
    // Goal: captured metrics retain all values, nulls, creation identities and native bytes.
    // Method: recover two captured ticks and compare complete published tables to literal columns.
    complete(true, 2);
}
