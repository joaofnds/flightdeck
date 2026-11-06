use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::recording::private;
use collector::ring::{Identity, RunIdentity};

use super::*;
use crate::canonical::ready_fixture::ReadyWindow;
use crate::engine::{AdmissionRefusal, Engine, PARQUET_SOURCES_MAX};
use crate::event_recovery::{Batch, batch};
use crate::{Losses, Manifest, Store, TableManifest, WindowIdentity};

mod source_pin;

static NEXT: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    manifest: Manifest,
}

impl Fixture {
    fn new(parts: usize, rows: usize) -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!(
            "/private/tmp/flightdeck-event-query-{}-{serial}",
            std::process::id()
        ));
        let store = Store::open(&root).unwrap();
        let identity = WindowIdentity {
            run: RunIdentity {
                recording: store.recording(),
                run: Identity([3, 4]),
                boot: Identity([5, 6]),
            },
            window: 1,
        };
        let ready = ReadyWindow::create(&root).unwrap();
        let events = ready.path().join("events");
        private::create_private_dir(&events).unwrap();
        let mut batches = Vec::with_capacity(parts);
        for index in 0..parts {
            let first = collector::u64_from_usize(index * rows) + 1;
            let mut frame = event_frame(first, rows);
            let path = events.join(crate::event_recovery::batch_name(index));
            crate::encoder::encode_write(
                &mut frame,
                private::open_private_file(&path, true).unwrap(),
            )
            .unwrap();
            batches.push(Batch {
                first,
                end: first + collector::u64_from_usize(rows),
                rows: collector::u64_from_usize(rows),
            });
        }
        let count = collector::u64_from_usize(parts * rows);
        let index = crate::event_table::index::Index {
            identity,
            first: 1,
            end: count + 1,
            rows: count,
            identities: false,
            batches,
        };
        index.write(&events.join("index")).unwrap();
        let manifest = ready
            .prove(Manifest {
                identity,
                publication_first: 1,
                publication_end: 8,
                events_first: 1,
                events_end: count + 1,
                wall_ns: None,
                continuous_ns: None,
                awake_ns: None,
                losses: Losses::default(),
                data_directory: String::new(),
                tables: Self::new_tables(&ready, 7, count),
                event_batches: Some(u32::try_from(parts).unwrap()),
            })
            .unwrap();
        drop(store);
        Self { root, manifest }
    }

    fn new_tables(ready: &ReadyWindow, ticks: u64, events: u64) -> Vec<TableManifest> {
        crate::schema::TABLES
            .iter()
            .map(|table| {
                if table.name() != "events" {
                    let path = ready.path().join(format!("{}.parquet", table.name()));
                    private::open_private_file(&path, true).unwrap();
                }
                TableManifest {
                    name: table.name(),
                    rows: match table.name() {
                        "ticks" => ticks,
                        "events" => events,
                        _ => 0,
                    },
                }
            })
            .collect()
    }

    fn source(&self, engine: &Engine, budget: &Arc<ByteBudget>) -> String {
        engine
            .event_source(&self.root, &self.manifest, budget)
            .unwrap()
    }

    fn table(&self, budget: &Arc<ByteBudget>) -> EventTable {
        EventTable::open_budgeted(&self.root, &self.manifest, budget).unwrap()
    }

    fn part(&self, index: usize) -> PathBuf {
        self.root
            .join(&self.manifest.data_directory)
            .join("events")
            .join(crate::event_recovery::batch_name(index))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn event_frame(first: u64, count: usize) -> DataFrame {
    let rows = (0..count)
        .map(|offset| {
            let number = first + collector::u64_from_usize(offset);
            batch::Row {
                publication: number.is_multiple_of(2).then_some(7),
                event: crate::window::EventRecord {
                    number,
                    tick: number,
                    pid: i32::try_from(number % 3 + 1).unwrap(),
                    kind: crate::schema::event_kind::START,
                    coalition: None,
                    usage: None,
                    availability: crate::schema::availability::NOT_APPLICABLE,
                    noticed: None,
                },
                identity: None,
                name: None,
                app: None,
            }
        })
        .collect::<Vec<_>>();
    batch::frame(&rows, false).unwrap()
}

fn budget() -> Arc<ByteBudget> {
    Arc::new(ByteBudget::new(256 * 1024 * 1024))
}

/// Polars sends a query's result before its worker drops the plan, and the plan's buffers stay
/// charged until that drop, so a charge can outlive every value the test holds. The bound catches
/// a charge that never returns, and thirty seconds leaves the scheduler time to run the worker.
fn assert_budget_drains(budget: &ByteBudget) {
    const POLLS_MAX: u32 = 3000;

    for _ in 0..POLLS_MAX {
        if budget.used() == 0 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(budget.used(), 0);
}

#[test]
fn preserves_predicate_projection_and_explicit_order_across_parts() {
    // Goal: physical event-number storage preserves the existing explicit presentation order.
    // Method: select associated events from three parts through the SQL production source.
    let fixture = Fixture::new(3, 5);
    let engine = Engine::new();
    let budget = budget();
    let source = fixture.source(&engine, &budget);

    let actual = engine
        .query(&format!(
            "SELECT event, pid, publication FROM {source} \
         WHERE publication = 7 ORDER BY pid, awake_ns_at_start, event"
        ))
        .unwrap();

    assert_eq!(
        actual
            .column("event")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [6, 12, 4, 10, 2, 8, 14]
    );
    assert_eq!(
        actual
            .get_column_names()
            .iter()
            .map(|name| name.as_str())
            .collect::<Vec<_>>(),
        ["event", "pid", "publication"]
    );
}

#[test]
fn preserves_detached_rows_and_normalized_legacy_identity() {
    // Goal: the shared relation contains complete detached raw events with explicit provenance.
    // Method: query detached rows and unsupported identity from the normal SQL source.
    let fixture = Fixture::new(2, 3);
    let engine = Engine::new();
    let budget = budget();
    let source = fixture.source(&engine, &budget);

    let actual = engine
        .query(&format!(
            "SELECT event, publication, identity_version, identity_availability, \
         event_observation_availability FROM {source} WHERE publication IS NULL ORDER BY event"
        ))
        .unwrap();

    assert_eq!(
        actual
            .column("event")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [1, 3, 5]
    );
    assert_eq!(actual.column("publication").unwrap().null_count(), 3);
    assert_eq!(actual.column("identity_version").unwrap().null_count(), 3);
    assert_eq!(
        actual
            .column("identity_availability")
            .unwrap()
            .u8()
            .unwrap()
            .get(0),
        Some(crate::schema::identity_availability::UNSUPPORTED)
    );
    assert_eq!(
        actual
            .column("event_observation_availability")
            .unwrap()
            .u8()
            .unwrap()
            .get(0),
        Some(crate::event_observation::UNAVAILABLE)
    );
}

#[test]
fn refuses_corrupt_detached_provenance_before_filtering() {
    // Goal: an associated predicate cannot hide corruption in an excluded physical row.
    // Method: replace one detached provenance version in the last part, then query associated rows.
    let fixture = Fixture::new(3, 2);
    let mut corrupt = event_frame(5, 2);
    corrupt
        .with_column(Column::new("event_observation_version".into(), [2_u8, 1]))
        .unwrap();
    let path = fixture.part(2);
    std::fs::remove_file(&path).unwrap();
    crate::encoder::encode_write(
        &mut corrupt,
        private::open_private_file(&path, true).unwrap(),
    )
    .unwrap();
    let engine = Engine::new();
    let budget = budget();
    let source = fixture.source(&engine, &budget);

    let result = engine.query(&format!("SELECT event FROM {source} WHERE publication = 7"));

    assert!(result.is_err());
}

#[test]
fn registers_without_capturing_files_and_reads_only_a_bounded_prefix() {
    // Goal: registering a relation never captures every physical input in advance.
    // Method: collect one row, then remove an unselected part and require the next scan to refuse.
    let fixture = Fixture::new(2, 3);
    let budget = budget();
    let observation = crate::work::observe();
    let lazy = fixture.table(&budget).into_lazy(&budget).unwrap();
    assert_eq!(observation.counts().collections, 0);
    let actual = lazy
        .clone()
        .limit(1)
        .select([col("event")])
        .collect()
        .unwrap();

    assert_eq!(
        actual.column("event").unwrap().u64().unwrap().get(0),
        Some(1)
    );
    assert_eq!(observation.counts().collections, 1);
    std::fs::remove_file(fixture.part(1)).unwrap();
    assert!(lazy.limit(1).select([col("event")]).collect().is_err());
    assert_eq!(observation.counts().collections, 1);
}

#[test]
fn keeps_result_buffers_charged_after_cache_eviction_and_rebinds_the_alias() {
    // Goal: eviction frees source state without uncharging returned event buffers or stale aliases.
    // Method: retain a SQL result across 128 other sources, then refresh and reuse its alias.
    let fixture = Fixture::new(1, 2);
    let engine = Engine::new();
    let budget = budget();
    let source = fixture.source(&engine, &budget);
    let result = engine
        .query(&format!("SELECT event, publication FROM {source}"))
        .unwrap();
    for index in 0..PARQUET_SOURCES_MAX {
        let path = fixture.root.join(format!("cache_{index}.parquet"));
        let mut frame = DataFrame::new(
            1,
            vec![Column::new(
                "number".into(),
                [u64::try_from(index).unwrap()],
            )],
        )
        .unwrap();
        crate::encoder::encode_write(&mut frame, private::open_private_file(&path, true).unwrap())
            .unwrap();
        engine.parquet_source(&path).unwrap();
    }
    assert!(
        engine
            .query(&format!("SELECT event FROM {source}"))
            .is_err()
    );
    assert!(budget.used() > 0);
    assert_eq!(fixture.source(&engine, &budget), source);
    drop(engine);
    assert!(budget.used() > 0);
    assert_eq!(result.height(), 2);
    drop(result);
    assert_budget_drains(&budget);
}

#[test]
fn separates_the_same_source_under_distinct_ledgers() {
    // Goal: a cache hit cannot silently charge a different caller's budget.
    // Method: register the same marker under two ledgers and inspect their stable aliases.
    let fixture = Fixture::new(1, 2);
    let engine = Engine::new();
    let first = budget();
    let second = budget();

    let first_alias = fixture.source(&engine, &first);
    let second_alias = fixture.source(&engine, &second);

    assert_ne!(first_alias, second_alias);
    assert_eq!(fixture.source(&engine, &first), first_alias);
    assert!(first.used() > 0);
    assert!(second.used() > 0);
}

#[test]
fn keeps_budget_refusal_retryable_at_direct_count_and_source_apis() {
    // Goal: budgeted entry points preserve a retryable refusal without claiming corruption.
    // Method: admit a source under a sufficient ledger and count under an empty one.
    let fixture = Fixture::new(1, 2);
    let budget = budget();
    let table = fixture.table(&budget);
    let empty = Arc::new(ByteBudget::new(0));

    let count = table.count_budgeted(None, &empty);
    let source = Engine::new().event_source_budgeted(&fixture.root, &fixture.manifest, &empty);

    assert!(matches!(
        count,
        Err(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))
    ));
    assert!(matches!(
        source,
        Err(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))
    ));
    assert_eq!(empty.used(), 0);
}

#[test]
fn preserves_filter_limit_and_offset_semantics() {
    // Goal: pushed prefixes must not limit the input before a filter that precedes the slice.
    // Method: compare SQL filter/limit and explicit pre-filter slicing across part boundaries.
    let fixture = Fixture::new(3, 4);
    let engine = Engine::new();
    let budget = budget();
    let source = fixture.source(&engine, &budget);

    let filtered = engine
        .query(&format!(
            "SELECT event FROM {source} WHERE publication = 7 LIMIT 3 OFFSET 1"
        ))
        .unwrap();
    let prefix = fixture
        .table(&budget)
        .into_lazy(&budget)
        .unwrap()
        .limit(5)
        .filter(col("publication").eq(lit(7_u64)))
        .select([col("event")])
        .collect()
        .unwrap();

    assert_eq!(
        filtered
            .column("event")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [4, 6, 8]
    );
    assert_eq!(
        prefix
            .column("event")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [2, 4]
    );
}

#[test]
fn preserves_ordered_filtered_pages_with_runtime_sort_filters() {
    // Goal: native top-k hints must not be round-tripped as executable DSL expressions.
    // Method: request a descending filtered SQL page, which injects a dynamic sort predicate.
    let fixture = Fixture::new(3, 4);
    let engine = Engine::new();
    let budget = budget();
    let source = fixture.source(&engine, &budget);

    let actual = engine
        .query(&format!(
            "SELECT event FROM {source} WHERE publication = 7 ORDER BY event DESC LIMIT 3 OFFSET 1"
        ))
        .unwrap();

    assert_eq!(
        actual
            .column("event")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [10, 8, 6]
    );
}

fn runtime_hint(label: &str, inputs: Vec<Expr>) -> Expr {
    Expr::Display {
        inputs,
        fmt_str: Box::new(label.into()),
    }
}

const HINT: &str = "dynamic_pred: 3260e925-7c60-47b5-a856-7b19c77b25ea";

#[test]
fn removes_only_complete_runtime_hint_conjuncts_and_keeps_null_semantics() {
    // Goal: omitting a redundant sort hint cannot broaden or narrow a semantic null predicate.
    // Method: sanitize two known hints around a mixed null predicate and compare real rows.
    let semantic = col("publication").is_null().or(col("event").gt(lit(5_u64)));
    let predicate = runtime_hint(HINT, vec![col("event")])
        .and(semantic.clone())
        .and(runtime_hint(
            "dynamic_pred: dropped",
            vec![col("publication")],
        ));
    let frame = event_frame(1, 8);

    let actual = select(
        frame.clone(),
        scan_predicate(&predicate).unwrap().as_ref(),
        None,
    )
    .unwrap();
    let expected = select(frame, Some(&semantic), None).unwrap();

    assert!(actual.equals_missing(&expected));
    assert_eq!(actual.height(), 6);
}

#[test]
fn refuses_runtime_hints_inside_or() {
    // Goal: dropping a hint inside OR would change the user's semantic predicate.
    // Method: nest a valid hint under OR and demand an error before native execution.
    let predicate = col("publication")
        .is_null()
        .or(runtime_hint(HINT, vec![col("event")]));

    assert!(matches!(
        scan_predicate(&predicate),
        Err(Error::Bound(
            "unsupported event query nested runtime predicate"
        ))
    ));
}

#[test]
fn refuses_runtime_hints_inside_another_expression() {
    // Goal: the adapter never rewrites a nested display node as an executable value.
    // Method: hide a valid hint under a cast and refuse it before native collection.
    let predicate = runtime_hint(HINT, vec![col("event")]).cast(DataType::Boolean);

    assert!(scan_predicate(&predicate).is_err());
}

#[test]
fn refuses_unrecognized_or_malformed_runtime_hints() {
    // Goal: only the pinned display spelling and a real source-column input are recognized.
    // Method: enumerate malformed labels and inputs around the complete accepted UUID shape.
    for label in [
        "",
        "unknown",
        "dynamic_pred:",
        "dynamic_pred: ",
        "dynamic_pred: 3260e9257c60-47b5-a856-7b19c77b25ea",
        "dynamic_pred: 3260e925-7c60-47b5-a856-7b19c77b25ez",
        "dynamic_pred: 3260e925-7C60-47b5-a856-7b19c77b25ea",
        "dynamic_pred: dropped extra",
    ] {
        assert!(
            scan_predicate(&runtime_hint(label, vec![col("event")])).is_err(),
            "{label}"
        );
    }
    for inputs in [
        vec![],
        vec![col("event"), col("pid")],
        vec![lit(1_u64)],
        vec![col("not_an_event_column")],
    ] {
        assert!(scan_predicate(&runtime_hint(HINT, inputs)).is_err());
    }
}

#[test]
fn bounds_predicate_nodes_before_expanding_a_wide_expression() {
    // Goal: examining a foreign expression cannot grow an unbounded traversal buffer.
    // Method: supply one wide display node whose children exceed the fixed traversal bound.
    let predicate = runtime_hint(HINT, vec![col("event"); PREDICATE_NODES_MAX]);

    assert!(matches!(
        scan_predicate(&predicate),
        Err(Error::Bound("event query predicate nodes"))
    ));
}

#[test]
fn preserves_join_and_filtered_ordered_page_results() {
    // Goal: source predicates remain correct under joins and an ordered page above the join.
    // Method: join a complete logical event table with selected native event identities.
    let fixture = Fixture::new(3, 4);
    let engine = Engine::new();
    let budget = budget();
    let source = fixture.source(&engine, &budget);
    engine
        .register(
            "selected",
            DataFrame::new(4, vec![Column::new("number".into(), [2_u64, 5, 8, 10])]).unwrap(),
        )
        .unwrap();

    let actual = engine
        .query(&format!(
            "SELECT e.event FROM {source} e INNER JOIN selected s ON e.event = s.number \
         WHERE e.publication = 7 ORDER BY e.event DESC LIMIT 2 OFFSET 1"
        ))
        .unwrap();

    assert_eq!(
        actual
            .column("event")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [8, 2]
    );
}

#[test]
fn supplies_the_complete_empty_schema_without_a_physical_part() {
    // Goal: a zero-row table has a usable complete schema without a fabricated file or event.
    // Method: execute projection and count over an empty committed table.
    let fixture = Fixture::new(0, 0);
    let engine = Engine::new();
    let budget = budget();
    let source = fixture.source(&engine, &budget);

    let result = engine
        .query(&format!("SELECT event, publication FROM {source}"))
        .unwrap();

    assert_eq!(result.height(), 0);
    assert_eq!(
        fixture
            .table(&budget)
            .count_budgeted(None, &budget)
            .unwrap(),
        0
    );
    assert_eq!(
        EventTable::empty_frame().unwrap().width(),
        model::catalog::EVENTS.column_count()
    );
}

#[test]
fn counts_more_than_the_output_cap_without_materializing_or_truncating() {
    // Goal: a complete count may exceed the old output cap while raw materialization refuses.
    // Method: reduce 65 full physical batches, then attempt an over-cap single-column result.
    let rows = crate::event_observation::BATCH_ROWS_MAX;
    let fixture = Fixture::new(65, rows);
    let budget = budget();
    let table = fixture.table(&budget);
    let observation = crate::work::observe();

    let count = table.count_budgeted(None, &budget).unwrap();
    assert_eq!(observation.counts().collections, 65);
    assert_eq!(count, collector::u64_from_usize(65 * rows));
    assert!(count > crate::schema::IDENTITY_ROWS_MAX);
    let scan = EventScan {
        table,
        budget: Arc::clone(&budget),
    };
    let options = AnonymousScanArgs {
        n_rows: None,
        with_columns: Some(Arc::from([PlSmallStr::from_static("event")])),
        schema: Arc::new(schema()),
        output_schema: Some(Arc::new(Schema::from_iter([(
            PlSmallStr::from_static("event"),
            DataType::UInt64,
        )]))),
        predicate: None,
    };
    let result = scan.scan_budgeted(&options);

    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Decode(Error::Bound(
            "event query output rows; use event-identity pages"
        )))
    ));
    let engine = Engine::new();
    let source = fixture.source(&engine, &budget);
    let count_result = engine.query(&format!("SELECT COUNT(*) FROM {source}"));
    assert!(count_result.is_err());
    let first = crate::schema::IDENTITY_ROWS_MAX + 1;
    let page = engine
        .query(&format!(
            "SELECT event, publication FROM {source} WHERE event >= {first} AND event < {} \
         ORDER BY event LIMIT 8",
            first + 8
        ))
        .unwrap();
    assert_eq!(
        page.column("event")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        (first..first + 8).collect::<Vec<_>>()
    );
    assert_eq!(page.column("publication").unwrap().null_count(), 4);
    eprintln!(
        "validated count={count} batches=65 ledger_peak={} output_cap={}",
        budget.used_peak(),
        crate::schema::IDENTITY_ROWS_MAX
    );
    assert!(budget.used_peak() < budget.maximum());
    drop(scan);
    drop(engine);
    drop(page);
    assert_budget_drains(&budget);
}
