use super::*;
use crate::cpu_history_sample::Counters;

#[derive(Clone, Copy)]
struct IdentitySpec {
    publication: u64,
    wall: u64,
}

#[test]
fn pages_a_maximum_domain_and_maximum_observation_count_inside_the_byte_bound() {
    // Goal: a bounded32-row page covers the pure1024-CPU/4096-observation protocol maximum.
    // Method: project maximum-width identities and long fractional values on the final full page.
    let mut context = Vec::with_capacity(4096);
    let mut entries = Vec::with_capacity(4096);
    for index in 0..4096 {
        let publication = u64::MAX - 4095 + index;
        let current = identity(IdentitySpec {
            publication,
            wall: 1_001_000_000_000 + index * 100_000,
        });
        let previous = identity(IdentitySpec {
            publication: publication - 1,
            wall: 1_001_000_000_000 + index * 100_000 - 100_000,
        });
        context.push(current.clone());
        entries.push(Entry {
            identity: current,
            previous: Some(previous),
            interval_available: true,
            values: vec![Some(33.333_333_333_333_336); 1024],
        });
    }
    let selected = context.last().unwrap().json();
    let cache = Cache {
        range: range(&selected).unwrap(),
        entries,
    };
    assert!(cache.bytes() <= CACHE_BYTES_MAX);

    let response = project(&Request {
        selected: &selected,
        context: &context,
        live: None,
        saved: Some(&cache),
        offset: 992,
    })
    .unwrap();

    assert_eq!(response["offset"], 992);
    assert_eq!(response["total"], 1024);
    assert_eq!(response["cpus"].as_array().unwrap().len(), 32);
    assert_eq!(response["cpus"][31]["cpu"], 1023);
    assert_eq!(response["observations"].as_array().unwrap().len(), 4096);
    assert_eq!(
        response["cpus"][0]["busy_percent"]
            .as_array()
            .unwrap()
            .len(),
        4096
    );
    let bytes = serde_json::to_vec(&response).unwrap().len();
    let envelope = serde_json::to_vec(&json!({"value":response,"error":null}))
        .unwrap()
        .len();
    println!(
        "{}",
        json!({"phase":"maximum_cpu_history","logical_cpus":1024,
        "observations":4096,"page_cpus":32,"payload_bytes":bytes,"envelope_bytes":envelope,
        "cache_accounted_bytes":cache.bytes(),"entry_size":size_of::<Entry>(),
        "identity_size":size_of::<Identity>(),"optional_percent_size":size_of::<Option<f64>>() })
    );
    assert!(bytes <= 7 * 1024 * 1024);
    assert!(envelope <= 8 * 1024 * 1024);
}

fn identity(spec: IdentitySpec) -> Identity {
    Identity {
        run: "a".repeat(64),
        boot: "b".repeat(64),
        observation: Observation {
            window: 0,
            publication: spec.publication,
            run_tick: spec.publication,
            wall: spec.wall,
            continuous: Some(spec.publication),
        },
    }
}

fn entry(publication: u64, values: Vec<Option<f64>>) -> Entry {
    let interval_available = !values.is_empty();
    Entry {
        identity: identity(IdentitySpec {
            publication,
            wall: (1000 + publication) * NS,
        }),
        previous: Some(identity(IdentitySpec {
            publication: publication - 1,
            wall: (999 + publication) * NS,
        })),
        interval_available,
        values,
    }
}

fn response(entries: Vec<Entry>, offset: u32) -> Result<Value> {
    let context = entries
        .iter()
        .map(|entry| entry.identity.clone())
        .collect::<Vec<_>>();
    let selected = context.last().unwrap().json();
    let cache = Cache {
        range: range(&selected)?,
        entries,
    };

    project(&Request {
        selected: &selected,
        context: &context,
        live: None,
        saved: Some(&cache),
        offset,
    })
}

#[test]
fn returns_a_partial_last_page_without_changing_selection() {
    // Goal: paging exposes every logical CPU exactly once with stable selected identity.
    // Method: request35 CPUs on pages0 and32 from the same independent values.
    let entries = vec![entry(2, (0..35).map(|cpu| Some(f64::from(cpu))).collect())];

    let first = response(entries.clone(), 0).unwrap();
    let last = response(entries, 32).unwrap();

    assert_eq!(first["observation"], last["observation"]);
    assert_eq!(first["cpus"].as_array().unwrap().len(), 32);
    assert_eq!(
        last["cpus"],
        json!([
        {"cpu":32,"busy_percent":[32.0]}, {"cpu":33,"busy_percent":[33.0]},
        {"cpu":34,"busy_percent":[34.0]}])
    );
}

macro_rules! offset_case {
    ($name:ident,$count:expr,$offset:expr,$reason:literal) => {
        #[test]
        fn $name() {
            // Goal: invalid row pages cannot drift to a different logical CPU domain.
            // Method: submit the stated offset against a complete independently bounded domain.
            let result = response(vec![entry(2, vec![Some(0.0); $count])], $offset);

            assert!(matches!(result, Err(Error::Bound($reason))));
        }
    };
}
offset_case!(rejects_non_aligned_offsets, 35, 1, "CPU history offset");
offset_case!(
    rejects_offsets_equal_to_the_domain,
    32,
    32,
    "CPU history page past end"
);
offset_case!(
    rejects_offsets_above_the_domain,
    35,
    64,
    "CPU history page past end"
);
offset_case!(
    rejects_a_nonzero_offset_for_an_empty_domain,
    0,
    32,
    "CPU history page past end"
);

#[test]
fn exposes_an_empty_cpu_domain_without_inventing_a_row() {
    // Goal: unavailable CPU acquisition remains distinguishable from measured idle.
    // Method: project a complete empty domain on page0.
    let response = response(vec![entry(2, vec![])], 0).unwrap();

    assert_eq!(response["total"], 0);
    assert_eq!(response["cpus"], json!([]));
    assert_eq!(response["observations"][0]["interval_available"], false);
}

#[test]
fn rejects_a_domain_above_1024_before_selecting_a_page() {
    // Goal: paging cannot conceal a source beyond the pure presentation bound.
    // Method: supply1025 genuine independently indexed values and request its first page.
    let result = response(vec![entry(2, vec![Some(0.0); 1025])], 0);

    assert!(matches!(result, Err(Error::Bound("logical CPU count"))));
}

#[test]
fn rejects_more_than_4096_context_observations() {
    // Goal: repeated wall seconds cannot bypass the global observation count bound.
    // Method: supply4097 context identities while keeping the source empty.
    let selected = identity(IdentitySpec {
        publication: 2,
        wall: 1002 * NS,
    })
    .json();
    let context = vec![
        identity(IdentitySpec {
            publication: 2,
            wall: 1002 * NS
        });
        4097
    ];

    let result = project(&Request {
        selected: &selected,
        context: &context,
        live: None,
        saved: None,
        offset: 0,
    });

    assert!(matches!(
        result,
        Err(Error::Bound("CPU history observations"))
    ));
}

#[test]
fn retains_measured_zero_and_independent_counter_reset_gaps() {
    // Goal: one CPU reset does not erase measured idle or a different CPU's activity.
    // Method: derive a valid3-CPU pair with a reset, measured zero, and25 percent activity.
    let previous = sample(1, vec![[Some(10), Some(0), Some(0), Some(0)]; 3]);
    let current = sample(
        2,
        vec![
            [Some(9), Some(0), Some(0), Some(100)],
            [Some(10), Some(0), Some(0), Some(100)],
            [Some(35), Some(0), Some(0), Some(75)],
        ],
    );
    let mut result = None;
    Entry::derive(
        &Interval {
            current: &current,
            previous: Some(&previous),
        },
        &mut result,
    )
    .unwrap();

    let response = response(vec![result.unwrap()], 0).unwrap();

    assert_eq!(response["observations"][0]["interval_available"], true);
    assert_eq!(
        response["cpus"],
        json!([{ "cpu":0,"busy_percent":[null]},
        {"cpu":1,"busy_percent":[0.0]},{"cpu":2,"busy_percent":[25.0]}])
    );
}

fn sample(publication: u64, counters: Counters) -> CounterSample {
    CounterSample {
        identity: identity(IdentitySpec {
            publication,
            wall: (1000 + publication) * NS,
        }),
        counters,
    }
}

macro_rules! discontinuity_case {
    ($name:ident,$change:expr) => {
        #[test]
        fn $name() {
            // Goal: incompatible CPU intervals leave every rendered cell unmeasured.
            // Method: change one captured interval condition before deriving the public matrix.
            let previous = sample(1, vec![[Some(0); 4]; 2]);
            let mut current = sample(2, vec![[Some(100); 4]; 2]);
            $change(&mut current);
            let mut derived = None;
            Entry::derive(
                &Interval {
                    current: &current,
                    previous: Some(&previous),
                },
                &mut derived,
            )
            .unwrap();

            let response = response(vec![derived.unwrap()], 0).unwrap();

            assert_eq!(response["observations"][0]["interval_available"], false);
            for cpu in response["cpus"].as_array().unwrap() {
                assert_eq!(cpu["busy_percent"], json!([null]));
            }
        }
    };
}
discontinuity_case!(
    gaps_a_whole_domain_transition,
    |sample: &mut CounterSample| {
        sample.counters.pop().unwrap();
    }
);
discontinuity_case!(gaps_a_missing_publication, |sample: &mut CounterSample| {
    sample.identity.observation.publication = 3;
});
discontinuity_case!(gaps_a_missing_run_tick, |sample: &mut CounterSample| {
    sample.identity.observation.run_tick = 3;
});
discontinuity_case!(gaps_a_clock_rollback, |sample: &mut CounterSample| {
    sample.identity.observation.wall = 1000 * NS;
});
discontinuity_case!(gaps_a_clock_jump, |sample: &mut CounterSample| {
    sample.identity.observation.wall = 1004 * NS;
});
discontinuity_case!(gaps_a_run_restart, |sample: &mut CounterSample| {
    sample.identity.run = "c".repeat(64);
});
discontinuity_case!(gaps_a_boot_change, |sample: &mut CounterSample| {
    sample.identity.boot = "c".repeat(64);
});

#[test]
fn retains_distinct_observations_inside_one_wall_second() {
    // Goal: repeated wall-second buckets preserve exact observation and predecessor timestamps.
    // Method: advance500 milliseconds within the same second and derive independent counters.
    let previous = sample(1, vec![[Some(0); 4]]);
    let mut current = sample(2, vec![[Some(1); 4]]);
    current.identity.observation.wall = 1001 * NS + 500_000_000;
    let mut derived = None;
    Entry::derive(
        &Interval {
            current: &current,
            previous: Some(&previous),
        },
        &mut derived,
    )
    .unwrap();

    let response = response(vec![derived.unwrap()], 0).unwrap();

    assert_eq!(response["observations"][0]["interval_available"], true);
    assert_eq!(
        response["observations"][0]["observation"]["wall_ns"],
        1001 * NS + 500_000_000
    );
    assert_eq!(
        response["observations"][0]["previous_observation"]["wall_ns"],
        1001 * NS
    );
    assert_eq!(response["cpus"][0]["busy_percent"], json!([75.0]));
}

macro_rules! complementary_case {
    ($name:ident,$live_valid:expr) => {
        #[test]
        fn $name() {
            // Goal: an incomplete duplicate cannot erase a valid independently derived interval.
            // Method: merge the same identity with a predecessor available in only one source.
            let mut valid = entry(2,vec![Some(25.0),Some(75.0)]);
            let mut missing = valid.clone();
            missing.interval_available=false;
            missing.previous=None;
            missing.values.fill(None);
            if !$live_valid {std::mem::swap(&mut valid,&mut missing);}
            let context = vec![valid.identity.clone()];
            let selected = context[0].json();
            let mut live = None;
            History::init(&mut live);
            live.as_mut().unwrap().entries.push_back(valid);
            let saved = Cache {range:range(&selected).unwrap(),entries:vec![missing]};

            let response = project(&Request {selected:&selected,context:&context,
                live:live.as_ref(),saved:Some(&saved),offset:0}).unwrap();

            assert_eq!(response["observations"][0]["interval_available"],true);
            assert_eq!(response["cpus"],json!([{ "cpu":0,"busy_percent":[25.0]},
                {"cpu":1,"busy_percent":[75.0]}]));
        }
    };
}
complementary_case!(
    retains_live_intervals_when_committed_predecessors_are_missing,
    true
);
complementary_case!(
    recovers_live_intervals_from_complete_committed_predecessors,
    false
);

#[test]
fn rejects_a_cpu_history_response_above_seven_mib() {
    // Goal: future observation metadata cannot bypass the separate payload byte budget.
    // Method: add oversized selected-observation metadata to an otherwise tiny valid matrix.
    let entry = entry(2, vec![Some(0.0)]);
    let context = vec![entry.identity.clone()];
    let mut selected = context[0].json();
    selected["window_losses"] = json!("x".repeat(7 * 1024 * 1024));
    let saved = Cache {
        range: range(&selected).unwrap(),
        entries: vec![entry],
    };

    let result = project(&Request {
        selected: &selected,
        context: &context,
        live: None,
        saved: Some(&saved),
        offset: 0,
    });

    assert!(matches!(
        result,
        Err(Error::Bound("CPU history response bytes"))
    ));
}

#[test]
fn reports_retention_limits_without_fabricating_dropped_cpu_values() {
    // Goal: reaching the count bound leaves truthful gaps in an older selected window.
    // Method: ingest4097 real typed CPU tables inside one second and inspect4096 prior identities.
    let mut history = None;
    History::init(&mut history);
    let history = history.as_mut().unwrap();
    let mut context = Vec::new();
    for publication in 1..=4097 {
        let identity = identity(IdentitySpec {
            publication,
            wall: 1001 * NS + publication * 100_000,
        });
        let table = Table {
            columns: [
                "publication",
                "cpu",
                "user_ticks",
                "system_ticks",
                "nice_ticks",
                "idle_ticks",
            ]
            .map(str::to_owned)
            .to_vec(),
            rows: vec![vec![
                Some(publication.to_string()),
                Some("0".to_owned()),
                Some(publication.to_string()),
                Some("0".to_owned()),
                Some("0".to_owned()),
                Some(publication.to_string()),
            ]],
        };
        history.capture(&identity, &table).unwrap();
        if publication <= 4096 {
            context.push(identity);
        }
    }
    let selected = context.last().unwrap().json();

    let response = project(&Request {
        selected: &selected,
        context: &context,
        live: Some(history),
        saved: None,
        offset: 0,
    })
    .unwrap();

    assert_eq!(response["retention_limited"], true);
    assert_eq!(response["observations"].as_array().unwrap().len(), 4096);
    assert_eq!(response["cpus"][0]["busy_percent"][0], Value::Null);
    assert_eq!(response["cpus"][0]["busy_percent"][1], 50.0);
    assert_eq!(response["cpus"][0]["busy_percent"][4095], 50.0);
    assert!(history.bytes() <= LIVE_BYTES_MAX);
    assert_eq!(history.entries.len(), 4096);
}
