use super::tests::{interval, observation, program, usage};
use super::*;

#[test]
fn sums_compatible_cpu_energy_and_live_memory_with_independent_literal_expectations() {
    // Goal: one App combines two live members using the actual elapsed denominator.
    // Method: give members 200ms/300ms CPU, 1J/2J energy and 100MiB/50MiB resident deltas.
    let interval = interval();
    let program = program(b"/Applications/App A.app");
    let before_usage = usage(10_000_000_000, 90_000_000_000, 1);
    let first_usage = usage(10_200_000_000, 91_000_000_000, 104_857_600);
    let second_usage = usage(10_300_000_000, 92_000_000_000, 52_428_800);
    let before = observation(&interval.previous, &program, &before_usage);
    let first = observation(&interval.selected, &program, &first_usage);
    let mut second_before = observation(&interval.previous, &program, &before_usage);
    second_before.pid = 8;
    second_before.creation.as_mut().unwrap().unique_id = 800;
    let mut second = observation(&interval.selected, &program, &second_usage);
    second.pid = 8;
    second.creation.as_mut().unwrap().unique_id = 800;
    let input = [
        ProcessInterval {
            previous: Some(&before),
            selected: &first,
            start: None,
        },
        ProcessInterval {
            previous: Some(&second_before),
            selected: &second,
            start: None,
        },
    ];
    let mut result = tests::aggregate();

    result.compute(&interval, &input).unwrap();

    let app = &result.apps[&AppKey::Bundle(b"/Applications/App A.app")];
    assert_eq!(app.cpu_ns.known, 500_000_000);
    assert_eq!(
        cpu_percent_one_core(CpuRate {
            cpu_ns: app.cpu_ns.known,
            elapsed_ns: result.elapsed_ns.unwrap()
        }),
        Some(50.0)
    );
    assert!((energy_joules(app.energy_nj.known) - 3.0).abs() < f64::EPSILON);
    assert_eq!(
        energy_watts(EnergyRate {
            energy_nj: app.energy_nj.known,
            elapsed_ns: result.elapsed_ns.unwrap()
        }),
        Some(3.0)
    );
    assert_eq!(app.resident_bytes.known, 157_286_400);
    assert_eq!(app.footprint_bytes.known, 157_286_402);
}

#[test]
fn retains_known_subtotals_when_a_member_refuses_usage() {
    // Goal: a refused member never turns into a measured zero or hides readable members.
    // Method: aggregate one measured member and one identity-only refused member.
    let interval = interval();
    let program = program(b"/Applications/A.app");
    let before_usage = usage(0, 0, 1);
    let now_usage = usage(200, 300, 400);
    let before = observation(&interval.previous, &program, &before_usage);
    let selected = observation(&interval.selected, &program, &now_usage);
    let mut refused = observation(&interval.selected, &program, &now_usage);
    refused.pid = 8;
    refused.creation.as_mut().unwrap().unique_id = 800;
    refused.usage = Err(Unavailable::Refused);
    let input = [
        ProcessInterval {
            previous: Some(&before),
            selected: &selected,
            start: None,
        },
        ProcessInterval {
            previous: None,
            selected: &refused,
            start: None,
        },
    ];
    let mut result = tests::aggregate();

    result.compute(&interval, &input).unwrap();

    let app = &result.apps[&AppKey::Bundle(b"/Applications/A.app")];
    assert_eq!(app.cpu_ns.known, 200);
    assert_eq!(app.cpu_ns.measured_members, 1);
    assert_eq!(app.cpu_ns.unavailable_members, 1);
    assert_eq!(app.resident_bytes.known, 400);
    assert_eq!(app.resident_bytes.unavailable_members, 1);
    assert_eq!(
        result.processes[1].cpu_ns,
        Amount::Unavailable(Unavailable::Refused)
    );
}

#[test]
fn keeps_exited_resident_beside_process_and_excludes_it_from_present_app_memory() {
    // Goal: verified final usage contributes rates but no currently present resident memory.
    // Method: compare a prior observation to a verified exit carrying a last resident gauge.
    let interval = interval();
    let program = program(b"/Applications/A.app");
    let before_usage = usage(100, 200, 300);
    let now_usage = usage(400, 600, 800);
    let before = observation(&interval.previous, &program, &before_usage);
    let mut selected = observation(&interval.selected, &program, &now_usage);
    selected.state = ProcessState::ExitedVerified;
    let mut result = tests::aggregate();

    result
        .compute(
            &interval,
            &[ProcessInterval {
                previous: Some(&before),
                selected: &selected,
                start: None,
            }],
        )
        .unwrap();

    let app = &result.apps[&AppKey::Bundle(b"/Applications/A.app")];
    assert_eq!(app.cpu_ns.known, 300);
    assert_eq!(app.energy_nj.known, 400);
    assert_eq!(app.resident_bytes.known, 0);
    assert_eq!(app.resident_bytes.measured_members, 0);
    assert_eq!(app.live_members, 0);
    assert_eq!(result.processes[0].resident_bytes_last, Some(800));
}

macro_rules! unavailable_interval {
    ($name:ident, $change:expr) => {
        #[test]
        fn $name() {
            // Goal: incompatible interval facts never produce cross-boundary deltas.
            // Method: change one fact while keeping counter increases readable.
            let mut interval = interval();
            $change(&mut interval);
            let program = program(b"/Applications/A.app");
            let before_usage = usage(1, 1, 1);
            let now_usage = usage(5, 5, 5);
            let before = observation(&interval.previous, &program, &before_usage);
            let selected = observation(&interval.selected, &program, &now_usage);
            let mut result = tests::aggregate();

            result
                .compute(
                    &interval,
                    &[ProcessInterval {
                        previous: Some(&before),
                        selected: &selected,
                        start: None,
                    }],
                )
                .unwrap();

            assert_eq!(
                result.processes[0].cpu_ns,
                Amount::Unavailable(Unavailable::Interval)
            );
            assert_eq!(
                result.processes[0].energy_nj,
                Amount::Unavailable(Unavailable::Interval)
            );
        }
    };
}
unavailable_interval!(rejects_deltas_across_boots, |value: &mut Interval| value
    .selected
    .boot =
    [9, 9]);
unavailable_interval!(rejects_deltas_across_runs, |value: &mut Interval| value
    .selected
    .run =
    [9, 9]);
unavailable_interval!(rejects_publication_gaps, |value: &mut Interval| value
    .selected
    .publication =
    4);
unavailable_interval!(rejects_missing_continuous_time, |value: &mut Interval| {
    value.selected.continuous_ns = None;
});
unavailable_interval!(rejects_nonpositive_elapsed_time, |value: &mut Interval| {
    value.selected.continuous_ns = value.previous.continuous_ns;
});
unavailable_interval!(
    rejects_incompletely_observed_intervals,
    |value: &mut Interval| {
        value.observations_complete = false;
    }
);

macro_rules! process_boundary {
    ($name:ident, $change:expr, $cpu:expr, $energy:expr, $attribution:expr) => {
        #[test]
        fn $name() {
            // Goal: the selected process boundary preserves exactly the supported metrics.
            // Method: change one identity/counter fact in otherwise compatible observations.
            let interval = interval();
            let program = program(b"/Applications/A.app");
            let before_usage = usage(100, 200, 300);
            let mut now_usage = usage(400, 600, 800);
            $change.0(&mut now_usage);
            let before = observation(&interval.previous, &program, &before_usage);
            let mut selected = observation(&interval.selected, &program, &now_usage);
            $change.1(&mut selected);
            let mut result = tests::aggregate();

            result
                .compute(
                    &interval,
                    &[ProcessInterval {
                        previous: Some(&before),
                        selected: &selected,
                        start: None,
                    }],
                )
                .unwrap();

            assert_eq!(result.processes[0].cpu_ns, $cpu);
            assert_eq!(result.processes[0].energy_nj, $energy);
            assert_eq!(result.processes[0].app_attribution, $attribution);
        }
    };
}
process_boundary!(
    rejects_pid_reuse,
    (
        (|_: &mut Usage| {}),
        (|value: &mut ProcessObservation| {
            value.creation.as_mut().unwrap().unique_id = 701;
        })
    ),
    Amount::Unavailable(Unavailable::Creation),
    Amount::Unavailable(Unavailable::Creation),
    None
);
process_boundary!(
    rejects_pid_only_join,
    (
        (|_: &mut Usage| {}),
        (|value: &mut ProcessObservation| value.pid = 8)
    ),
    Amount::Unavailable(Unavailable::Creation),
    Amount::Unavailable(Unavailable::Creation),
    None
);
process_boundary!(
    rejects_changed_awake_creation,
    (
        (|value: &mut Usage| value.awake_ns_at_start = 20),
        (|_: &mut ProcessObservation| {})
    ),
    Amount::Unavailable(Unavailable::Creation),
    Amount::Unavailable(Unavailable::Creation),
    None
);
process_boundary!(
    keeps_energy_when_cpu_resets,
    (
        (|value: &mut Usage| value.user_ns = 50),
        (|_: &mut ProcessObservation| {})
    ),
    Amount::Unavailable(Unavailable::CounterReset),
    Amount::Known(400),
    None
);
process_boundary!(
    keeps_cpu_when_energy_resets,
    (
        (|value: &mut Usage| value.energy_nj = 50),
        (|_: &mut ProcessObservation| {})
    ),
    Amount::Known(300),
    Amount::Unavailable(Unavailable::CounterReset),
    None
);
process_boundary!(
    keeps_process_rates_but_excludes_same_path_exec_from_app_rates,
    (
        (|_: &mut Usage| {}),
        (|value: &mut ProcessObservation| {
            value.creation.as_mut().unwrap().pid_version = 2;
        })
    ),
    Amount::Known(300),
    Amount::Known(400),
    Some(Unavailable::Exec)
);
process_boundary!(
    refuses_unverified_exit_deltas,
    (
        (|_: &mut Usage| {}),
        (|value: &mut ProcessObservation| {
            value.state = ProcessState::ExitedUnverified;
        })
    ),
    Amount::Unavailable(Unavailable::ExitUnverified),
    Amount::Unavailable(Unavailable::ExitUnverified),
    None
);
process_boundary!(
    preserves_unknown_usage,
    (
        (|_: &mut Usage| {}),
        (|value: &mut ProcessObservation| {
            value.usage = Err(Unavailable::Unknown);
        })
    ),
    Amount::Unavailable(Unavailable::Unknown),
    Amount::Unavailable(Unavailable::Unknown),
    None
);

#[test]
fn first_seen_existing_process_has_gauges_without_an_invented_rate_baseline() {
    // Goal: first sight of an existing process is not a measured zero baseline.
    // Method: provide one readable endpoint without a verified Start or predecessor.
    let interval = interval();
    let program = program(b"/Applications/A.app");
    let usage = usage(400, 600, 800);
    let selected = observation(&interval.selected, &program, &usage);
    let mut result = tests::aggregate();

    result
        .compute(
            &interval,
            &[ProcessInterval {
                previous: None,
                selected: &selected,
                start: None,
            }],
        )
        .unwrap();

    assert_eq!(
        result.processes[0].cpu_ns,
        Amount::Unavailable(Unavailable::Baseline)
    );
    assert_eq!(result.processes[0].resident_bytes_last, Some(800));
}

#[test]
fn reports_lost_events_as_uncertain_membership_without_erasing_known_rates() {
    // Goal: lost event ranges prevent complete membership claims while retaining known deltas.
    // Method: supply compatible existing members under incomplete event coverage.
    let mut interval = interval();
    interval.events_complete = false;
    let program = program(b"/Applications/A.app");
    let before_usage = usage(100, 200, 300);
    let now_usage = usage(400, 600, 800);
    let before = observation(&interval.previous, &program, &before_usage);
    let selected = observation(&interval.selected, &program, &now_usage);
    let mut result = tests::aggregate();

    result
        .compute(
            &interval,
            &[ProcessInterval {
                previous: Some(&before),
                selected: &selected,
                start: None,
            }],
        )
        .unwrap();

    assert_eq!(result.processes[0].cpu_ns, Amount::Known(300));
    assert!(!result.membership_complete);
    assert!(!result.apps[&AppKey::Bundle(b"/Applications/A.app")].membership_complete);
}

#[test]
fn groups_original_bytes_without_merging_invalid_utf8_and_literal_backslash_names() {
    // Goal: App keys preserve byte identities independently of escaped display text.
    // Method: give four independent raw basenames, two of them identical.
    let interval = interval();
    let programs = [
        ProgramBytes {
            executable_name: b"claude",
            app_path: None,
        },
        ProgramBytes {
            executable_name: b"claude",
            app_path: None,
        },
        ProgramBytes {
            executable_name: b"bad\xff",
            app_path: None,
        },
        ProgramBytes {
            executable_name: b"bad\\xff",
            app_path: None,
        },
    ];
    let usage = usage(0, 0, 1);
    let observations: Vec<_> = programs
        .iter()
        .enumerate()
        .map(|(index, program)| {
            let mut value = observation(&interval.selected, program, &usage);
            value.pid = i32::try_from(index + 1).unwrap();
            value
        })
        .collect();
    let spans: Vec<_> = observations
        .iter()
        .map(|selected| ProcessInterval {
            previous: None,
            selected,
            start: None,
        })
        .collect();
    let mut result = tests::aggregate();

    result.compute(&interval, &spans).unwrap();

    assert_eq!(result.apps.len(), 3);
    assert_eq!(result.apps[&AppKey::Executable(b"claude")].members, 2);
    assert_eq!(result.apps[&AppKey::Executable(b"bad\xff")].members, 1);
    assert_eq!(result.apps[&AppKey::Executable(b"bad\\xff")].members, 1);
}

#[test]
fn observed_start_of_existing_creation_does_not_supply_zero_baseline() {
    // Goal: a notice seen this interval cannot turn an old creation into a new process.
    // Method: supply a verified identity and current notice, but old source birth time.
    let interval = interval();
    let program = program(b"/Applications/A.app");
    let usage = usage(400, 600, 800);
    let selected = observation(&interval.selected, &program, &usage);
    let start = VerifiedStart {
        stamp: Stamp {
            continuous_ns: Some(1_500_000_000),
            ..interval.selected
        },
        pid: 7,
        creation: selected.creation.unwrap(),
        program: Ok(&program),
    };
    let mut result = tests::aggregate();

    result
        .compute(
            &interval,
            &[ProcessInterval {
                previous: None,
                selected: &selected,
                start: Some(&start),
            }],
        )
        .unwrap();

    assert_eq!(
        result.processes[0].cpu_ns,
        Amount::Unavailable(Unavailable::Baseline)
    );
}

#[test]
fn lost_event_coverage_prevents_zero_baseline_for_a_new_creation() {
    // Goal: source birth inside the interval is insufficient when event coverage was lost.
    // Method: supply a matching verified Start and actual birth under incomplete event coverage.
    let mut interval = interval();
    interval.events_complete = false;
    let program = program(b"/Applications/A.app");
    let mut usage = usage(400, 600, 800);
    usage.awake_ns_at_start = 1_500_000_000;
    let selected = observation(&interval.selected, &program, &usage);
    let start = VerifiedStart {
        stamp: interval.selected,
        pid: selected.pid,
        creation: selected.creation.unwrap(),
        program: Ok(&program),
    };
    let mut result = tests::aggregate();

    result
        .compute(
            &interval,
            &[ProcessInterval {
                previous: None,
                selected: &selected,
                start: Some(&start),
            }],
        )
        .unwrap();

    assert_eq!(
        result.processes[0].cpu_ns,
        Amount::Unavailable(Unavailable::Baseline)
    );
    assert!(!result.membership_complete);
}

#[test]
fn retains_process_rates_across_a_changed_program_without_inventing_app_split() {
    // Goal: captured program changes prohibit App attribution even if a version change is absent.
    // Method: compare matching creation/usage counters with two different captured App paths.
    let interval = interval();
    let before_program = program(b"/Applications/Before.app");
    let selected_program = program(b"/Applications/After.app");
    let before_usage = usage(100, 200, 300);
    let selected_usage = usage(400, 600, 800);
    let before = observation(&interval.previous, &before_program, &before_usage);
    let selected = observation(&interval.selected, &selected_program, &selected_usage);
    let mut result = tests::aggregate();

    result
        .compute(
            &interval,
            &[ProcessInterval {
                previous: Some(&before),
                selected: &selected,
                start: None,
            }],
        )
        .unwrap();

    assert_eq!(result.processes[0].cpu_ns, Amount::Known(300));
    assert_eq!(result.processes[0].app_attribution, Some(Unavailable::Exec));
    assert_eq!(
        result.apps[&AppKey::Bundle(b"/Applications/After.app")]
            .cpu_ns
            .known,
        0
    );
}

#[test]
fn retains_previous_resident_when_final_exit_usage_is_unavailable() {
    // Goal: an unavailable final usage does not discard the historical process's last gauge.
    // Method: mark a measured predecessor exited with Gone usage at the selected endpoint.
    let interval = interval();
    let program = program(b"/Applications/A.app");
    let usage = usage(100, 200, 300);
    let previous = observation(&interval.previous, &program, &usage);
    let mut selected = observation(&interval.selected, &program, &usage);
    selected.state = ProcessState::ExitedUnverified;
    selected.usage = Err(Unavailable::Gone);
    let mut result = tests::aggregate();

    result
        .compute(
            &interval,
            &[ProcessInterval {
                previous: Some(&previous),
                selected: &selected,
                start: None,
            }],
        )
        .unwrap();

    assert_eq!(result.processes[0].resident_bytes_last, Some(300));
    assert_eq!(result.processes[0].footprint_bytes_last, Some(301));
    assert_eq!(
        result.processes[0].cpu_ns,
        Amount::Unavailable(Unavailable::ExitUnverified)
    );
    assert_eq!(
        result.apps[&AppKey::Bundle(b"/Applications/A.app")]
            .resident_bytes
            .known,
        0
    );
}
