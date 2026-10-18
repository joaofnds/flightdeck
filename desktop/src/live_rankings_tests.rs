use serde_json::{Value, json};

use super::*;

#[test]
fn rankings_include_the_largest_apps_beyond_the_first_page() {
    // Stream two App pages and require the largest exact totals from the second page.
    let source = Pages::new(600);

    let result = read(&selected(), |offset| Ok(source.page(offset)));

    let Rankings::Ready(result) = result else {
        panic!("{result:?}")
    };
    assert_eq!(result.app_count, 600);
    assert_eq!(result.metrics[0].entries.len(), 10);
    assert_eq!(result.metrics[0].entries[0].amount, 599);
    assert_eq!(result.metrics[0].entries[9].amount, 590);
    assert_eq!(result.metrics[1].entries[0].amount, 599);
    assert_eq!(result.metrics[2].entries[0].amount, 599);
}

#[test]
fn ranking_ties_use_captured_app_keys_instead_of_names_or_page_order() {
    // Keep two equal totals deterministic even when their display names disagree with keys.
    let mut first = app(7);
    first["key"]["bytes_hex"] = json!("02");
    first["display_name"] = json!("A");
    let mut second = first.clone();
    second["key"]["bytes_hex"] = json!("01");
    second["display_name"] = json!("Z");

    let result = read_one(&[first, second]);

    assert_eq!(result.metrics[0].entries[0].app.name, "Z");
    assert_eq!(result.metrics[0].entries[1].app.name, "A");
}

#[test]
fn ranking_order_preserves_integer_differences_above_float_precision() {
    // Equal displayed rates must still rank by the exact captured interval totals.
    let mut first = app(1);
    first["cpu_ns"]["known"] = json!("9007199254740992");
    let mut second = app(2);
    second["cpu_ns"]["known"] = json!("9007199254740993");
    second["cpu_percent_one_core"] = first["cpu_percent_one_core"].clone();

    let result = read_one(&[first, second]);

    assert_eq!(result.metrics[0].entries[0].amount, 9_007_199_254_740_993);
    assert_eq!(result.metrics[0].entries[1].amount, 9_007_199_254_740_992);
}

#[test]
fn measured_zero_partial_missing_and_overflow_totals_stay_distinct() {
    // Each metric keeps its own coverage and excludes only unreadable values.
    let zero = app(0);
    let mut overflow = app(1);
    overflow["cpu_ns"]["overflowed"] = json!(true);
    let mut missing = app(2);
    missing["cpu_ns"]["measured_members"] = json!(0);
    let mut partial = app(3);
    partial["cpu_ns"]["unavailable_members"] = json!(2);

    let result = read_one(&[zero, overflow, missing, partial]);

    let cpu = &result.metrics[0];
    assert_eq!(cpu.unavailable, 1);
    assert_eq!(cpu.overflowed, 1);
    assert_eq!(cpu.partial, 1);
    assert_eq!(cpu.entries.len(), 2);
    assert_eq!(cpu.entries[0].amount, 3);
    assert_eq!(cpu.entries[0].unavailable_members, 2);
    assert_eq!(cpu.entries[1].amount, 0);
    assert_eq!(cpu.entries[1].rate, Some(0.0));
    assert_eq!(result.metrics[1].entries.len(), 4);
}

#[test]
fn an_unavailable_rate_cannot_be_ranked_as_a_measured_zero() {
    // A first observation has measured counters but no elapsed interval for CPU or watts.
    let mut first = app(0);
    first["cpu_percent_one_core"] = Value::Null;
    first["energy_watts"] = Value::Null;

    let result = read_one(&[first]);

    assert!(result.metrics[0].entries.is_empty());
    assert_eq!(result.metrics[0].unavailable, 1);
    assert!(result.metrics[2].entries.is_empty());
    assert_eq!(result.metrics[1].entries[0].amount, 0);
}

#[test]
fn a_later_page_with_a_different_same_second_publication_invalidates_all_rankings() {
    // A page boundary must not combine two observation identities into one ranking.
    let source = Pages::new(600);

    let result = read(&selected(), |offset| {
        let mut page = source.page(offset);
        if offset > 0 {
            page["selected"]["publication"] = json!(3);
        }
        Ok(page)
    });

    assert!(matches!(result, Rankings::Unavailable(_)));
}

#[test]
fn a_cancelled_page_read_stops_before_any_more_pages_are_requested() {
    // Inject the same boundary error a superseded reader receives between pages.
    let source = Pages::new(24_000);
    let mut offsets = Vec::new();

    let result = read(&selected(), |offset| {
        offsets.push(offset);
        if offset == 512 {
            return Err("cancelled".into());
        }
        Ok(source.page(offset))
    });

    assert!(matches!(result, Rankings::Unavailable(_)));
    assert_eq!(offsets, [0, 512]);
}

#[test]
fn the_full_source_bound_reads_forty_seven_pages_and_retains_only_thirty_entries() {
    // Measure the bounded synthetic JSON pager plus fold, without native UI or acquisition.
    let source = Pages::new(24_000);
    let mut calls = 0;

    let (result, allocations) = crate::allocation_tests::measure(|| {
        read(&selected(), |offset| {
            calls += 1;
            Ok(source.page(offset))
        })
    });

    let Rankings::Ready(result) = result else {
        panic!("{result:?}")
    };
    assert_eq!(calls, 47);
    assert_eq!(
        result
            .metrics
            .iter()
            .map(|metric| metric.entries.len())
            .sum::<usize>(),
        30
    );
    assert_eq!(result.metrics[0].entries[0].amount, 23_999);
    eprintln!(
        "ranking synthetic pager+fold: pages={calls}, allocation_calls={}, bytes={}",
        allocations.calls, allocations.bytes
    );
}

#[test]
fn a_source_total_above_its_bound_is_rejected_after_one_page() {
    // Oversized metadata cannot turn the page loop into unbounded work.
    let mut calls = 0;

    let result = read(&selected(), |offset| {
        calls += 1;
        Ok(Pages::new(24_001).page(offset))
    });

    assert!(matches!(result, Rankings::Unavailable(_)));
    assert_eq!(calls, 1);
}

#[test]
fn a_cyclic_next_offset_is_rejected_after_one_page() {
    // A malformed continuation cannot duplicate rows or consume the whole operation bound.
    let mut page = Pages::new(600).page(0);
    page["page"]["next_offset"] = json!(0);
    let mut calls = 0;

    let result = read(&selected(), |_| {
        calls += 1;
        Ok(page.clone())
    });

    assert!(matches!(result, Rankings::Unavailable(_)));
    assert_eq!(calls, 1);
}

#[test]
fn a_source_error_retains_only_a_bounded_notice() {
    // Keep a large external error from enlarging the retained ranking failure payload.
    let result = read(&selected(), |_| Err("失".repeat(4096)));

    let Rankings::Unavailable(error) = result else {
        panic!("{result:?}")
    };
    assert_eq!(error.chars().count(), 513);
    assert!(error.ends_with('…'));
}

#[test]
fn partial_membership_keeps_known_rankings_and_ungrouped_counts() {
    // A source with unknown program identities still exposes its explicitly partial ranks.
    let mut page = Pages::new(2).page(0);
    page["membership_complete"] = json!(false);
    page["ungrouped_members"] = json!(7);

    let result = read(&selected(), |_| Ok(page.clone()));

    let Rankings::Ready(result) = result else {
        panic!("{result:?}")
    };
    assert!(!result.membership_complete);
    assert_eq!(result.ungrouped_members, 7);
    assert_eq!(result.metrics[0].entries[0].amount, 1);
}

fn read_one(apps: &[Value]) -> TopApps {
    let count = apps.len();
    let page = json!({"selected":selected(),"membership_complete":true,
        "ungrouped_members":0,"app_count":count,"apps":apps,
        "page":{"offset":0,"limit":512,"total":count,"next_offset":null}});
    match read(&selected(), |_| Ok(page.clone())) {
        Rankings::Ready(result) => result,
        Rankings::Unavailable(error) => panic!("{error}"),
    }
}

struct Pages {
    count: u32,
}

impl Pages {
    fn new(count: u32) -> Self {
        Self { count }
    }

    fn page(&self, offset: u32) -> Value {
        let end = (offset + 512).min(self.count);
        let apps: Vec<_> = (offset..end).map(app).collect();
        json!({"selected":selected(),"membership_complete":true,
            "ungrouped_members":0,"app_count":self.count,"apps":apps,
            "page":{"offset":offset,"limit":512,"total":self.count,
                "next_offset":if end < self.count {Some(end)} else {None}}})
    }
}

fn selected() -> Value {
    json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004","publication":2})
}

fn app(amount: u32) -> Value {
    let measure = json!({"known":amount.to_string(),"measured_members":1,
        "unavailable_members":0,"overflowed":false});
    json!({"display_name":format!("App{amount:05}"),
        "key":{"kind":"executable","bytes_hex":format!("{amount:08x}")},
        "members":1,"membership_complete":true,
        "cpu_ns":measure,"resident_bytes":measure,"energy_nj":measure,
        "cpu_percent_one_core":f64::from(amount),"energy_watts":f64::from(amount)})
}
