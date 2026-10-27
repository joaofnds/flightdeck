#![cfg(test)]
use std::sync::Arc;
use viewer::live_app_response::{App, Key, KeyKind, Measure, RankedApp};
use viewer::store::{AppRow, AppsSecond, ByteBudget};

#[test]
fn a_shared_app_identity_keeps_its_charge_after_the_query_owner_drops() {
    // Goal: sharing a charged identity cannot release its charge before its backing storage.
    // Method: detach real App rows, clone their public identity owner, then drop the context.
    let source = source();
    let budget = Arc::new(ByteBudget::new(16_384));
    let context = source.detached(&budget).unwrap();
    drop(source);
    let name = Arc::clone(&context.apps[0].name);
    let backing = name.bytes.capacity() + name.display_name.capacity();
    assert!(backing > 0);

    drop(context);

    assert_eq!(name.bytes, b"x");
    assert!(
        budget.used() >= 2 * size_of::<usize>() + size_of::<RankedApp>() + backing,
        "a retained identity has backing storage after its shared charge returned to zero"
    );
    drop(name);
    assert_eq!(budget.used(), 0);
}

#[test]
fn returned_rankings_keep_the_same_identity_charge_after_the_context_drops() {
    // Goal: returned ranks cannot outlive the charge of their shared identity.
    // Method: rank a detached second, release its source/context, then release the rankings.
    let source = source();
    let budget = Arc::new(ByteBudget::new(16_384));
    let context = source.detached(&budget).unwrap();
    let ranks = context.ranks().unwrap();
    let charge = context.apps[0].name.reserved_bytes();
    assert!(charge > 0);
    assert!(Arc::ptr_eq(
        &ranks.metrics[0].entries[0].app,
        &context.apps[0].name
    ));
    assert!(Arc::ptr_eq(
        &ranks.metrics[1].entries[0].app,
        &context.apps[0].name
    ));

    drop((source, context));

    assert_eq!(budget.used(), charge);
    assert_eq!(ranks.metrics[0].entries[0].app.bytes, b"x");
    drop(ranks);
    assert_eq!(budget.used(), 0);
}

#[test]
fn detached_readers_share_one_identity_charge_without_pinning_each_others_rows() {
    // Goal: another context reuses admitted identities while keeping independent row ownership.
    // Method: detach twice, retain one row, and release the contexts independently.
    let source = source();
    let budget = Arc::new(ByteBudget::new(16_384));
    let first = source.detached(&budget).unwrap();
    let second = first.detached(&budget).unwrap();
    let row = second.apps[0].clone();
    let charge = row.name.reserved_bytes();
    assert!(Arc::ptr_eq(&first.apps[0].name, &second.apps[0].name));
    assert_eq!(budget.used(), first.bytes() + second.bytes() + charge);

    drop((source, first, second));

    assert_eq!(budget.used(), charge);
    assert_eq!(row.name.bytes, b"x");
    drop(row);
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_failed_detachment_releases_partial_identity_admissions_and_can_retry() {
    // Goal: a refused second identity cannot leak the first identity's provisional admission.
    // Method: admit the rows and one of two names, refuse the second, free pressure and retry.
    let mut source = source();
    let mut other = source.apps[0].clone();
    other.name = Arc::new(RankedApp::named(
        KeyKind::Executable,
        b"y".to_vec(),
        String::from("y"),
    ));
    let mut rows = source.apps.to_vec();
    rows.push(other);
    source.apps = rows.try_into().unwrap();
    let budget = Arc::new(ByteBudget::new(16_384));
    let baseline = source.detached(&budget).unwrap();
    let required = budget.used();
    drop(baseline);
    assert_eq!(budget.used(), 0);
    let pressure = budget.reserve(budget.maximum() - required + 1).unwrap();

    let refused = source.detached(&budget);

    assert!(matches!(refused, Err(viewer::store::Missing::Budget)));
    assert_eq!(budget.used(), pressure.bytes());
    assert_eq!(source.apps[0].name.bytes, b"x");
    assert_eq!(source.apps[1].name.bytes, b"y");
    drop(pressure);
    let retried = source.detached(&budget).unwrap();
    assert_eq!(retried.apps, source.apps);
    drop(retried);
    assert_eq!(budget.used(), 0);
}

#[test]
fn identity_admission_includes_the_shared_allocation_beside_its_value_and_buffers() {
    // Goal: fitting the value and buffers cannot omit the shared allocation's header.
    // Method: admit only detached rows, the name value and its buffers, excluding its Arc header.
    let source = source();
    let row_bytes = size_of::<viewer::store::DetachedAppsSecond>() + size_of::<AppRow>();
    let name = &source.apps[0].name;
    let budget = Arc::new(ByteBudget::new(
        row_bytes + size_of::<RankedApp>() + name.bytes.capacity() + name.display_name.capacity(),
    ));

    let result = source.detached(&budget);

    assert!(matches!(result, Err(viewer::store::Missing::Budget)));
    assert_eq!(budget.used(), 0);
    assert_eq!(source.apps[0].name.bytes, b"x");
}

fn source() -> AppsSecond {
    let value = Measure {
        known: 0,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    };
    let app = App {
        key: Key {
            kind: KeyKind::Executable,
            bytes: b"x".to_vec(),
            display: String::from("x"),
        },
        display_name: String::from("x"),
        members: 1,
        live_members: 1,
        membership_complete: true,
        cpu_ns: value,
        energy_nj: value,
        gpu_ns: value,
        disk_read_bytes: value,
        disk_written_bytes: value,
        disk_io_bytes: value,
        resident_bytes: value,
        footprint_bytes: value,
    };
    let mut source = AppsSecond {
        missing: None,
        ..AppsSecond::default()
    };
    source.apps = vec![AppRow::new(
        &app,
        Arc::new(RankedApp::named(
            KeyKind::Executable,
            b"x".to_vec(),
            String::from("x"),
        )),
    )]
    .try_into()
    .unwrap();
    source.observation = Some(viewer::live_app_response::Observation {
        recording: [1, 2],
        stamp: viewer::apps::Stamp {
            boot: [3, 4],
            run: [5, 6],
            publication: 1,
            continuous_ns: Some(1_000_000_000),
            awake_ns: Some(1_000_000_000),
        },
        run_tick: 0,
        wall_ns: Some(1_000_000_000),
    });
    source.elapsed_ns = Some(1_000_000_000);
    source
}
