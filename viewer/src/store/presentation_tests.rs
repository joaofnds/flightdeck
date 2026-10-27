use std::sync::Arc;

use persistence::Error;
use serde_json::Value;

use super::{AppsSecond, Missing, TickApps};
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::{Detail, Page, Sample};
use crate::store::ByteBudget;

#[test]
fn borrowed_pages_preserve_native_app_and_process_json_after_sources_are_gone() {
    // Goal: retained facts preserve populated and refused member pages without a source.
    // Method: capture real native samples, freeze legacy JSON, then retire the ring.
    let (second, expected) = native_second(true, 514);

    let shown = pages(&second);

    assert_eq!(shown, expected);
    assert_eq!(shown[3]["processes"].as_array().unwrap().len(), 512);
    assert_eq!(shown[4]["processes"].as_array().unwrap().len(), 1);
    assert_eq!(shown[3]["processes"][0]["cpu_ns"]["unavailable"], "refused");
}

#[test]
fn borrowed_pages_preserve_ungrouped_processes_without_inventing_apps() {
    // Goal: unsupported program identities remain ungrouped and unavailable.
    // Method: compare real legacy-ring JSON after discarding its source and Sample.
    let (second, expected) = native_second(false, 2);

    let shown = pages(&second);

    assert_eq!(shown, expected);
    assert_eq!(shown[1]["page"]["total"], 1);
    assert_eq!(shown[1]["processes"][0]["app_index"], Value::Null);
}

#[test]
fn borrowed_pages_preserve_gone_processes_and_unverified_exit_state() {
    // Goal: a process that exits without usage remains present with its exact absence reason.
    // Method: freeze legacy JSON across real Start/Exit events, then retire the native source.
    let mut fixture = Fixture::new();
    fixture.publish_with_events(
        100,
        &[collector::tracker::Event::Start {
            pid: 9,
            coalition: None,
        }],
    );
    fixture.publish_with_events(
        101,
        &[collector::tracker::Event::Exit {
            pid: 9,
            last: Err(collector::processes::Unreadable::Gone),
            noticed: collector::tracker::Noticed::OnExit,
        }],
    );
    let (second, expected) = retained_fixture(&fixture);
    drop(fixture);

    let shown = pages(&second);

    assert_eq!(shown, expected);
    assert_eq!(shown[4]["processes"][0]["pid"], 9);
    assert_eq!(shown[4]["processes"][0]["state"], "exited_unverified");
    assert_eq!(
        shown[4]["processes"][0]["cpu_ns"]["unavailable"],
        "exit_unverified"
    );
}

#[test]
fn borrowed_pages_preserve_the_same_json_from_native_and_saved_samples() {
    // Goal: saved and native retained payloads obey the same complete presentation contract.
    // Method: seal the real native fixture, capture saved Samples, then retire both sources.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let (native, expected) = retained_fixture(&fixture);
    fixture.seal_minute(0);
    let marker = persistence::Store::open(&fixture.root)
        .unwrap()
        .marker_path(&persistence::WindowIdentity {
            run: fixture.writer.identity().unwrap(),
            window: 0,
        });
    let minute = crate::recording::SealedMinute::open(&fixture.root, &marker).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut saved = AppsSecond::with_room();
    let mut before = None;
    let mut legacy_saved = Vec::new();
    minute
        .visit_samples(&mut before, |_, sample| {
            let sample = sample.unwrap();
            legacy_saved = sample_pages(sample);
            saved.take(TickApps::Derived(sample), None, Some(&budget));
            Ok(())
        })
        .unwrap();
    drop(before);
    drop(minute);
    drop(fixture);

    let shown = pages(&saved);

    assert_eq!(shown, expected);
    assert_eq!(shown, legacy_saved);
    assert_eq!(shown, pages(&native));
}

#[test]
fn missing_payload_is_an_error_even_when_app_totals_are_present() {
    // Goal: a payload budget refusal cannot masquerade as an empty complete process list.
    // Method: remove only the payload from a populated retained second.
    let (mut second, _) = native_second(true, 2);
    second.processes = Err(Missing::Budget);

    let result = second.apps_page(page(0));

    assert!(matches!(result, Err(Error::Bound("live App empty sample"))));
}

#[test]
fn a_changed_observation_cannot_relabel_retained_processes() {
    // Goal: complete payload rows remain associated with the exact captured observation.
    // Method: change the surrounding second's publication while keeping its payload.
    let (mut second, _) = native_second(true, 2);
    second.observation.as_mut().unwrap().stamp.publication += 1;

    let result = second.apps_page(page(0));

    assert!(matches!(result, Err(Error::Bound("live App stale detail"))));
}

#[test]
fn a_stale_detail_request_cannot_select_the_same_index_in_another_observation() {
    // Goal: an App index is meaningful only within its requested exact observation.
    // Method: request a valid retained App index under the preceding publication.
    let (second, _) = native_second(true, 2);
    let mut selected = second.observation.unwrap();
    selected.stamp.publication -= 1;

    let result = second.app_detail(&Detail {
        selected: &selected,
        app_index: 0,
        page: page(0),
    });

    assert!(matches!(result, Err(Error::Bound("live App stale detail"))));
}

#[test]
fn an_unknown_app_index_is_an_error() {
    // Goal: App detail never substitutes a different App for an index outside the observation.
    // Method: request the exact end of a real two-App observation.
    let (second, _) = native_second(true, 2);

    let result = second.app_detail(&Detail {
        selected: second.observation.as_ref().unwrap(),
        app_index: 2,
        page: page(0),
    });

    assert!(matches!(result, Err(Error::Bound("live App index"))));
}

#[test]
fn the_exact_end_of_apps_and_members_is_a_complete_empty_page() {
    // Goal: exact-end paging terminates with the same totals and absent continuation as Sample.
    // Method: read the end of two Apps and the end of the one-member first App.
    let (second, _) = native_second(true, 2);

    let apps = second.apps_page(page(2)).unwrap();
    let members = second
        .app_detail(&Detail {
            selected: second.observation.as_ref().unwrap(),
            app_index: 0,
            page: page(1),
        })
        .unwrap();

    assert_eq!(apps["apps"], serde_json::json!([]));
    assert_eq!(apps["page"]["next_offset"], Value::Null);
    assert_eq!(members["processes"], serde_json::json!([]));
    assert_eq!(members["page"]["next_offset"], Value::Null);
}

#[test]
fn incomplete_app_details_cannot_be_presented_as_complete() {
    // Goal: complete process payloads cannot accompany a truncated outer App list.
    // Method: remove the captured second's App rows while retaining their detail measures.
    let (mut second, _) = native_second(true, 2);
    second.apps.clear();

    let result = second.apps_page(page(0));

    assert!(matches!(result, Err(Error::Bound("live App index"))));
}

#[test]
fn detached_contexts_share_complete_payloads_without_retaining_a_store_pool_slot() {
    // Goal: pinned App queries outlive their pool slot without duplicating process/name rows.
    // Method: detach a real captured second, compare payload ownership, then drop the source.
    let (second, expected) = native_second(true, 2);
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let detached = second.detached(&budget).unwrap();
    assert!(Arc::ptr_eq(
        second.processes.as_ref().unwrap(),
        detached.processes.as_ref().unwrap(),
    ));
    drop(second);

    let shown = pages(&detached);

    assert_eq!(shown, expected);
    assert!(budget.used() > 0);
    drop(detached);
    assert_eq!(budget.used(), 0);
}

#[test]
fn detachment_refuses_its_own_retained_byte_budget_before_copying_apps() {
    // Goal: a pinned context cannot retain unreserved App metadata or names.
    // Method: detach a real populated second under a one-byte budget.
    let (second, _) = native_second(true, 2);
    let budget = Arc::new(ByteBudget::new(1));

    let result = second.detached(&budget);

    assert!(matches!(result, Err(Missing::Budget)));
    assert_eq!(budget.used(), 0);
}

#[test]
fn detached_missing_processes_preserve_independent_compact_app_facts() {
    // Goal: a missing process facet cannot erase otherwise retained App metadata on pinning.
    // Method: detach an explicit process refusal and compare rows, reason and page refusal.
    let (mut second, _) = native_second(true, 2);
    second.processes = Err(Missing::Refused(String::from("fixture payload refusal")));
    let budget = Arc::new(ByteBudget::new(1024 * 1024));

    let detached = second.detached(&budget).unwrap();

    assert_eq!(detached.apps, second.apps);
    assert_eq!(detached.processes, second.processes);
    assert!(matches!(
        detached.apps_page(page(0)),
        Err(Error::Bound("live App empty sample"))
    ));
    assert_eq!(budget.used(), detached.bytes());
    drop(detached);
    assert_eq!(budget.used(), 0);
}

macro_rules! rejected_page {
    ($name:ident, $offset:expr, $limit:expr, $error:literal) => {
        #[test]
        fn $name() {
            // Goal: retained page admission keeps the Sample API's exact bounds.
            // Method: request one invalid Apps page from real captured facts.
            let (second, _) = native_second(true, 2);

            let result = second.apps_page(Page {
                offset: $offset,
                limit: $limit,
            });

            assert!(matches!(result, Err(Error::Bound($error))));
        }
    };
}

rejected_page!(rejects_zero_page_limit, 0, 0, "live App page limit");
rejected_page!(rejects_more_than_512_rows, 0, 513, "live App page limit");
rejected_page!(
    rejects_offset_after_last_app,
    3,
    512,
    "live App page offset"
);

fn native_second(identity: bool, count: u64) -> (AppsSecond, Vec<Value>) {
    let mut fixture = Fixture::new_mode(identity, count, None);
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let result = retained_fixture(&fixture);
    drop(fixture);

    result
}

fn retained_fixture(fixture: &Fixture) -> (AppsSecond, Vec<Value>) {
    let mut ring = crate::store::ring::Ring::new(fixture.root.clone());
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut second = AppsSecond::with_room();
    let mut expected = Vec::new();
    ring.follow().unwrap();
    ring.read(|_, _, apps| {
        let TickApps::Derived(sample) = apps else {
            panic!("the native fixture supplies derived App samples");
        };
        expected = sample_pages(sample);
        second.take(TickApps::Derived(sample), None, Some(&budget));
        true
    })
    .unwrap();
    drop(ring);

    (second, expected)
}

fn sample_pages(sample: &Sample) -> Vec<Value> {
    let mut pages = vec![sample.apps_page(page(0)).unwrap()];
    pages.push(sample.ungrouped_page(page(0)).unwrap());
    for index in 0..sample.apps().len() {
        let app_index = u32::try_from(index).unwrap();
        let count = sample
            .processes()
            .iter()
            .filter(|process| process.app_index == Some(app_index))
            .count();
        for offset in (0..count).step_by(512) {
            pages.push(
                sample
                    .app_detail(&Detail {
                        selected: &sample.selected,
                        app_index,
                        page: page(u32::try_from(offset).unwrap()),
                    })
                    .unwrap(),
            );
        }
    }
    for index in 0..sample.apps().len() {
        pages.push(
            sample
                .apps_page(Page {
                    offset: u32::try_from(index).unwrap(),
                    limit: 1,
                })
                .unwrap(),
        );
    }

    pages
}

fn pages(second: &AppsSecond) -> Vec<Value> {
    let mut pages = vec![second.apps_page(page(0)).unwrap()];
    pages.push(second.ungrouped_page(page(0)).unwrap());
    let selected = second.observation.as_ref().unwrap();
    for index in 0..second.apps.len() {
        let app_index = u32::try_from(index).unwrap();
        let count = second
            .processes
            .as_ref()
            .unwrap()
            .iter()
            .filter(|process| process.app_index == Some(app_index))
            .count();
        for offset in (0..count).step_by(512) {
            pages.push(
                second
                    .app_detail(&Detail {
                        selected,
                        app_index,
                        page: page(u32::try_from(offset).unwrap()),
                    })
                    .unwrap(),
            );
        }
    }
    for index in 0..second.apps.len() {
        pages.push(
            second
                .apps_page(Page {
                    offset: u32::try_from(index).unwrap(),
                    limit: 1,
                })
                .unwrap(),
        );
    }

    pages
}

fn page(offset: u32) -> Page {
    Page { offset, limit: 512 }
}
