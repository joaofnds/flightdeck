use super::*;
use serde_json::json;

#[test]
fn decodes_a_selected_history_without_filling_missing_activity() {
    // Goal: absent intervals and measured idle remain distinguishable.
    // Method: decode two bounded observations with null followed by measured zero.
    let (payload, selection) = fixture();

    let history = CPUHistory::decode(payload, &selection).unwrap();

    assert_eq!(history.wire.cpus[0].busy_percent, vec![None, Some(0.0)]);
    assert_eq!(history.total(), 1);
    assert_eq!(history.wire.observations.len(), 2);
}

#[test]
fn retains_an_unavailable_cpu_domain_without_inventing_an_idle_cpu() {
    // Goal: an empty acquired domain is distinct from one measured idle logical CPU.
    // Method: decode a bounded context with no CPU acquisition at either observation.
    let (mut payload, selection) = fixture();
    payload["total"] = json!(0);
    payload["cpus"] = json!([]);
    payload["observations"][0]["cpu_count"] = json!(0);
    payload["observations"][1]["cpu_count"] = json!(0);
    payload["observations"][1]["interval_available"] = json!(false);

    let history = CPUHistory::decode(payload, &selection).unwrap();

    assert!(history.wire.cpus.is_empty());
    assert_eq!(history.selected(), None);
    assert_eq!(
        history.step(
            None,
            CPUStep {
                observations: 1,
                rows: 0
            }
        ),
        None
    );
    assert_eq!(history.inspect(0, 0.5), None);
}

#[test]
fn rejects_a_selected_identity_with_different_context_clock_metadata() {
    // Goal: an exact identity cannot smuggle a changed clock through the context.
    // Method: change both context copies while keeping the selected observation unchanged.
    let (mut payload, mut selection) = fixture();
    payload["observations"][1]["observation"]["wall_ns"] = json!(1001 * NS + 1);
    selection["trends"][1]["wall_ns"] = json!(1001 * NS + 1);

    let result = CPUHistory::decode(payload, &selection);

    assert!(result.is_err());
}

#[test]
fn hits_the_actual_predecessor_interval_and_selects_the_exact_publication() {
    // Goal: a cell targets its observation instead of rounding to a neighboring second.
    // Method: hit the measured interval and the unmeasured second immediately after it.
    let (payload, selection) = fixture();
    let history = CPUHistory::decode(payload, &selection).unwrap();
    let inside = history.fraction(1000 * NS + NS / 2);
    let gap = history.fraction(1001 * NS + NS / 2);

    let inspection = history.inspect(0, inside).unwrap();

    assert_eq!(
        history.target(inspection),
        Some((
            1001,
            json!({
        "run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004","publication":2})
        ))
    );
    assert_eq!(history.value(inspection), Some(0.0));
    assert_eq!(history.inspect(0, gap), None);
}

#[test]
fn later_intervals_own_overlapping_wall_time_even_when_their_row_is_missing() {
    // Goal: an overlap cannot expose the older source's measured row through a newer null.
    // Method: add a second run whose measured interval overlaps the first run's interval.
    let (mut payload, mut selection) = fixture();
    let mut previous = observation(1, 1000 * NS + NS / 2);
    previous["run"] = json!("00000000000000050000000000000006");
    let mut selected = observation(2, 1001 * NS + NS / 2);
    selected["run"] = previous["run"].clone();
    payload["observation"] = selected.clone();
    payload["observations"].as_array_mut().unwrap().push(json!({
        "observation":selected,"previous_observation":previous,
        "interval_available":true,"cpu_count":1}));
    payload["cpus"][0]["busy_percent"]
        .as_array_mut()
        .unwrap()
        .push(Value::Null);
    selection["selected"] = selected.clone();
    selection["trends"].as_array_mut().unwrap().push(selected);
    let history = CPUHistory::decode(payload, &selection).unwrap();

    let older = history
        .inspect(0, history.fraction(1000 * NS + NS / 4))
        .unwrap();
    let newer = history
        .inspect(0, history.fraction(1000 * NS + NS * 3 / 4))
        .unwrap();

    assert_eq!(history.value(older), Some(0.0));
    assert_eq!(history.value(newer), None);
    assert_eq!(
        history.target(newer).unwrap().1["run"],
        "00000000000000050000000000000006"
    );
}

#[test]
fn pages_a_partial_cpu_domain_without_relabeling_its_rows() {
    // Goal: a later page retains its scheduler indices and selected identity.
    // Method: decode the final three rows of a 35-CPU domain.
    let (mut payload, selection) = fixture();
    payload["offset"] = json!(32);
    payload["total"] = json!(35);
    payload["observations"][0]["cpu_count"] = json!(35);
    payload["observations"][1]["cpu_count"] = json!(35);
    payload["cpus"] = json!(
        (32..35)
            .map(|cpu| json!({"cpu":cpu,"busy_percent":[null,f64::from(cpu)]}))
            .collect::<Vec<_>>()
    );

    let history = CPUHistory::decode(payload, &selection).unwrap();

    assert_eq!(
        history
            .wire
            .cpus
            .iter()
            .map(|cpu| cpu.cpu)
            .collect::<Vec<_>>(),
        vec![32, 33, 34]
    );
    assert_eq!(history.value(history.selected().unwrap()), Some(32.0));
}

#[test]
fn keyboard_inspection_preserves_exact_observations_and_clamps_at_the_context_boundaries() {
    // Goal: arrows navigate observations and CPU rows without changing recording selection.
    // Method: step from the selected cell to the first observation, then clamp one more left.
    let (payload, selection) = fixture();
    let history = CPUHistory::decode(payload, &selection).unwrap();

    let first = history.step(
        None,
        CPUStep {
            observations: -1,
            rows: 0,
        },
    );
    let clamped = history.step(
        first,
        CPUStep {
            observations: -1,
            rows: 0,
        },
    );

    assert_eq!(
        first,
        Some(CPUInspection::Sample {
            row: 0,
            observation_index: 0
        })
    );
    assert_eq!(clamped, first);
    assert_eq!(history.target(first.unwrap()).unwrap().0, 1000);
    assert_eq!(history.target(history.selected().unwrap()).unwrap().0, 1001);
}

#[test]
fn decodes_the_maximum_cpu_page_with_bounded_retained_memory_and_geometry() {
    // Goal: the full protocol bound remains affordable without shortening its history window.
    // Method: decode 32 rows of 4096 observations, measure owned storage and hit its last interval.
    let (payload, selection) = maximum_fixture();
    let started = std::time::Instant::now();

    let history = CPUHistory::decode(payload, &selection).unwrap();

    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let retained_bytes = retained_bytes(&history);
    println!(
        "{}",
        json!({"phase":"gpui_maximum_cpu_history_decode",
        "observations":history.wire.observations.len(),"page_cpus":history.wire.cpus.len(),
        "values":history.wire.cpus.iter().map(|cpu|cpu.busy_percent.len()).sum::<usize>(),
        "intervals":history.wire.intervals.len(),"retained_owned_bytes":retained_bytes,
        "decode_ms":elapsed_ms,"timing_scope":"debug decode, validation and geometry only"})
    );
    assert_eq!(history.wire.cpus.len(), 32);
    assert_eq!(history.wire.observations.len(), 4096);
    assert!(history.wire.intervals.len() <= 4096);
    assert!(retained_bytes < 4 * 1024 * 1024);
    let last = history
        .wire
        .observations
        .last()
        .unwrap()
        .observation
        .wall_ns;
    let inspection = history.inspect(31, history.fraction(last - 1)).unwrap();
    assert_eq!(history.target(inspection).unwrap().1["publication"], 4097);
    assert_eq!(history.value(inspection), Some(33.333_333_333_333_336));
}

fn maximum_fixture() -> (Value, Value) {
    let mut samples = Vec::with_capacity(4096);
    let mut trends = Vec::with_capacity(4096);
    for index in 0..4096_u64 {
        let current = observation(index + 2, 1000 * NS + index * 100_000_000);
        let previous = observation(index + 1, 1000 * NS + index * 100_000_000 - 100_000_000);
        samples.push(
            json!({"observation":current,"previous_observation":previous,
            "interval_available":true,"cpu_count":1024}),
        );
        trends.push(current);
    }
    let selected = trends.last().unwrap().clone();
    let second = selected["wall_ns"].as_u64().unwrap() / NS;
    let rows = (992..1024)
        .map(|cpu| {
            json!({"cpu":cpu,
        "busy_percent":vec![Some(33.333_333_333_333_336);4096]})
        })
        .collect::<Vec<_>>();
    let payload = json!({"observation":selected,"context_seconds":900,
        "first_ns":second.saturating_sub(899) * NS,"end_ns":(second+1)*NS,
        "offset":992,"total":1024,"retention_limited":false,
        "observations":samples,"cpus":rows});
    let selection = json!({"selected":selected,"second":second,"context_seconds":900,
        "trends":trends});

    (payload, selection)
}

fn retained_bytes(history: &CPUHistory) -> usize {
    size_of::<CPUHistory>()
        + size_of::<CPUHistoryWire>()
        + history.wire.observations.capacity() * size_of::<CPUSample>()
        + history
            .wire
            .observations
            .iter()
            .map(|sample| {
                sample.observation.run.capacity()
                    + sample.observation.boot.capacity()
                    + sample.previous_observation.as_ref().map_or(0, |previous| {
                        previous.run.capacity() + previous.boot.capacity()
                    })
            })
            .sum::<usize>()
        + history.wire.cpus.capacity() * size_of::<CPUHistoryRow>()
        + history
            .wire
            .cpus
            .iter()
            .map(|row| row.busy_percent.capacity() * size_of::<Option<f64>>())
            .sum::<usize>()
        + history.wire.intervals.capacity() * size_of::<CPUInterval>()
        + history.wire.observation.run.capacity()
        + history.wire.observation.boot.capacity()
}

macro_rules! rejects_payload {
    ($name:ident, $payload:ident, $selection:ident, $change:block) => {
        #[test]
        fn $name() {
            // Goal: inconsistent acquired history cannot reach the plot as a plausible measurement.
            // Method: change one independent part of an otherwise valid source response.
            let (mut $payload, $selection) = fixture();
            $change

            let result = CPUHistory::decode($payload, &$selection);

            assert!(result.is_err());
        }
    };
}

rejects_payload!(
    rejects_a_context_with_a_different_selected_publication,
    payload,
    selection,
    {
        payload["observation"]["publication"] = json!(3);
    }
);
rejects_payload!(
    rejects_a_context_with_a_different_clock_extent,
    payload,
    selection,
    {
        payload["first_ns"] = json!(101 * NS);
    }
);
rejects_payload!(rejects_an_unaligned_cpu_page, payload, selection, {
    payload["offset"] = json!(1);
});
rejects_payload!(
    rejects_a_page_beyond_the_selected_cpu_domain,
    payload,
    selection,
    {
        payload["offset"] = json!(32);
    }
);
rejects_payload!(
    rejects_a_row_with_the_wrong_cpu_index,
    payload,
    selection,
    {
        payload["cpus"][0]["cpu"] = json!(1);
    }
);
rejects_payload!(
    rejects_a_history_row_longer_than_its_observation_context,
    payload,
    selection,
    {
        payload["cpus"][0]["busy_percent"] = json!([null, 0.0, 1.0]);
    }
);
rejects_payload!(
    rejects_a_cpu_count_above_the_acquisition_bound,
    payload,
    selection,
    {
        payload["total"] = json!(1025);
    }
);
rejects_payload!(
    rejects_a_percentage_just_above_one_fully_busy_cpu,
    payload,
    selection,
    {
        payload["cpus"][0]["busy_percent"][1] = json!(100.000_000_1);
    }
);
rejects_payload!(
    rejects_a_measurement_in_an_unavailable_interval,
    payload,
    selection,
    {
        payload["cpus"][0]["busy_percent"][0] = json!(0.0);
    }
);
rejects_payload!(
    rejects_a_measured_interval_without_a_previous_observation,
    payload,
    selection,
    {
        payload["observations"][1]["previous_observation"] = Value::Null;
    }
);
rejects_payload!(rejects_an_interval_crossing_runs, payload, selection, {
    payload["observations"][1]["previous_observation"]["run"] =
        json!("00000000000000050000000000000006");
});
rejects_payload!(rejects_an_interval_crossing_boots, payload, selection, {
    payload["observations"][1]["previous_observation"]["boot"] =
        json!("00000000000000050000000000000006");
});
rejects_payload!(rejects_a_nonconsecutive_interval, payload, selection, {
    payload["observations"][1]["previous_observation"]["publication"] = json!(0);
});
rejects_payload!(
    rejects_a_nonconsecutive_tick_interval,
    payload,
    selection,
    {
        payload["observations"][1]["previous_observation"]["run_tick"] = json!(0);
    }
);
rejects_payload!(rejects_a_reversed_clock_interval, payload, selection, {
    payload["observations"][1]["previous_observation"]["wall_ns"] = json!(1002 * NS);
});
rejects_payload!(
    rejects_an_interval_crossing_an_unavailable_second,
    payload,
    selection,
    {
        payload["observations"][1]["previous_observation"]["wall_ns"] = json!(999 * NS);
    }
);
rejects_payload!(
    rejects_a_measurement_outside_its_earlier_cpu_domain,
    payload,
    selection,
    {
        payload["observations"][0]["cpu_count"] = json!(0);
    }
);
rejects_payload!(
    rejects_a_changed_previous_observation,
    payload,
    selection,
    {
        payload["observations"][1]["previous_observation"]["wall_ns"] = json!(1000 * NS + 1);
    }
);
rejects_payload!(
    rejects_an_empty_restoration_diagnostic,
    payload,
    selection,
    {
        payload["restoration_error"] = json!("");
    }
);

rejects_payload!(
    rejects_an_observation_count_above_the_context_bound,
    payload,
    selection,
    {
        payload["observations"] = json!(vec![payload["observations"][0].clone(); 4097]);
    }
);
rejects_payload!(rejects_a_cpu_page_above_32_rows, payload, selection, {
    payload["cpus"] = json!(vec![payload["cpus"][0].clone(); 33]);
});

pub(super) fn fixture() -> (Value, Value) {
    let previous = observation(1, 1000 * NS);
    let selected = observation(2, 1001 * NS);
    let payload = json!({"observation":selected,"context_seconds":900,
        "first_ns":102 * NS,"end_ns":1002 * NS,"offset":0,"total":1,
        "retention_limited":false,"observations":[
            {"observation":previous,"previous_observation":null,
                "interval_available":false,"cpu_count":1},
            {"observation":selected,"previous_observation":previous,
                "interval_available":true,"cpu_count":1}],
        "cpus":[{"cpu":0,"busy_percent":[null,0.0]}]});
    let selection = json!({"second":1001,"context_seconds":900,"selected":selected,
        "trends":[previous,selected]});

    (payload, selection)
}

fn observation(publication: u64, wall_ns: u64) -> Value {
    json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004",
        "publication":publication,"run_tick":publication,"wall_ns":wall_ns,"segment":0})
}
