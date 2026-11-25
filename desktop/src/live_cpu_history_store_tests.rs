use super::tests::{fixture, observation};
use super::*;
use viewer::store::{CpuHistoryIdentity, CpuHistoryObservation, CpuHistoryRow as StoreRow};

#[test]
fn builds_from_the_store_the_history_its_saved_form_decodes_to() {
    // Goal: the page shows the same CPU history whether the Store hands it over as values or the
    // saved minutes write it as JSON.
    // Method: decode the JSON the saved minutes write for one history, then build the page's
    // history from the same values as the Store hands them over.
    let (history, selection) = store_fixture();
    let saved = CPUHistory::decode(history.json(&selection["selected"]), &selection).unwrap();

    let from_store = CPUHistory::from_store(history, &selection).unwrap();

    assert_eq!(from_store, saved);
}

#[test]
fn builds_from_the_store_a_later_page_with_a_gap_that_its_saved_form_decodes_to() {
    // Goal: the page shows the same CPU history from either form beyond the smallest one, with
    // many rows, a page past the first CPUs and an observation without a predecessor.
    // Method: page the second 32 of 64 CPUs over three observations, the middle one without a
    // predecessor, then decode its JSON and build the page's history from its values.
    let (mut history, _) = store_fixture();
    let (_, mut selection) = fixture();
    let selected = observation(3, 1001 * NS + NS / 2);
    selection["selected"] = selected.clone();
    selection["trends"].as_array_mut().unwrap().push(selected);
    history.offset = 32;
    history.total = 64;
    history.selected_index = 2;
    for sample in &mut history.observations {
        sample.cpu_count = 64;
    }
    history.observations[1].previous = None;
    history.observations[1].interval_available = false;
    history.observations.push(CpuHistoryObservation {
        identity: identity(3, 1001 * NS + NS / 2),
        previous: Some(identity(2, 1001 * NS)),
        interval_available: true,
        cpu_count: 64,
    });
    history.cpus = (32..64)
        .map(|cpu| StoreRow {
            cpu,
            busy_percent: vec![None, None, Some(f64::from(cpu))],
        })
        .collect();
    let selection = crate::live_store_fixtures::selection_of_json(&selection);
    let saved = CPUHistory::decode(history.json(&selection["selected"]), &selection).unwrap();

    let from_store = CPUHistory::from_store(history, &selection).unwrap();

    assert_eq!(from_store, saved);
}

#[test]
fn refuses_a_store_measurement_outside_its_observation_domain() {
    // Goal: a core's reading at an observation that counts fewer cores never reaches the plot.
    // Method: measure the second core at an observation of one core, beside a selected
    // observation of two, which every other check accepts.
    let (mut history, _) = store_fixture();
    let (_, mut selection) = fixture();
    let selected = observation(3, 1001 * NS + NS / 2);
    selection["selected"] = selected.clone();
    selection["trends"].as_array_mut().unwrap().push(selected);
    history.selected_index = 2;
    history.total = 2;
    history.observations.push(CpuHistoryObservation {
        identity: identity(3, 1001 * NS + NS / 2),
        previous: None,
        interval_available: false,
        cpu_count: 2,
    });
    history.cpus = vec![
        StoreRow {
            cpu: 0,
            busy_percent: vec![None, Some(0.0), None],
        },
        StoreRow {
            cpu: 1,
            busy_percent: vec![None, Some(5.0), None],
        },
    ];
    let selection = crate::live_store_fixtures::selection_of_json(&selection);

    let result = CPUHistory::from_store(history, &selection);

    assert_eq!(
        result.unwrap_err(),
        "CPU history measurement lies outside its observation domain"
    );
}

macro_rules! refuses_store_history {
    ($name:ident, $history:ident, $refusal:literal, $change:block) => {
        #[test]
        fn $name() {
            // Goal: a fault the Store's values can carry is refused on the page as its JSON is.
            // Method: change one part of an otherwise valid Store history, then build the page's
            // history from its JSON and from its values, and compare the refusals.
            let (mut $history, selection) = store_fixture();
            $change
            let saved =
                CPUHistory::decode($history.json(&selection["selected"]), &selection).unwrap_err();

            let refusal = CPUHistory::from_store($history, &selection).unwrap_err();

            assert_eq!(refusal, saved);
            // The refusal names the fault the row made, and not one an earlier check found.
            assert_eq!(refusal, $refusal);
        }
    };
    ($name:ident, $history:ident, $selection:ident, $refusal:literal, $change:block) => {
        #[test]
        fn $name() {
            // Goal: a fault the Store's values can carry beside a selection is refused on the page
            // as its JSON is.
            // Method: change one part of an otherwise valid Store history and its selection, then
            // build the page's history from its JSON and from its values, and compare the
            // refusals.
            let (mut $history, mut $selection) = store_fixture();
            $change
            let saved = CPUHistory::decode($history.json(&$selection["selected"]), &$selection)
                .unwrap_err();

            let refusal = CPUHistory::from_store($history, &$selection).unwrap_err();

            assert_eq!(refusal, saved);
            // The refusal names the fault the row made, and not one an earlier check found.
            assert_eq!(refusal, $refusal);
        }
    };
    ($name:ident, $history:ident, $json_refusal:literal, $store_refusal:literal, $change:block) => {
        #[test]
        fn $name() {
            // Goal: a selected observation that differs from the selection is refused from both
            // forms, the JSON, which writes the selection's observation beside its list and so
            // differs from itself, and the Store's values, which differ from the selection.
            // Method: change the selected observation of an otherwise valid Store history, then
            // build the page's history from its JSON and from its values.
            let (mut $history, selection) = store_fixture();
            $change
            let saved =
                CPUHistory::decode($history.json(&selection["selected"]), &selection).unwrap_err();

            let refusal = CPUHistory::from_store($history, &selection).unwrap_err();

            assert_eq!((saved.as_str(), refusal.as_str()), ($json_refusal, $store_refusal));
        }
    };
}

macro_rules! refuses_store_history_the_json_cannot_carry {
    ($name:ident, $history:ident, $refusal:literal, $change:block) => {
        #[test]
        fn $name() {
            // Goal: the Store's values can carry this fault and the JSON cannot, and the page
            // refuses it from the values.
            // Method: change one part of an otherwise valid Store history, then decode its JSON
            // and build the page's history from its values.
            let (mut $history, selection) = store_fixture();
            $change
            let saved = CPUHistory::decode($history.json(&selection["selected"]), &selection);

            let result = CPUHistory::from_store($history, &selection);

            assert!(saved.is_ok());
            assert_eq!(result.unwrap_err(), $refusal);
        }
    };
}

refuses_store_history!(
    refuses_an_observation_count_above_the_context_bound,
    history,
    "CPU history exceeds 4096 observations",
    {
        history.observations = (0..4097)
            .map(|_| CpuHistoryObservation {
                identity: identity(1, 1000 * NS),
                previous: None,
                interval_available: false,
                cpu_count: 1,
            })
            .collect();
    }
);
refuses_store_history!(
    refuses_a_cpu_page_above_32_rows,
    history,
    "CPU history exceeds 32 CPU rows",
    {
        history.cpus = (0..33)
            .map(|cpu| StoreRow {
                cpu,
                busy_percent: vec![None, Some(0.0)],
            })
            .collect();
    }
);
refuses_store_history!(
    refuses_a_row_longer_than_its_observation_context,
    history,
    "CPU history row length differs from its context",
    {
        history.cpus[0].busy_percent = vec![None, Some(0.0), Some(1.0)];
    }
);
refuses_store_history_the_json_cannot_carry!(
    refuses_a_selected_index_past_its_observations,
    history,
    "Selected CPU observation is missing from history",
    {
        history.selected_index = 2;
    }
);
refuses_store_history!(
    refuses_an_empty_context,
    history,
    "CPU history context is empty",
    {
        history.observations.clear();
        history.cpus[0].busy_percent.clear();
    }
);
refuses_store_history!(
    refuses_a_selected_identity_of_an_invalid_length,
    history,
    "CPU history identity has an invalid length",
    {
        history.observations[1].identity.run = "0001".into();
    }
);
refuses_store_history!(
    refuses_a_selected_identity_that_is_not_hexadecimal,
    history,
    "CPU history identity is not hexadecimal",
    {
        history.observations[1].identity.run = "z".repeat(32);
    }
);
refuses_store_history_the_json_cannot_carry!(
    refuses_a_selection_of_another_observation,
    history,
    "CPU history publication does not match its context",
    {
        history.selected_index = 0;
    }
);
refuses_store_history!(
    refuses_a_different_context_start,
    history,
    "CPU history has a different context start",
    {
        history.first_ns = 103 * NS;
    }
);
refuses_store_history!(
    refuses_a_different_context_end,
    history,
    "CPU history has a different context end",
    {
        history.end_ns = 1002 * NS - 1;
    }
);
refuses_store_history!(
    refuses_a_context_of_more_than_900_seconds,
    history,
    "CPU history context exceeds 900 seconds",
    {
        history.first_ns = 101 * NS;
    }
);
refuses_store_history!(
    refuses_a_context_that_ends_before_it_starts,
    history,
    "CPU history context ends before it starts",
    {
        history.end_ns = history.first_ns;
    }
);
refuses_store_history!(
    refuses_a_cpu_count_above_the_acquisition_bound,
    history,
    "CPU history domain exceeds 1024",
    {
        history.total = 1025;
    }
);
refuses_store_history!(
    refuses_an_unaligned_cpu_page,
    history,
    "CPU history page is not aligned to 32 CPUs",
    {
        history.offset = 1;
    }
);
refuses_store_history!(
    refuses_an_empty_domain_with_a_nonzero_page,
    history,
    "Empty CPU history has a nonzero page",
    {
        history.total = 0;
        history.offset = 32;
    }
);
refuses_store_history!(
    refuses_an_empty_domain_with_rows,
    history,
    "Empty CPU history contains rows",
    {
        history.total = 0;
    }
);
refuses_store_history!(
    refuses_a_page_beyond_the_cpu_domain,
    history,
    "CPU history page lies beyond its domain",
    {
        history.offset = 32;
    }
);
refuses_store_history!(
    refuses_a_page_with_a_different_row_count,
    history,
    "CPU history page has a different row count",
    {
        history.total = 2;
    }
);
refuses_store_history!(
    refuses_a_context_shorter_than_the_selection,
    history,
    "CPU history context differs from the selected context",
    {
        history.observations.remove(0);
        history.selected_index = 0;
        history.cpus[0].busy_percent = vec![Some(0.0)];
    }
);
refuses_store_history!(
    refuses_an_observation_domain_above_the_acquisition_bound,
    history,
    "CPU history observation domain exceeds 1024",
    {
        history.observations[0].cpu_count = 1025;
    }
);
refuses_store_history!(
    refuses_a_selected_domain_other_than_the_page,
    history,
    "Selected CPU history domain differs from the page",
    {
        history.observations[1].cpu_count = 2;
    }
);
refuses_store_history!(
    refuses_a_predecessor_identity_of_an_invalid_length,
    history,
    "CPU history identity has an invalid length",
    {
        predecessor(&mut history).run = "0001".into();
    }
);
refuses_store_history!(
    refuses_an_available_interval_without_cpus,
    history,
    "Available CPU history interval has no CPUs",
    {
        history.total = 0;
        history.cpus.clear();
        history.observations[1].cpu_count = 0;
    }
);
refuses_store_history!(
    refuses_a_measured_interval_without_a_previous_observation,
    history,
    "Available CPU history interval lacks a predecessor",
    {
        history.observations[1].previous = None;
    }
);
refuses_store_history!(
    refuses_an_interval_crossing_runs,
    history,
    "CPU history interval crosses runs",
    {
        predecessor(&mut history).run = "00000000000000050000000000000006".into();
    }
);
refuses_store_history!(
    refuses_an_interval_crossing_boots,
    history,
    "CPU history interval crosses boots",
    {
        predecessor(&mut history).boot = "00000000000000050000000000000006".into();
    }
);
refuses_store_history!(
    refuses_a_nonconsecutive_interval,
    history,
    "CPU history interval has nonconsecutive publications",
    {
        predecessor(&mut history).publication = 0;
    }
);
refuses_store_history!(
    refuses_a_nonconsecutive_tick_interval,
    history,
    "CPU history interval has nonconsecutive ticks",
    {
        predecessor(&mut history).run_tick = 0;
    }
);
refuses_store_history!(
    refuses_a_reversed_clock_interval,
    history,
    "CPU history interval clock does not advance",
    {
        predecessor(&mut history).wall_ns = 1002 * NS;
    }
);
refuses_store_history!(
    refuses_an_interval_crossing_an_unavailable_second,
    history,
    "CPU history interval crosses an unavailable second",
    {
        predecessor(&mut history).wall_ns = 999 * NS;
    }
);
refuses_store_history!(
    refuses_an_interval_across_different_cpu_domains,
    history,
    "CPU history interval crosses different CPU domains",
    {
        history.observations[0].cpu_count = 0;
    }
);
refuses_store_history!(
    refuses_a_changed_previous_observation,
    history,
    "CPU history interval has a changed predecessor",
    {
        predecessor(&mut history).wall_ns = 1000 * NS + 1;
    }
);
refuses_store_history!(
    refuses_a_row_with_the_wrong_cpu_index,
    history,
    "CPU history rows differ from the requested CPU indices",
    {
        history.cpus[0].cpu = 1;
    }
);
refuses_store_history_the_json_cannot_carry!(
    refuses_a_non_finite_percentage,
    history,
    "CPU history percentage is non-finite",
    {
        history.cpus[0].busy_percent[1] = Some(f64::NAN);
    }
);
refuses_store_history!(
    refuses_a_percentage_just_above_one_fully_busy_cpu,
    history,
    "CPU history percentage lies outside zero to 100",
    {
        history.cpus[0].busy_percent[1] = Some(100.000_000_1);
    }
);
refuses_store_history!(
    refuses_a_measurement_in_an_unavailable_interval,
    history,
    "Unavailable CPU history interval contains a measurement",
    {
        history.cpus[0].busy_percent[0] = Some(0.0);
    }
);
refuses_store_history!(
    refuses_an_observation_outside_the_context,
    history,
    selection,
    "CPU history observation lies outside the context",
    {
        history.observations[0].identity.wall_ns = 1002 * NS;
        selection.trends_edit(|trends| trends[0]["wall_ns"] = json!(1002 * NS));
    }
);
refuses_store_history!(
    refuses_duplicate_identities,
    history,
    selection,
    "CPU history contains duplicate identities",
    {
        history.observations[0].identity = identity(2, 1001 * NS);
        selection.trends_edit(|trends| trends[0] = observation(2, 1001 * NS));
    }
);
refuses_store_history!(
    refuses_unordered_observations,
    history,
    selection,
    "CPU history observations are not ordered",
    {
        history.observations[0].identity.wall_ns = 1001 * NS + 1;
        selection.trends_edit(|trends| trends[0]["wall_ns"] = json!(1001 * NS + 1));
    }
);
#[test]
fn refuses_an_observation_that_differs_from_its_trend() {
    // Goal: a history whose observation differs from the selection's point of the same second is
    // refused from both forms, though the history agrees with itself.
    // Method: give the selection's first point another run tick, then build the page's history
    // from its JSON and from its values.
    let (history, mut selection) = store_fixture();
    selection.trends_edit(|trends| trends[0]["run_tick"] = json!(7));
    let saved = CPUHistory::decode(history.json(&selection["selected"]), &selection).unwrap_err();

    let refusal = CPUHistory::from_store(history, &selection).unwrap_err();

    assert_eq!(
        (saved.as_str(), refusal.as_str()),
        (
            "CPU history run tick does not match its context",
            "CPU history run tick does not match its context"
        )
    );
}
refuses_store_history!(
    refuses_an_unselected_identity_that_is_not_hexadecimal,
    history,
    "CPU history identity is not hexadecimal",
    {
        history.observations[0].identity.boot = "z".repeat(32);
    }
);
refuses_store_history!(
    refuses_a_selected_run_tick_that_differs_from_the_selection,
    history,
    "Selected CPU history observation has changed metadata",
    "CPU history run tick does not match its context",
    {
        // The selected second loses its interval, so the history still agrees with itself.
        history.observations[1].identity.run_tick = 9;
        history.observations[1].previous = None;
        history.observations[1].interval_available = false;
        history.cpus[0].busy_percent[1] = None;
    }
);
refuses_store_history!(
    refuses_a_selected_wall_clock_that_differs_from_the_selection,
    history,
    "Selected CPU history observation has changed metadata",
    "CPU history wall clock does not match its context",
    {
        history.observations[1].identity.wall_ns = 1001 * NS + 5;
    }
);

#[test]
fn refuses_from_a_view_a_history_of_more_than_900_seconds() {
    // Goal: a history the store's View built goes through the validation that holds the
    // reader's history to itself, though no selection holds it to a context.
    // Method: start an otherwise valid Store history's window a second early, then build the
    // page's history from it as the View hands it over.
    let (mut history, _) = store_fixture();
    history.first_ns = 101 * NS;

    let refusal = CPUHistory::from_view(history).unwrap_err();

    assert_eq!(refusal, "CPU history context exceeds 900 seconds");
}

/// The Store's values of the history `fixture` writes as JSON, with that fixture's selection.
fn store_fixture() -> (viewer::store::CpuHistory, viewer::Selection) {
    let (_, selection) = fixture();
    let history = viewer::store::CpuHistory {
        selected_index: 1,
        first_ns: 102 * NS,
        end_ns: 1002 * NS,
        offset: 0,
        total: 1,
        retention_limited: false,
        observations: vec![
            CpuHistoryObservation {
                identity: identity(1, 1000 * NS),
                previous: None,
                interval_available: false,
                cpu_count: 1,
            },
            CpuHistoryObservation {
                identity: identity(2, 1001 * NS),
                previous: Some(identity(1, 1000 * NS)),
                interval_available: true,
                cpu_count: 1,
            },
        ],
        cpus: vec![StoreRow {
            cpu: 0,
            busy_percent: vec![None, Some(0.0)],
        }],
    };

    (
        history,
        crate::live_store_fixtures::selection_of_json(&selection),
    )
}

fn identity(publication: u64, wall_ns: u64) -> CpuHistoryIdentity {
    CpuHistoryIdentity {
        run: "00000000000000010000000000000002".into(),
        boot: "00000000000000030000000000000004".into(),
        publication,
        run_tick: publication,
        wall_ns,
    }
}

/// The predecessor of the selected observation of `store_fixture`'s history.
fn predecessor(history: &mut viewer::store::CpuHistory) -> &mut CpuHistoryIdentity {
    history.observations[1].previous.as_mut().unwrap()
}
