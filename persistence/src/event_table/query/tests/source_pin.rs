use super::*;
use crate::engine::EventSourcePin;

#[test]
fn multipart_capture_refuses_oversized_inputs_before_the_forwarding_factory() {
    // Goal: permanent physical-part oversize cannot become retryable budget pressure.
    // Method: pin a private part one byte over the decoder's limit and compare capture with direct
    // read at two ledger sizes.
    let fixture = Fixture::new(1, 1);
    private::open_private_file_writable(&fixture.part(0))
        .unwrap()
        .set_len(collector::u64_from_usize(
            crate::event_observation::BATCH_BYTES_MAX * 3 + 1,
        ))
        .unwrap();
    for maximum in [64 * 1024 * 1024, 256 * 1024 * 1024] {
        let budget = Arc::new(ByteBudget::new(maximum));
        let table = fixture.table(&budget);
        let direct = table.part(0).unwrap().read_slice(0, 1, &budget);
        assert!(matches!(
            direct,
            Err(AdmittedReadRefusal::Admission(AdmissionRefusal::Bound(
                "parquet file bytes"
            )))
        ));
        let trace = crate::work::ingestion::Trace::new(fixture.manifest.identity.run.recording);
        let scope = trace.enter();
        let mut calls = 0;

        let result = table
            .part(0)
            .unwrap()
            .capture_with(&budget, |file, budget| {
                calls += 1;
                crate::engine::AdmittedTable::from_file_budgeted(file, budget)
            });

        assert!(matches!(
            result,
            Err(AdmittedReadRefusal::Admission(AdmissionRefusal::Bound(
                "parquet file bytes"
            )))
        ));
        assert_eq!(calls, 0);
        let mut events = Vec::new();
        trace.snapshot(&mut events).unwrap();
        assert!(!events.iter().any(|event| matches!(
            event.kind,
            crate::work::ingestion::Kind::ContentCapture
                | crate::work::ingestion::Kind::NativeCollect
                | crate::work::ingestion::Kind::ProjectionDecode
        )));
        drop(scope);
        drop(table);
        assert_eq!(budget.used(), 0);
    }
}

fn replace(path: &std::path::Path) {
    let replacement = path.with_extension("replacement");
    std::fs::copy(path, &replacement).unwrap();
    std::fs::rename(&replacement, path).unwrap();
}

fn evict(engine: &Engine, fixture: &Fixture) {
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    for index in 0..PARQUET_SOURCES_MAX {
        let path = fixture
            .root
            .join(format!("pin_cache_{serial}_{index}.parquet"));
        let mut frame = DataFrame::new(1, vec![Column::new("number".into(), [1_u64])]).unwrap();
        crate::encoder::encode_write(&mut frame, private::open_private_file(&path, true).unwrap())
            .unwrap();
        engine.parquet_source(&path).unwrap();
    }
}

#[test]
fn refuses_same_byte_replacement_of_first_last_and_index_files() {
    // Goal: every physical identity participates even when contents and marker stay unchanged.
    // Method: replace each boundary file with the same bytes and a new inode after registration.
    for replaced in [0, 1, 2] {
        let fixture = Fixture::new(3, 2);
        let engine = Engine::new();
        let budget = budget();
        let (alias, pin) = engine
            .event_source_pinned(&fixture.root, &fixture.manifest, &budget)
            .unwrap();
        let (same_alias, same_pin) = engine
            .event_source_pinned(&fixture.root, &fixture.manifest, &budget)
            .unwrap();
        assert_eq!(alias, same_alias);
        assert_eq!(pin, same_pin);
        let path = match replaced {
            0 => fixture.part(0),
            1 => fixture.part(2),
            2 => fixture.part(0).with_file_name("index"),
            _ => unreachable!(),
        };
        replace(&path);
        assert!(!pin.validate().unwrap());
        assert!(
            engine
                .event_source(&fixture.root, &fixture.manifest, &budget)
                .is_err()
        );
        assert!(
            engine
                .query(&format!("SELECT event FROM {alias} LIMIT 1"))
                .is_err()
        );
        let refreshed = Engine::new();
        let (new_alias, new_pin) = refreshed
            .event_source_pinned(&fixture.root, &fixture.manifest, &budget)
            .unwrap();
        assert_eq!(alias, new_alias);
        assert_ne!(pin, new_pin);
        assert!(new_pin.validate().unwrap());
    }
}

#[test]
fn preserves_exact_identity_across_eviction_and_detects_replacement_after_eviction() {
    // Goal: stable short aliases do not hide changed parts when the registration was evicted.
    // Method: retain a pin and source buffers across eviction, rebind unchanged, then replace.
    let fixture = Fixture::new(3, 2);
    let engine = Engine::new();
    let budget = budget();
    let (alias, pin) = engine
        .event_source_pinned(&fixture.root, &fixture.manifest, &budget)
        .unwrap();
    let result = engine.query(&format!("SELECT event FROM {alias}")).unwrap();
    evict(&engine, &fixture);
    let (same_alias, same_pin) = engine
        .event_source_pinned(&fixture.root, &fixture.manifest, &budget)
        .unwrap();
    assert_eq!(alias, same_alias);
    assert_eq!(pin, same_pin);
    evict(&engine, &fixture);
    replace(&fixture.part(2));
    let (changed_alias, changed_pin) = engine
        .event_source_pinned(&fixture.root, &fixture.manifest, &budget)
        .unwrap();
    assert_eq!(alias, changed_alias);
    assert_ne!(pin, changed_pin);
    assert!(!pin.validate().unwrap());
    drop(engine);
    drop(pin);
    drop(same_pin);
    drop(changed_pin);
    assert!(budget.used() > 0);
    assert_eq!(
        result.column("event").unwrap().u64().unwrap().get(5),
        Some(6)
    );
    drop(result);
    assert_budget_drains(&budget);
}

#[test]
fn cloned_pin_keeps_one_exact_descriptor_charge_after_source_drop() {
    // Goal: cached and caller-owned pins share one bounded allocation and retain its reservation.
    // Method: drop the source, clone its pin, then release both while observing the shared ledger.
    let fixture = Fixture::new(3, 2);
    let engine = Engine::new();
    let budget = budget();
    let (_alias, pin) = engine
        .event_source_pinned(&fixture.root, &fixture.manifest, &budget)
        .unwrap();
    drop(engine);
    let retained = budget.used();
    assert!(retained > 0);
    let cloned = pin.clone();
    assert_eq!(budget.used(), retained);
    drop(pin);
    assert_eq!(budget.used(), retained);
    assert!(cloned.validate().unwrap());
    println!("three-part pin retained_bytes={retained}");
    drop(cloned);
    assert_eq!(budget.used(), 0);
}

#[test]
fn validates_the_callers_commitment_even_when_the_source_is_cached() {
    // Goal: equal file identities cannot turn a mismatched caller manifest into a cache hit.
    // Method: register once, change only the expected row count, and request the cached source.
    let fixture = Fixture::new(2, 2);
    let engine = Engine::new();
    let budget = budget();
    let alias = fixture.source(&engine, &budget);
    let mut changed = fixture.manifest.clone();
    changed
        .tables
        .iter_mut()
        .find(|table| table.name == "events")
        .unwrap()
        .rows += 1;
    assert!(matches!(
        engine.event_source_pinned_budgeted(&fixture.root, &changed, &budget),
        Err(AdmittedReadRefusal::Decode(Error::Bound(
            "changed event query commitment"
        )))
    ));
    assert_eq!(fixture.source(&engine, &budget), alias);
}

#[test]
fn admits_pin_allocations_before_capture_and_allocates_nothing_on_budget_refusal() {
    // Goal: descriptor capture and clones cannot escape the caller's fixed byte ledger.
    // Method: trace the actual capture, its Arc clone, and a capture under a zero-byte budget.
    let fixture = Fixture::new(3, 2);
    let budget = budget();
    let (pin, allocation) = crate::allocation_tests::observe(&budget, || {
        EventSourcePin::capture(&fixture.root, &fixture.manifest, &budget).unwrap()
    });
    assert!(allocation.allocation_charge_min > 0);
    assert!(allocation.allocated_bytes <= budget.used_peak());
    let (cloned, allocation) = crate::allocation_tests::observe(&budget, || pin.clone());
    assert_eq!(allocation.allocations, 0);
    println!(
        "pin cap_parts={} cap_bytes={} retained_bytes={} reserved_peak={}",
        crate::event_recovery::metadata::BATCHES_MAX,
        EventSourcePin::CAPTURE_BYTES_MAX,
        budget.used(),
        budget.used_peak()
    );
    drop(pin);
    drop(cloned);
    assert_eq!(budget.used(), 0);
    let empty = Arc::new(ByteBudget::new(0));
    let (refused, allocation) = crate::allocation_tests::observe(&empty, || {
        EventSourcePin::capture(&fixture.root, &fixture.manifest, &empty)
    });
    assert!(matches!(
        refused,
        Err(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))
    ));
    assert_eq!(allocation.allocations, 0);
}

#[test]
fn binds_opened_part_files_and_preserves_missing_file_errors() {
    // Goal: pin checks inspect the actual descriptor, and missing files remain operating errors.
    // Method: present a different opened file with identical bytes, then remove the original path.
    let fixture = Fixture::new(2, 2);
    let engine = Engine::new();
    let budget = budget();
    let (_alias, pin) = engine
        .event_source_pinned(&fixture.root, &fixture.manifest, &budget)
        .unwrap();
    let original = private::open_private_file(&fixture.part(1), false).unwrap();
    pin.validate_part_file(1, &original).unwrap();
    let replacement = fixture.part(1).with_extension("other");
    std::fs::copy(fixture.part(1), &replacement).unwrap();
    let different = private::open_private_file(&replacement, false).unwrap();
    assert!(pin.validate_part_file(1, &different).is_err());
    assert!(pin.validate_part_file(usize::MAX, &original).is_err());
    std::fs::remove_file(fixture.part(1)).unwrap();
    assert!(matches!(pin.validate(), Err(Error::Io(error))
        if error.kind() == std::io::ErrorKind::NotFound));
}

#[test]
fn pins_an_empty_index_without_fabricating_parts() {
    // Goal: a table with no parts keeps its index in its source identity.
    // Method: replace the empty index after opening a direct plan.
    let fixture = Fixture::new(0, 2);
    let budget = budget();
    let table = fixture.table(&budget);
    let path = fixture.part(0).with_file_name("index");
    let plan = table.into_lazy(&budget).unwrap();
    replace(&path);
    assert!(plan.collect().is_err());
}

#[test]
fn checks_identity_metadata_linearly_in_the_number_of_parts() {
    // Goal: whole-source validation plus per-open checks must not multiply full index traversals.
    // Method: count tiny tables with three and 65 parts, observing actual source identity checks.
    for parts in [3, 65] {
        let fixture = Fixture::new(parts, 1);
        let budget = budget();
        let table = fixture.table(&budget);
        let observation = crate::work::observe();
        assert_eq!(
            table.count_budgeted(None, &budget).unwrap(),
            collector::u64_from_usize(parts)
        );
        let counts = observation.counts();
        assert_eq!(counts.collections, collector::u64_from_usize(parts));
        assert_eq!(
            counts.source_metadata_checks,
            collector::u64_from_usize(6 * parts + 6)
        );
        println!(
            "count parts={parts} identity_metadata_checks={} native_collects={}",
            counts.source_metadata_checks, counts.collections
        );
    }
}

#[test]
fn observes_outer_sort_allocation_separately_from_source_leases() {
    // Goal: distinguish source-owned buffers from new buffers allocated by outer Polars transforms.
    // Method: retain a sorted primitive result after the source drops and report its ledger charge.
    let fixture = Fixture::new(2, 3);
    let engine = Engine::new();
    let budget = budget();
    let source = fixture.source(&engine, &budget);
    let result = engine
        .query(&format!("SELECT event FROM {source} ORDER BY event DESC"))
        .unwrap();
    drop(engine);
    assert_eq!(
        result
            .column("event")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
            .collect::<Vec<_>>(),
        [6, 5, 4, 3, 2, 1]
    );
    println!(
        "outer ORDER BY retained_result_rows=6 retained_ledger_bytes={}",
        budget.used()
    );
    drop(result);
    assert_budget_drains(&budget);
}
