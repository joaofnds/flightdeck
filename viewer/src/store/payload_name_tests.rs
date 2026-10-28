use std::sync::Arc;

use super::{AppDetails, Payload, ProcessName, ProcessRow};
use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::{Bytes, RankedApp, Sample};
use crate::store::apps::TickApps;
use crate::store::{AppsSecond, ByteBudget};

#[test]
fn a_shared_process_name_keeps_its_charge_after_its_original_payload_is_gone() {
    // Goal: shared names remain admitted while the later payload retains them.
    // Method: release the original real native payload, then release its successor.
    let (budget, first, second) = native_pair();
    let first_payload = Arc::clone(first.processes.as_ref().unwrap());
    let second_payload = Arc::clone(second.processes.as_ref().unwrap());
    let names = name_bytes(&second_payload);
    assert!(names > 0);
    for (first, second) in first_payload.iter().zip(second_payload.iter()) {
        assert!(std::ptr::eq(
            first.executable_name().unwrap(),
            second.executable_name().unwrap()
        ));
    }
    drop((first, second));
    assert_eq!(
        budget.used(),
        first_payload.bytes() + second_payload.bytes() + names
    );

    drop(first_payload);

    assert_eq!(budget.used(), second_payload.bytes() + names);
    assert!(
        !second_payload[0]
            .executable_name()
            .unwrap()
            .bytes
            .is_empty()
    );
    drop(second_payload);
    assert_eq!(budget.used(), 0);
}

#[test]
fn payload_admission_counts_its_containers_without_recharging_shared_names() {
    // Goal: shared names are not charged again for every retained publication.
    // Method: compare both native payload charges with their complete row/detail allowance.
    let (budget, first, second) = native_pair();
    for apps in [&first, &second] {
        let payload = apps.processes.as_ref().unwrap();
        let bytes = 2 * size_of::<usize>()
            + size_of::<Payload>()
            + size_of_val(&payload[..])
            + size_of_val(payload.apps());
        assert_eq!(payload.bytes(), bytes);
    }
    drop((first, second));
    assert_eq!(budget.used(), 0);
}

#[test]
fn equal_names_from_independent_sources_keep_separate_admissions() {
    // Goal: equal text cannot erase an independent allocation's admission.
    // Method: retain equal names in two real payloads after releasing both sources.
    let (first_budget, first, first_next) = native_pair();
    let (other_budget, other, other_next) = native_pair();
    let first_payload = Arc::clone(first.processes.as_ref().unwrap());
    let other_payload = Arc::clone(other.processes.as_ref().unwrap());
    assert_eq!(
        first_payload[0].executable_name(),
        other_payload[0].executable_name()
    );
    assert!(!std::ptr::eq(
        first_payload[0].executable_name().unwrap(),
        other_payload[0].executable_name().unwrap()
    ));
    drop((first, first_next, other, other_next));

    assert_eq!(
        first_budget.used(),
        first_payload.bytes() + name_bytes(&first_payload)
    );
    assert_eq!(
        other_budget.used(),
        other_payload.bytes() + name_bytes(&other_payload)
    );
    drop(first_payload);
    assert_eq!(first_budget.used(), 0);
    assert!(other_budget.used() > 0);
    drop(other_payload);
    assert_eq!(other_budget.used(), 0);
}

#[test]
fn a_partial_name_refusal_releases_the_unpublished_payload_and_all_new_names() {
    // Goal: refusal cannot publish a partial process list or leak earlier name admissions.
    // Method: leave room for containers and the first name at the real ingestion boundary.
    let sample = native_sample();
    assert_eq!(sample.processes().len(), 2);
    let first_name = sample
        .processes()
        .iter()
        .filter_map(|row| row.executable_name.as_ref())
        .min_by(|left, right| left.bytes.cmp(&right.bytes))
        .unwrap();
    let payload_bytes = 2 * size_of::<usize>()
        + size_of::<Payload>()
        + sample.processes().len() * size_of::<ProcessRow>()
        + sample.apps().len() * size_of::<AppDetails>();
    let first_name_bytes = 2 * size_of::<usize>()
        + size_of::<ProcessName>()
        + first_name.bytes.len()
        + first_name.display.len();
    let app_names: usize = sample
        .apps()
        .iter()
        .map(|app| {
            2 * size_of::<usize>()
                + size_of::<RankedApp>()
                + app.key.bytes.len()
                + app.display_name.len()
        })
        .sum();
    let budget = Arc::new(ByteBudget::new(64 * 1024));
    let work_bytes =
        sample.processes().len() * (2 * size_of::<Arc<ProcessName>>() + size_of::<&Bytes>());
    let pressure = budget
        .reserve(budget.maximum() - app_names - payload_bytes - first_name_bytes - work_bytes)
        .unwrap();
    let mut refused = AppsSecond::with_room_for(sample.apps().len());

    refused.take(TickApps::Derived(&sample), None, Some(&budget));

    assert!(refused.missing.is_none());
    assert!(matches!(
        refused.processes,
        Err(crate::store::Missing::Budget)
    ));
    assert_eq!(refused.observation, Some(sample.selected));
    assert_eq!(budget.used_peak(), budget.maximum());
    assert_eq!(budget.used(), pressure.bytes() + app_names);
    drop(refused);
    assert_eq!(budget.used(), pressure.bytes());
    drop(pressure);
    let mut retry = AppsSecond::with_room_for(sample.apps().len());
    retry.take(TickApps::Derived(&sample), None, Some(&budget));
    let payload = retry.processes.as_ref().unwrap();
    assert_eq!(payload.len(), sample.processes().len());
    for (retained, original) in payload.iter().zip(sample.processes()) {
        assert_eq!(retained.pid, original.pid);
        assert_eq!(retained.creation, original.creation);
        assert_eq!(
            retained.executable_name().unwrap().bytes,
            original.executable_name.as_ref().unwrap().bytes
        );
    }
    drop(retry);
    assert_eq!(budget.used(), 0);
}

#[test]
fn an_owned_name_preserves_raw_and_display_bytes_after_its_source_is_gone() {
    // Goal: an admitted owner preserves escaped names independently of its source.
    // Method: adopt reserved copies of invalid UTF-8 and a long display, then retire the source.
    let source = Bytes {
        bytes: vec![0xff; 4096],
        display: "\\xff".repeat(4096),
    };
    let bytes = 2 * size_of::<usize>()
        + size_of::<ProcessName>()
        + source.bytes.len()
        + source.display.len();
    let budget = Arc::new(ByteBudget::new(bytes));
    let reservation = budget.reserve(bytes).unwrap();

    let name = ProcessName::from_reserved(source.clone(), reservation);

    assert_eq!(name.bytes, source.bytes);
    assert_eq!(name.display, source.display);
    assert_eq!(budget.used(), bytes);
    drop(source);
    assert_eq!(budget.used(), bytes);
    assert_eq!(name.bytes[0], 0xff);
    drop(name);
    assert_eq!(budget.used(), 0);
}

#[test]
fn rejected_name_additions_leave_the_accepted_namespace_unchanged_and_retry_exactly() {
    // Goal: partial name allocation never becomes reachable from the accepted input index.
    // Method: leave room for the first sorted name only, refuse, then retry the same real Sample.
    let sample = native_sample();
    let budget = Arc::new(ByteBudget::new(64 * 1024));
    let mut names = super::ProcessNames::new(sample.processes().len(), &budget).unwrap();
    let first = sample
        .processes()
        .iter()
        .filter_map(|row| row.executable_name.as_ref())
        .min_by(|left, right| left.bytes.cmp(&right.bytes))
        .unwrap();
    let name =
        2 * size_of::<usize>() + size_of::<ProcessName>() + first.bytes.len() + first.display.len();
    let scratch = sample.processes().len() * (size_of::<&Bytes>() + size_of::<Arc<ProcessName>>());
    let pressure = budget
        .reserve(
            budget.maximum() - budget.used() - Payload::capture_bytes(&sample) - scratch - name,
        )
        .unwrap();
    let before = budget.used();

    assert!(matches!(
        Payload::capture_with_names(&sample, &names),
        Err(crate::store::Missing::Budget)
    ));

    assert_eq!(budget.used(), before);
    assert_eq!(names.observation().0, 0);
    drop(pressure);
    let (payload, additions) = Payload::capture_with_names(&sample, &names).unwrap();
    names.commit(additions);
    assert_eq!(names.observation().0, 2);
    for (actual, expected) in payload.iter().zip(sample.processes()) {
        assert_eq!(actual.pid, expected.pid);
        assert_eq!(actual.creation, expected.creation);
        assert_eq!(
            actual.executable_name().unwrap().bytes,
            expected.executable_name.as_ref().unwrap().bytes
        );
    }
    drop(names);
    assert_eq!(budget.used(), payload.bytes() + name_bytes(&payload));
    drop(payload);
    assert_eq!(budget.used(), 0);
}

fn name_bytes(payload: &Payload) -> usize {
    let mut seen = std::collections::HashSet::new();
    let mut bytes = 0;
    for name in payload.iter().filter_map(ProcessRow::executable_name) {
        if seen.insert(std::ptr::from_ref(name)) {
            bytes += 2 * size_of::<usize>()
                + size_of::<ProcessName>()
                + name.bytes.capacity()
                + name.display.capacity();
        }
    }
    bytes
}

pub(super) fn native_sample() -> Box<Sample> {
    use crate::live_apps::{Frame, Publication};
    use collector::ring::{Found, RingReader};

    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
    let mut identities = Vec::with_capacity(2);
    let Found::Publication { publication, .. } = reader
        .read_identity_into(&mut tick, &mut identities)
        .unwrap()
    else {
        panic!("fixture publication absent");
    };
    let mut frame = None;
    Frame::capture(
        &Publication {
            reader: &reader,
            tick: &tick,
            identities: &identities,
            publication,
        },
        &mut frame,
    )
    .unwrap();
    let mut sample = None;
    Sample::capture(frame.as_ref().unwrap(), None, &mut sample).unwrap();
    sample.unwrap()
}

fn native_pair() -> (Arc<ByteBudget>, AppsSecond, AppsSecond) {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let mut first = AppsSecond::default();
    ring.read_prepared(&budget, &operation, None, |_, prepared| {
        first = std::mem::take(&mut prepared.apps);
        true
    })
    .unwrap();
    fixture.publish_with_system_rows(101);
    let mut second = AppsSecond::default();
    ring.read_prepared(&budget, &operation, Some(&first), |_, prepared| {
        second = std::mem::take(&mut prepared.apps);
        true
    })
    .unwrap();
    drop((ring, fixture));
    (budget, first, second)
}
