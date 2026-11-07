use super::*;
use crate::cpu_history_sample::counters_rows;
use crate::recording::CpuRow;
use collector::machine::CPUTicks;

#[test]
fn derives_each_logical_cpu_activity_from_its_own_counter_interval() {
    // Goal: logical CPUs retain independent activity, including idle and fully busy.
    // Method: derive a compatible pair with exact counter deltas.
    let previous = sample(1, vec![[0, 0, 0, 0], [0, 0, 0, 0]]);
    let current = sample(2, vec![[0, 0, 0, 100], [25, 25, 50, 0]]);

    let result = project(&current, Some(&previous)).unwrap();

    assert_eq!(
        result["cpus"],
        json!([
            {"cpu": 0, "busy_percent": 0.0}, {"cpu": 1, "busy_percent": 100.0}
        ])
    );
    assert_eq!(result["interval_available"], true);
}

fn sample(publication: u64, counters: Vec<[u64; 4]>) -> Sample {
    Sample {
        identity: json!({"run":"run", "boot":"boot", "publication":publication,
                "run_tick":publication,"wall_ns":publication*1_000_000_000}),
        observation: Observation {
            window: 0,
            publication,
            run_tick: publication,
            wall: publication * 1_000_000_000,
            continuous: Some(publication * 1_000_000_000),
        },
        cpus: counters
            .into_iter()
            .map(|values| values.map(Some))
            .collect(),
    }
}

/// `count` CPU rows of one publication, each its index as its user ticks.
fn cpu_rows(publication: u64, count: u32) -> Vec<CpuRow> {
    (0..count)
        .map(|cpu| CpuRow {
            publication,
            cpu,
            ticks: CPUTicks {
                user: cpu,
                system: 0,
                nice: 0,
                idle: 0,
            },
        })
        .collect()
}

macro_rules! unavailable_case {
    ($name:ident, $change:expr) => {
        #[test]
        fn $name() {
            // Goal: incompatible observations never produce plausible logical CPU activity.
            // Method: change one identity or clock condition in a compatible pair.
            let previous = sample(1, vec![[0, 0, 0, 0]]);
            let mut current = sample(2, vec![[1, 0, 0, 1]]);
            $change(&mut current);

            let result = project(&current, Some(&previous)).unwrap();

            assert_eq!(result["interval_available"], false);
            assert_eq!(result["cpus"][0]["busy_percent"], Value::Null);
            assert!(result["unavailable_reason"].is_string());
        }
    };
}

unavailable_case!(gaps_when_the_run_changes, |current: &mut Sample| {
    current.identity["run"] = json!("other");
});
unavailable_case!(gaps_when_the_boot_changes, |current: &mut Sample| {
    current.identity["boot"] = json!("other");
});
unavailable_case!(
    gaps_when_a_publication_is_missing,
    |current: &mut Sample| {
        current.observation.publication += 1;
    }
);
unavailable_case!(gaps_when_a_run_tick_is_missing, |current: &mut Sample| {
    current.observation.run_tick += 1;
});
unavailable_case!(gaps_when_the_wall_clock_repeats, |current: &mut Sample| {
    current.observation.wall = 1_000_000_000;
});
unavailable_case!(
    gaps_when_the_continuous_clock_is_missing,
    |current: &mut Sample| {
        current.observation.continuous = None;
    }
);

#[test]
fn retains_independent_unavailable_and_available_cpus() {
    // Goal: a reset, missing counter and zero denominator do not erase other CPU readings.
    // Method: compare four CPUs with one independently valid interval.
    let previous = sample(1, vec![[10, 0, 0, 0]; 4]);
    let mut current = sample(
        2,
        vec![
            [9, 0, 0, 10],
            [20, 0, 0, 10],
            [10, 0, 0, 0],
            [10, 0, 10, 30],
        ],
    );
    current.cpus[1][0] = None;

    let result = project(&current, Some(&previous)).unwrap();

    assert_eq!(
        result["cpus"],
        json!([
            {"cpu":0,"busy_percent":null}, {"cpu":1,"busy_percent":null},
            {"cpu":2,"busy_percent":null}, {"cpu":3,"busy_percent":25.0}
        ])
    );
    assert_eq!(result["interval_available"], true);
}

#[test]
fn gaps_every_cpu_when_the_complete_domain_changes() {
    // Goal: a changed topology never pairs a partial set of CPU identities.
    // Method: grow a complete logical domain between observations.
    let previous = sample(1, vec![[0, 0, 0, 0]]);
    let current = sample(2, vec![[25, 0, 0, 75]; 2]);

    let result = project(&current, Some(&previous)).unwrap();

    assert_eq!(result["interval_available"], false);
    assert_eq!(result["unavailable_reason"], "Logical CPU domain changed");
    assert_eq!(
        result["cpus"],
        json!([
            {"cpu":0,"busy_percent":null}, {"cpu":1,"busy_percent":null}
        ])
    );
}

macro_rules! invalid_domain_case {
    ($name:ident, $cpu:literal, $reason:literal) => {
        #[test]
        fn $name() {
            // Goal: malformed CPU identity domains are refused where their rows are read.
            // Method: corrupt one complete domain and compare the typed boundary error.
            let mut rows = cpu_rows(2, 2);
            rows[1].cpu = $cpu;
            let mut counters = Vec::new();

            let result = counters_rows(&rows, &mut counters);

            assert!(matches!(result, Err(Error::Bound($reason))), "{result:?}");
        }
    };
}
invalid_domain_case!(
    rejects_duplicate_cpu_indices,
    0,
    "incomplete logical CPU domain"
);
invalid_domain_case!(
    rejects_incomplete_cpu_domains,
    2,
    "incomplete logical CPU domain"
);

#[test]
fn keeps_the_maximum_cpu_domain_inside_the_projection_byte_bound() {
    // Goal: the maximum admitted machine fits its separate CPU response budget.
    // Method: derive and serialize all 1024 independently indexed logical CPUs.
    let previous = sample(1, vec![[0, 0, 0, 0]; 1024]);
    let current = sample(2, vec![[1, 0, 0, 2]; 1024]);

    let result = project(&current, Some(&previous)).unwrap();

    assert_eq!(result["cpus"].as_array().unwrap().len(), 1024);
    assert!(serde_json::to_vec(&result).unwrap().len() <= RESPONSE_BYTES_MAX);
}

#[test]
fn rejects_cpu_counts_above_the_projection_bound() {
    // Goal: a source cannot bypass the fixed logical CPU admission limit.
    // Method: supply one more independently indexed row than the bound.
    let rows = cpu_rows(1, 1025);
    let mut counters = Vec::new();

    let result = counters_rows(&rows, &mut counters);

    assert!(matches!(result, Err(Error::Bound("logical CPU count"))));
}

#[test]
fn preserves_exact_observations_across_repeated_wall_seconds() {
    // Goal: a repeated second with increasing nanoseconds remains a genuine interval.
    // Method: keep one wall second while advancing exact wall and continuous clocks.
    let previous = sample(1, vec![[0, 0, 0, 0]]);
    let mut current = sample(2, vec![[1, 0, 0, 1]]);
    current.observation.wall = 1_500_000_000;
    current.identity["wall_ns"] = json!(1_500_000_000_u64);

    let result = project(&current, Some(&previous)).unwrap();

    assert_eq!(result["observation"], current.identity);
    assert_eq!(result["previous_observation"], previous.identity);
    assert_eq!(result["cpus"][0]["busy_percent"], 50.0);
}

#[test]
fn measures_bounded_cpu_projection_and_serialization_work() {
    // Goal: keep the actual 18-CPU and maximum projection costs reviewable.
    // Method: retain individual elapsed samples after constructing fixed counter inputs.
    measure_projection(18);
    measure_projection(CPUS_MAX);
}

fn measure_projection(count: usize) {
    let previous = sample(1, vec![[0, 0, 0, 0]; count]);
    let current = sample(2, vec![[1, 0, 0, 2]; count]);
    for index in 0..40 {
        let started = std::time::Instant::now();
        let value = std::hint::black_box(project(&current, Some(&previous)).unwrap());
        let projection_ns = started.elapsed().as_nanos();
        let started = std::time::Instant::now();
        let bytes = std::hint::black_box(serde_json::to_vec(&value).unwrap());
        let serialization_ns = started.elapsed().as_nanos();
        println!(
            "{}",
            json!({"logical_cpus":count,"sample":index,
            "projection_including_bound_serialization_ns":projection_ns,
            "serialization_ns":serialization_ns,"bytes":bytes.len()})
        );
        assert!(bytes.len() <= RESPONSE_BYTES_MAX);
    }
}

#[test]
fn exposes_missing_predecessors_without_inventing_idle_cpus() {
    // Goal: the first selected CPU reading remains distinct from measured zero activity.
    // Method: project counters without any predecessor.
    let current = sample(1, vec![[0, 0, 0, 100]]);

    let result = project(&current, None).unwrap();

    assert_eq!(
        result,
        json!({"observation":current.identity,"previous_observation":null,
        "interval_available":false,"unavailable_reason":"Previous CPU reading unavailable",
        "cpus":[{"cpu":0,"busy_percent":null}]})
    );
}

#[test]
fn exposes_unavailable_cpu_domains_as_an_empty_projection() {
    // Goal: an absent source never appears as one zero-activity logical CPU.
    // Method: project two compatible observations containing no CPU rows.
    let previous = sample(1, vec![]);
    let current = sample(2, vec![]);

    let result = project(&current, Some(&previous)).unwrap();

    assert_eq!(result["cpus"], json!([]));
    assert_eq!(result["interval_available"], false);
    assert_eq!(result["unavailable_reason"], "CPU readings unavailable");
}

#[test]
fn refuses_a_counter_sum_that_cannot_be_represented() {
    // Goal: malformed counters never overflow or wrap into plausible activity.
    // Method: supply independently valid integers whose delta sum exceeds u64.
    let previous = sample(1, vec![[0, 0, 0, 0]]);
    let current = sample(2, vec![[u64::MAX, 1, 0, 0]]);

    let result = project(&current, Some(&previous));

    assert!(matches!(result, Err(Error::Bound("CPU counter total"))));
}
